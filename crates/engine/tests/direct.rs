//! Repeat / Compose (Phase 7b): прямая отправка из UI идёт конвейером
//! движка — ответ возвращается вызывающему, обмен попадает в трафик с
//! тегом происхождения.

use application::dto::capture::{HeaderEntryDto, ProxyEvent};
use application::dto::compose::ComposedRequestDto;
use application::dto::proxy_config::EngineCommand;
use application::services::DirectHttpService;
use domain::entities::exchange::RequestOrigin;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

mod common;
use common::*;

/// Origin: отвечает телом запроса и методом в заголовке.
async fn spawn_echo() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                let method = text.split_whitespace().next().unwrap_or("?").to_string();
                let body = text
                    .split_once("\r\n\r\n")
                    .map(|(_, b)| b.to_string())
                    .unwrap_or_default();
                let resp = format!(
                    "HTTP/1.1 201 Created\r\nX-Method: {method}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn composed_request_is_sent_and_recorded_with_tag() {
    let origin = spawn_echo().await;
    let (_, events, cmd_tx, client) = spawn_engine_with_direct().await;

    let result = client
        .send(ComposedRequestDto {
            method: "POST".into(),
            url: format!("http://{origin}/compose"),
            headers: vec![HeaderEntryDto {
                name: "Content-Type".into(),
                value: "text/plain".into(),
            }],
            body: Some(b"hello".to_vec()),
            origin: RequestOrigin::Compose,
        })
        .await
        .expect("sent");
    assert_eq!(result.status, 201);
    assert_eq!(result.body.as_deref(), Some(&b"hello"[..]));
    assert!(
        result
            .headers
            .iter()
            .any(|h| h.name == "x-method" && h.value == "POST")
    );

    let tags = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeStarted { exchange, .. } => Some(exchange.tags.clone()),
        _ => None,
    })
    .await;
    assert_eq!(tags, ["composed"]);
    wait_event(&events, |e| {
        matches!(e, ProxyEvent::ExchangeCompleted { .. }).then_some(())
    })
    .await;
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_url_is_a_validation_error() {
    let (_, _, cmd_tx, client) = spawn_engine_with_direct().await;
    let error = client
        .send(ComposedRequestDto {
            method: "GET".into(),
            url: "/relative".into(),
            headers: Vec::new(),
            body: None,
            origin: RequestOrigin::Repeat,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(error, domain::errors::DomainError::ValidationError(_)),
        "{error:?}"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
