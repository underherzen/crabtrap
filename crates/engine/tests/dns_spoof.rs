//! DNS Spoofing: хост из таблицы подключается по заданному IP, запрос —
//! с исходным Host; смена таблицы действует сразу, а не после истечения
//! keep-alive пула; обмены и туннели помечаются `dns-spoofed`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::proxy_config::EngineCommand;
use domain::entities::DnsOverride;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;
use common::*;

const SPOOFED_HOST: &str = "spoofed.crabtrap.test";

/// Keep-alive origin: отвечает `name` на каждый запрос, Host запросов —
/// в `hosts`.
async fn spawn_origin(listener: TcpListener, name: &'static str) -> Arc<Mutex<Vec<String>>> {
    let hosts = Arc::new(Mutex::new(Vec::new()));
    let seen = hosts.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 {
                        return;
                    }
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let host = head
                        .lines()
                        .find_map(|l| l.strip_prefix("host: ").or(l.strip_prefix("Host: ")))
                        .unwrap_or_default()
                        .to_string();
                    seen.lock().expect("hosts").push(host);
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{name}",
                        name.len()
                    );
                    if sock.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    hosts
}

async fn set_overrides(cmd_tx: &async_channel::Sender<EngineCommand>, ip: IpAddr) {
    cmd_tx
        .send(EngineCommand::SetDnsOverrides {
            entries: vec![DnsOverride::new("*.crabtrap.test", ip)],
        })
        .await
        .expect("send");
    tokio::time::sleep(Duration::from_millis(50)).await;
}

async fn get(engine: &str, port: u16) -> String {
    let mut client = TcpStream::connect(engine).await.expect("connect");
    let req = format!(
        "GET http://{SPOOFED_HOST}:{port}/x HTTP/1.1\r\nHost: {SPOOFED_HOST}:{port}\r\nConnection: close\r\n\r\n"
    );
    client.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    client.read_to_end(&mut raw).await.expect("read");
    String::from_utf8_lossy(&raw).to_string()
}

fn spoofed_tags(events: &Arc<RecordingPublisher>) -> usize {
    events
        .snapshot()
        .iter()
        .filter(|e| matches!(e, ProxyEvent::ExchangeTagged { tag, .. } if tag == "dns-spoofed"))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn spoofed_host_reaches_the_given_ip_with_its_own_host_header() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let hosts = spawn_origin(listener, "origin").await;
    let (engine, events, cmd_tx) = spawn_engine().await;
    set_overrides(&cmd_tx, Ipv4Addr::LOCALHOST.into()).await;

    let reply = get(&engine, port).await;

    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(reply.ends_with("origin"), "{reply}");
    assert_eq!(
        hosts.lock().expect("hosts").as_slice(),
        [format!("{SPOOFED_HOST}:{port}")]
    );
    wait_event(&events, |e| {
        matches!(e, ProxyEvent::ExchangeTagged { tag, .. } if tag == "dns-spoofed").then_some(())
    })
    .await;
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_table_applies_to_the_next_request_despite_keep_alive() {
    let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind v4");
    let port = v4.local_addr().expect("addr").port();
    let Ok(v6) = TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))).await else {
        eprintln!("skipped: [::1]:{port} is not available");
        return;
    };
    spawn_origin(v4, "v4").await;
    spawn_origin(v6, "v6").await;
    let (engine, _events, cmd_tx) = spawn_engine().await;

    set_overrides(&cmd_tx, Ipv4Addr::LOCALHOST.into()).await;
    let first = get(&engine, port).await;
    set_overrides(&cmd_tx, Ipv6Addr::LOCALHOST.into()).await;
    let second = get(&engine, port).await;

    assert!(first.ends_with("v4"), "{first}");
    assert!(
        second.ends_with("v6"),
        "pooled connection to the old address was reused: {second}"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_tunnel_dials_the_spoofed_ip_and_is_tagged() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    spawn_origin(listener, "origin").await;
    let (engine, events, cmd_tx) = spawn_engine().await;
    set_overrides(&cmd_tx, Ipv4Addr::LOCALHOST.into()).await;

    let mut client = TcpStream::connect(&engine).await.expect("connect");
    let connect =
        format!("CONNECT {SPOOFED_HOST}:{port} HTTP/1.1\r\nHost: {SPOOFED_HOST}:{port}\r\n\r\n");
    client.write_all(connect.as_bytes()).await.expect("write");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .expect("tunnel write");
    let mut reply = vec![0u8; 256];
    let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut reply))
        .await
        .expect("tunnel timeout")
        .expect("tunnel read");

    assert!(String::from_utf8_lossy(&reply[..n]).ends_with("origin"));
    assert_eq!(spoofed_tags(&events), 1);
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn hosts_outside_the_table_are_not_tagged() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind");
    let origin = listener.local_addr().expect("addr");
    spawn_origin(listener, "origin").await;
    let (engine, events, cmd_tx) = spawn_engine().await;
    set_overrides(&cmd_tx, Ipv4Addr::LOCALHOST.into()).await;

    let mut client = TcpStream::connect(&engine).await.expect("connect");
    let req =
        format!("GET http://{origin}/ HTTP/1.1\r\nHost: {origin}\r\nConnection: close\r\n\r\n");
    client.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    client.read_to_end(&mut raw).await.expect("read");

    assert!(String::from_utf8_lossy(&raw).ends_with("origin"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(spoofed_tags(&events), 0);
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
