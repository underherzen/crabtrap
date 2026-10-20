//! Reverse Proxy: запрос на локальный порт уходит на цель с тем же path и
//! query, записывается с тегом `reverse-proxy`; Host — цели или клиента;
//! занятый порт — событие с ошибкой; снятая запись гасит листенер.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::proxy_config::EngineCommand;
use domain::entities::network::ReverseProxyConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;
use common::*;

/// Origin: отвечает «origin», голову запроса кладёт в `heads`.
async fn spawn_origin() -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let heads = Arc::new(Mutex::new(Vec::new()));
    let seen = heads.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let head = read_head_only(&mut sock).await;
                seen.lock().expect("heads").push(head);
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\norigin",
                    )
                    .await;
            });
        }
    });
    (addr, heads)
}

/// Свободный порт (bind :0 и сразу освободить).
async fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    listener.local_addr().expect("addr").port()
}

async fn set_reverse(
    cmd_tx: &async_channel::Sender<EngineCommand>,
    entries: Vec<ReverseProxyConfig>,
) {
    cmd_tx
        .send(EngineCommand::SetReverseProxies { entries })
        .await
        .expect("send");
}

/// Дождаться `ReverseProxyStatus` порта, удовлетворяющего `pick`.
async fn wait_status<T>(
    events: &Arc<RecordingPublisher>,
    port: u16,
    pick: impl Fn(&Option<String>, &Option<String>) -> Option<T>,
) -> T {
    wait_event(events, |e| match e {
        ProxyEvent::ReverseProxyStatus {
            listen_port,
            listening,
            error,
        } if *listen_port == port => pick(listening, error),
        _ => None,
    })
    .await
}

async fn get(port: u16, path: &str) -> String {
    let mut client = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n");
    client.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    tokio::time::timeout(TEST_TIMEOUT, client.read_to_end(&mut raw))
        .await
        .expect("reply in time")
        .expect("read");
    String::from_utf8_lossy(&raw).to_string()
}

fn config(port: u16, origin: std::net::SocketAddr, preserve_host: bool) -> ReverseProxyConfig {
    ReverseProxyConfig {
        listen_port: port,
        target: format!("http://{origin}"),
        preserve_host,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_to_the_local_port_reach_the_target_and_are_recorded() {
    let (origin, heads) = spawn_origin().await;
    let (_engine, events, cmd_tx) = spawn_engine().await;
    let port = free_port().await;
    set_reverse(&cmd_tx, vec![config(port, origin, false)]).await;
    wait_status(&events, port, |listening, _| listening.clone()).await;

    let reply = get(port, "/v1/users?id=7").await;

    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(reply.ends_with("origin"), "{reply}");
    let head = heads.lock().expect("heads")[0].to_ascii_lowercase();
    assert!(head.starts_with("get /v1/users?id=7 "), "{head}");
    assert!(head.contains(&format!("host: {origin}")), "{head}");
    let (host, path, tags) = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeStarted { exchange, .. } => Some((
            exchange.host.clone(),
            exchange.path.clone(),
            exchange.tags.clone(),
        )),
        _ => None,
    })
    .await;
    assert_eq!(host, "127.0.0.1");
    assert_eq!(path, "/v1/users?id=7");
    assert_eq!(tags, vec!["reverse-proxy".to_string()]);
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_original_host_sends_the_client_host() {
    let (origin, heads) = spawn_origin().await;
    let (_engine, events, cmd_tx) = spawn_engine().await;
    let port = free_port().await;
    set_reverse(&cmd_tx, vec![config(port, origin, true)]).await;
    wait_status(&events, port, |listening, _| listening.clone()).await;

    let reply = get(port, "/").await;

    assert!(reply.ends_with("origin"), "{reply}");
    let head = heads.lock().expect("heads")[0].to_ascii_lowercase();
    assert!(head.contains(&format!("host: localhost:{port}")), "{head}");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn busy_port_reports_an_error_and_removed_entries_stop_listening() {
    let (origin, _heads) = spawn_origin().await;
    let (_engine, events, cmd_tx) = spawn_engine().await;
    let busy = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let busy_port = busy.local_addr().expect("addr").port();
    let port = free_port().await;

    set_reverse(
        &cmd_tx,
        vec![
            config(busy_port, origin, false),
            config(port, origin, false),
        ],
    )
    .await;
    let error = wait_status(&events, busy_port, |_, error| error.clone()).await;
    wait_status(&events, port, |listening, _| listening.clone()).await;
    assert!(error.contains("cannot listen"), "{error}");

    set_reverse(&cmd_tx, Vec::new()).await;
    wait_status(&events, port, |listening, error| {
        (listening.is_none() && error.is_none()).then_some(())
    })
    .await;
    let refused = tokio::time::timeout(
        Duration::from_secs(2),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .expect("connect attempt in time");
    assert!(refused.is_err(), "listener must be gone");
    drop(busy);
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_on_a_reverse_port_is_refused() {
    let (origin, _heads) = spawn_origin().await;
    let (_engine, events, cmd_tx) = spawn_engine().await;
    let port = free_port().await;
    set_reverse(&cmd_tx, vec![config(port, origin, false)]).await;
    wait_status(&events, port, |listening, _| listening.clone()).await;

    let mut client = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    client
        .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
        .await
        .expect("write");
    let head = read_head_only(&mut client).await;

    assert!(head.starts_with("HTTP/1.1 405"), "{head}");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

fn errors_for(events: &Arc<RecordingPublisher>, port: u16) -> usize {
    events
        .snapshot()
        .iter()
        .filter(|e| {
            matches!(e, ProxyEvent::ReverseProxyStatus { listen_port, error: Some(_), .. }
                if *listen_port == port)
        })
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_entry_is_reported_once_until_it_changes() {
    let (origin, _heads) = spawn_origin().await;
    let (_engine, events, cmd_tx) = spawn_engine().await;
    let busy = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = busy.local_addr().expect("addr").port();
    let marker = free_port().await;

    for _ in 0..3 {
        set_reverse(&cmd_tx, vec![config(port, origin, false)]).await;
    }
    // Последняя команда обработана, когда встал листенер-метка.
    set_reverse(
        &cmd_tx,
        vec![config(port, origin, false), config(marker, origin, false)],
    )
    .await;
    wait_status(&events, marker, |listening, _| listening.clone()).await;
    assert_eq!(
        errors_for(&events, port),
        1,
        "same entry, same error — one report"
    );

    set_reverse(&cmd_tx, vec![config(port, origin, true)]).await;
    wait_event(&events, |_| (errors_for(&events, port) == 2).then_some(())).await;
    set_reverse(&cmd_tx, Vec::new()).await;
    wait_status(&events, port, |listening, error| {
        (listening.is_none() && error.is_none()).then_some(())
    })
    .await;
    drop(busy);
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_target_on_its_own_port_is_refused_by_the_engine() {
    let (_engine, events, cmd_tx) = spawn_engine().await;
    let port = free_port().await;
    let looping = ReverseProxyConfig {
        listen_port: port,
        target: format!("http://localhost:{port}"),
        preserve_host: false,
    };

    set_reverse(&cmd_tx, vec![looping]).await;

    let error = wait_status(&events, port, |_, error| error.clone()).await;
    assert!(error.contains("forwards to itself"), "{error}");
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_closes_reverse_listeners() {
    let (origin, _heads) = spawn_origin().await;
    let (_engine, events, cmd_tx) = spawn_engine().await;
    let port = free_port().await;
    set_reverse(&cmd_tx, vec![config(port, origin, false)]).await;
    wait_status(&events, port, |listening, _| listening.clone()).await;

    cmd_tx.send(EngineCommand::Stop).await.expect("stop");

    wait_status(&events, port, |listening, error| {
        (listening.is_none() && error.is_none()).then_some(())
    })
    .await;
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn map_remote_keeping_host_sends_the_reverse_target_host() {
    use domain::entities::rule::{MapRemoteRule, RemotePathMode, RuleSet};
    use domain::value_objects::{RuleId, TrafficMatch};

    let (target, _target_heads) = spawn_origin().await;
    let (mapped, mapped_heads) = spawn_origin().await;
    let rules = RuleSet {
        map_remote: vec![MapRemoteRule {
            id: RuleId::new(),
            r#match: TrafficMatch {
                host_glob: Some("127.0.0.1".into()),
                ..Default::default()
            },
            remote: format!("http://{mapped}"),
            preserve_host: true,
            path_mode: RemotePathMode::Replace,
            enabled: true,
        }],
        ..Default::default()
    };
    let pipeline = Arc::new(infrastructure::services::RulesInterceptionPipeline::new(
        rules,
    ));
    let (_engine, events, cmd_tx) = spawn_engine_with_pipeline(pipeline).await;
    let port = free_port().await;
    set_reverse(&cmd_tx, vec![config(port, target, false)]).await;
    wait_status(&events, port, |listening, _| listening.clone()).await;

    let reply = get(port, "/x").await;

    assert!(reply.ends_with("origin"), "{reply}");
    let head = mapped_heads.lock().expect("heads")[0].to_ascii_lowercase();
    assert!(head.contains(&format!("host: {target}")), "{head}");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
