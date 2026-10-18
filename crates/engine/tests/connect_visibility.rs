//! Видимость в трафике того, что Charles показывает всегда: туннели
//! (CONNECT без расшифровки), неудавшиеся CONNECT и понятная причина
//! недоступности origin.

use application::dto::capture::ProxyEvent;
use application::dto::proxy_config::EngineCommand;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;
use common::*;

/// Порт, на котором гарантированно никто не слушает.
async fn closed_port_addr() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    addr
}

fn failure_of(e: &ProxyEvent) -> Option<String> {
    match e {
        ProxyEvent::ExchangeFailed { error, .. } => Some(error.clone()),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_origin_reports_connection_error_not_body_drop() {
    let (engine_addr, events, cmd_tx) = spawn_engine().await;
    let dead = closed_port_addr().await;

    let mut client = TcpStream::connect(&engine_addr).await.expect("connect");
    let req = format!("GET http://{dead}/ HTTP/1.1\r\nHost: {dead}\r\n\r\n");
    client.write_all(req.as_bytes()).await.expect("write");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 502"), "{head}");

    let error = wait_event(&events, failure_of).await;
    assert!(
        error.to_lowercase().contains("refused"),
        "real cause expected, got: {error}"
    );
    assert!(!error.contains("request body"), "got: {error}");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn passthrough_tunnel_is_recorded_as_connect_exchange() {
    let echo = TcpListener::bind("127.0.0.1:0").await.expect("echo bind");
    let echo_addr = echo.local_addr().expect("echo addr");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    let (engine_addr, events, cmd_tx) = spawn_engine().await;

    let mut client = TcpStream::connect(&engine_addr).await.expect("connect");
    let connect = format!("CONNECT {echo_addr} HTTP/1.1\r\nHost: {echo_addr}\r\n\r\n");
    client.write_all(connect.as_bytes()).await.expect("write");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    let started = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeStarted { exchange, .. } => Some(exchange.clone()),
        _ => None,
    })
    .await;
    assert_eq!(started.method, "CONNECT");
    assert_eq!(
        started.host,
        echo_addr.to_string(),
        "non-443 port kept in authority"
    );
    assert_eq!(started.tls_mitm, Some(false), "tunnel, not decrypted");

    client.write_all(b"0123456789").await.expect("tunnel write");
    let mut echoed = [0u8; 10];
    client.read_exact(&mut echoed).await.expect("tunnel read");
    drop(client);

    let (status, down) = (
        wait_event(&events, |e| match e {
            ProxyEvent::ResponseHeaders { id, status, .. } if *id == started.id => Some(*status),
            _ => None,
        })
        .await,
        wait_event(&events, |e| match e {
            ProxyEvent::ExchangeCompleted { id, raw_size, .. } if *id == started.id => {
                Some(*raw_size)
            }
            _ => None,
        })
        .await,
    );
    assert_eq!(status, 200);
    assert_eq!(down, 10, "bytes origin → client counted");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_connect_is_recorded_with_cause() {
    let (engine_addr, events, cmd_tx) = spawn_engine().await;
    let dead = closed_port_addr().await;

    let mut client = TcpStream::connect(&engine_addr).await.expect("connect");
    let connect = format!("CONNECT {dead} HTTP/1.1\r\nHost: {dead}\r\n\r\n");
    client.write_all(connect.as_bytes()).await.expect("write");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 502"), "{head}");

    let started = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeStarted { exchange, .. } => Some(exchange.clone()),
        _ => None,
    })
    .await;
    assert_eq!(started.method, "CONNECT");
    let error = wait_event(&events, failure_of).await;
    assert!(error.to_lowercase().contains("refused"), "got: {error}");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_engine_finishes_open_tunnel_record_and_connection() {
    let echo = TcpListener::bind("127.0.0.1:0").await.expect("echo bind");
    let echo_addr = echo.local_addr().expect("echo addr");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    let (engine_addr, events, cmd_tx) = spawn_engine().await;

    let mut client = TcpStream::connect(&engine_addr).await.expect("connect");
    let connect = format!("CONNECT {echo_addr} HTTP/1.1\r\nHost: {echo_addr}\r\n\r\n");
    client.write_all(connect.as_bytes()).await.expect("write");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let started = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeStarted { exchange, .. } => Some(exchange.clone()),
        _ => None,
    })
    .await;

    // Stop абортит туннельную задачу посреди копирования.
    cmd_tx.send(EngineCommand::Stop).await.ok();

    let terminal = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeFailed { id, error } if *id == started.id => Some(error.clone()),
        ProxyEvent::ExchangeCompleted { id, .. } if *id == started.id => Some("completed".into()),
        _ => None,
    })
    .await;
    assert!(!terminal.is_empty());
    wait_event(&events, |e| match e {
        ProxyEvent::ConnectionClosed { id } if *id == started.connection_id => Some(()),
        _ => None,
    })
    .await;
    drop(client);
}

#[tokio::test(flavor = "multi_thread")]
async fn exchange_started_carries_client_request_headers() {
    let origin = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let origin_addr = origin.local_addr().expect("origin addr");
    tokio::spawn(async move {
        if let Ok((mut sock, _)) = origin.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    });
    let (engine_addr, events, cmd_tx) = spawn_engine().await;

    let mut client = TcpStream::connect(&engine_addr).await.expect("connect");
    let req = format!(
        "GET http://{origin_addr}/h HTTP/1.1\r\nHost: {origin_addr}\r\nUser-Agent: crabtrap-test/1.0\r\nX-Trace: abc\r\n\r\n"
    );
    client.write_all(req.as_bytes()).await.expect("write");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 204"), "{head}");

    let headers = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeStarted {
            request_headers, ..
        } => Some(request_headers.clone()),
        _ => None,
    })
    .await;
    let get = |name: &str| {
        headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.clone())
    };
    assert_eq!(get("user-agent").as_deref(), Some("crabtrap-test/1.0"));
    assert_eq!(get("x-trace").as_deref(), Some("abc"));
    assert_eq!(get("host"), Some(origin_addr.to_string()));
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
