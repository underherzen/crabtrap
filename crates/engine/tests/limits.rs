//! Лимит одновременных соединений учитывает и туннели: CONNECT после
//! upgrade живёт дольше http1-соединения, из которого вырос.

use std::time::Duration;

use application::dto::proxy_config::EngineCommand;
use engine::EngineConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;
use common::*;

async fn spawn_echo() -> std::net::SocketAddr {
    let echo = TcpListener::bind("127.0.0.1:0").await.expect("echo bind");
    let addr = echo.local_addr().expect("echo addr");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}

async fn open_tunnel(engine_addr: &str, target: std::net::SocketAddr) -> TcpStream {
    let mut client = TcpStream::connect(engine_addr).await.expect("connect");
    let connect = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    client.write_all(connect.as_bytes()).await.expect("write");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    client
}

/// Соединение принято и обслужено (true) или сброшено лимитом (false).
async fn connection_is_served(engine_addr: &str) -> bool {
    let mut client = TcpStream::connect(engine_addr).await.expect("connect");
    let _ = client
        .write_all(b"GET /ssl HTTP/1.1\r\nHost: x\r\n\r\n")
        .await;
    let mut buf = [0u8; 16];
    matches!(
        tokio::time::timeout(Duration::from_secs(2), client.read(&mut buf)).await,
        Ok(Ok(n)) if n > 0
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn open_tunnel_occupies_a_connection_slot() {
    let echo = spawn_echo().await;
    let (engine_addr, _events, cmd_tx) =
        spawn_engine_with(EngineConfig::new("127.0.0.1:0", 1), body_store()).await;

    let mut tunnel = open_tunnel(&engine_addr, echo).await;
    tunnel.write_all(b"ping").await.expect("tunnel write");
    let mut pong = [0u8; 4];
    tunnel.read_exact(&mut pong).await.expect("tunnel read");

    assert!(
        !connection_is_served(&engine_addr).await,
        "limit 1 with a live tunnel must refuse another connection"
    );

    drop(tunnel);
    // Слот освобождается асинхронно (туннельная задача видит EOF).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut served = false;
    while tokio::time::Instant::now() < deadline && !served {
        served = connection_is_served(&engine_addr).await;
    }
    assert!(served, "slot must be released after the tunnel closes");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
