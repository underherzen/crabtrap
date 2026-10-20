//! WebSocket сквозь прокси (Phase 11b): рукопожатие 101 проходит,
//! кадры пересылаются как есть, сообщения обеих сторон приходят событиями,
//! обмен — WebSocket и завершается с закрытием; сжатие снято.

use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::proxy_config::EngineCommand;
use domain::entities::websocket::{WsDirection, WsOpcode};
use domain::value_objects::http_vo::Protocol;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;
use common::*;

/// Текстовый фрейм (клиентский — с маской).
fn text_frame(payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = vec![
        0x81,
        payload.len() as u8 | if mask.is_some() { 0x80 } else { 0 },
    ];
    match mask {
        Some(key) => {
            out.extend_from_slice(&key);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        }
        None => out.extend_from_slice(payload),
    }
    out
}

/// Origin: 101 на рукопожатие, затем на каждый текстовый фрейм —
/// `echo: <текст>`; заголовки рукопожатия отдаёт в канал.
async fn spawn_ws_origin() -> (std::net::SocketAddr, tokio::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let head = read_head_only(&mut sock).await;
                let _ = tx.send(head).await;
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                          Sec-WebSocket-Accept: dummy\r\n\r\n",
                    )
                    .await;
                let mut buf = [0u8; 1024];
                while let Ok(n) = sock.read(&mut buf).await {
                    if n < 6 {
                        break;
                    }
                    let opcode = buf[0] & 0x0F;
                    let len = (buf[1] & 0x7F) as usize;
                    let key = [buf[2], buf[3], buf[4], buf[5]];
                    let text: Vec<u8> = buf[6..6 + len]
                        .iter()
                        .enumerate()
                        .map(|(i, b)| b ^ key[i % 4])
                        .collect();
                    if opcode == 0x8 {
                        let _ = sock.write_all(&[0x88, 0x00]).await;
                        break;
                    }
                    let reply = [b"echo: ".as_slice(), &text].concat();
                    let _ = sock.write_all(&text_frame(&reply, None)).await;
                }
            });
        }
    });
    (addr, rx)
}

#[tokio::test(flavor = "multi_thread")]
async fn websocket_messages_are_relayed_and_recorded() {
    let (origin, mut handshakes) = spawn_ws_origin().await;
    let (engine, events, cmd_tx) = spawn_engine().await;

    let mut client = TcpStream::connect(&engine).await.expect("connect");
    let handshake = format!(
        "GET http://{origin}/socket HTTP/1.1\r\nHost: {origin}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Extensions: permessage-deflate\r\n\r\n"
    );
    client
        .write_all(handshake.as_bytes())
        .await
        .expect("handshake");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert!(
        head.to_ascii_lowercase().contains("upgrade: websocket"),
        "{head}"
    );

    let seen = handshakes
        .recv()
        .await
        .expect("origin handshake")
        .to_ascii_lowercase();
    assert!(seen.contains("upgrade: websocket"), "{seen}");
    assert!(
        !seen.contains("permessage-deflate"),
        "compression must be stripped: {seen}"
    );

    client
        .write_all(&text_frame(b"hi", Some([7, 7, 7, 7])))
        .await
        .expect("send");
    let mut reply = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(5), client.read(&mut reply))
        .await
        .expect("reply in time")
        .expect("read");
    assert_eq!(&reply[..n], text_frame(b"echo: hi", None).as_slice());

    // Close от клиента → origin отвечает close и закрывает.
    client
        .write_all(&[0x88, 0x80, 1, 2, 3, 4])
        .await
        .expect("close");
    let _ = tokio::time::timeout(Duration::from_secs(5), client.read(&mut reply)).await;
    drop(client);

    let protocol = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeStarted { exchange, .. } => Some(exchange.protocol),
        _ => None,
    })
    .await;
    assert_eq!(protocol, Protocol::WebSocket);
    wait_event(&events, |e| {
        matches!(e, ProxyEvent::ExchangeCompleted { .. }).then_some(())
    })
    .await;
    let messages: Vec<_> = events
        .snapshot()
        .into_iter()
        .filter_map(|e| match e {
            ProxyEvent::WebSocketMessage { message, .. } => {
                Some((message.direction, message.opcode, message.text()))
            }
            _ => None,
        })
        .collect();
    assert!(
        messages.contains(&(WsDirection::Sent, WsOpcode::Text, Some("hi".into()))),
        "{messages:?}"
    );
    assert!(
        messages.contains(&(
            WsDirection::Received,
            WsOpcode::Text,
            Some("echo: hi".into())
        )),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|(d, o, _)| *d == WsDirection::Sent && *o == WsOpcode::Close)
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_with_an_open_socket_settles_the_exchange() {
    let (origin, _handshakes) = spawn_ws_origin().await;
    let (engine, events, cmd_tx) = spawn_engine().await;
    let mut client = TcpStream::connect(&engine).await.expect("connect");
    let handshake = format!(
        "GET http://{origin}/socket HTTP/1.1\r\nHost: {origin}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    client
        .write_all(handshake.as_bytes())
        .await
        .expect("handshake");
    assert!(
        read_head_only(&mut client)
            .await
            .starts_with("HTTP/1.1 101")
    );

    cmd_tx.send(EngineCommand::Stop).await.expect("stop");
    let error = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeFailed { error, .. } => Some(error.clone()),
        _ => None,
    })
    .await;
    assert!(error.contains("aborted"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn map_remote_sends_the_handshake_and_frames_to_the_target() {
    let (origin, mut handshakes) = spawn_ws_origin().await;
    let rules = domain::entities::rule::RuleSet {
        map_remote: vec![domain::entities::rule::MapRemoteRule {
            id: domain::value_objects::RuleId::new(),
            r#match: domain::value_objects::TrafficMatch {
                host_glob: Some("socket.crabtrap.test".into()),
                ..Default::default()
            },
            remote: format!("ws://{origin}/v2"),
            preserve_host: false,
            path_mode: domain::entities::RemotePathMode::Replace,
            enabled: true,
        }],
        ..Default::default()
    };
    let pipeline = std::sync::Arc::new(infrastructure::services::RulesInterceptionPipeline::new(
        rules,
    ));
    let (engine, events, cmd_tx) = spawn_engine_with_pipeline(pipeline).await;

    let mut client = TcpStream::connect(&engine).await.expect("connect");
    client
        .write_all(
            b"GET http://socket.crabtrap.test/v1/chat HTTP/1.1\r\nHost: socket.crabtrap.test\r\n\
              Upgrade: websocket\r\nConnection: Upgrade\r\n\
              Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        )
        .await
        .expect("handshake");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    let seen = tokio::time::timeout(Duration::from_secs(5), handshakes.recv())
        .await
        .expect("origin handshake in time")
        .expect("origin handshake");
    assert!(seen.starts_with("GET /v2 "), "{seen}");
    assert!(
        seen.to_ascii_lowercase()
            .contains(&format!("host: {origin}")),
        "{seen}"
    );

    client
        .write_all(&text_frame(b"hi", Some([7, 7, 7, 7])))
        .await
        .expect("send");
    let expected = text_frame(b"echo: hi", None);
    let mut reply = vec![0u8; expected.len()];
    tokio::time::timeout(Duration::from_secs(5), client.read_exact(&mut reply))
        .await
        .expect("reply in time")
        .expect("read");
    assert_eq!(reply, expected);
    wait_event(&events, |e| {
        matches!(e, ProxyEvent::ExchangeTagged { tag, .. } if tag == "map-remote").then_some(())
    })
    .await;
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
