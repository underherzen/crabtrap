//! Брейкпоинт сквозь движок (Phase 7c): запрос паркуется, в событиях —
//! InterceptWaiting; команда ResolveIntercept с правкой продолжает обмен —
//! origin видит изменённый заголовок.

use std::sync::Arc;
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::proxy_config::EngineCommand;
use application::dto::rules::{InterceptResolutionDto, InterceptableMessageDto};
use domain::entities::rule::{BreakPhase, BreakpointRule, RuleSet};
use domain::value_objects::http_vo::HeaderEntry;
use domain::value_objects::{RuleId, TrafficMatch};
use engine::EngineConfig;
use infrastructure::services::{
    BreakpointInterceptionPipeline, RulesInterceptionPipeline, RustBodyDecoder,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;
use common::*;

/// Origin: отвечает головой полученного запроса.
async fn spawn_head_echo() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{head}",
                    head.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn parked_request_resumes_with_edits() {
    let origin = spawn_head_echo().await;
    let rules = RuleSet {
        breakpoints: vec![BreakpointRule {
            id: RuleId::new(),
            r#match: TrafficMatch {
                host_glob: Some("127.0.0.1".into()),
                ..Default::default()
            },
            phase: BreakPhase::Request,
            enabled: true,
        }],
        ..Default::default()
    };
    let events = RecordingPublisher::new();
    let pipeline = Arc::new(BreakpointInterceptionPipeline::new(
        Arc::new(RulesInterceptionPipeline::new(RuleSet::default())),
        RuleSet::default(),
        events.clone(),
        Arc::new(RustBodyDecoder::new()),
    ));
    let (engine, events, cmd_tx) = spawn_engine_cfg_pipeline_with_events(
        EngineConfig::new("127.0.0.1:0", 16),
        pipeline,
        events,
    )
    .await;
    cmd_tx
        .send(EngineCommand::UpdateRules { snapshot: rules })
        .await
        .expect("rules");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let request = tokio::spawn(async move {
        let mut client = TcpStream::connect(&engine).await.expect("connect");
        let req = format!(
            "GET http://{origin}/bp HTTP/1.1\r\nHost: {origin}\r\nConnection: close\r\n\r\n"
        );
        client.write_all(req.as_bytes()).await.expect("write");
        let mut raw = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut raw)).await;
        String::from_utf8_lossy(&raw).to_string()
    });

    let preview = wait_event(&events, |e| match e {
        ProxyEvent::InterceptWaiting { preview, .. } => Some(preview.clone()),
        _ => None,
    })
    .await;
    let mut headers = preview.message.headers.clone();
    headers.push(HeaderEntry {
        name: "x-breakpoint".into(),
        value: "edited".into(),
    });
    let edited = InterceptableMessageDto {
        headers,
        ..preview.message
    };
    cmd_tx
        .send(EngineCommand::ResolveIntercept {
            id: preview.intercept_id,
            resolution: InterceptResolutionDto::Resume {
                edited: Some(edited),
            },
        })
        .await
        .expect("resolve");

    let reply = request.await.expect("client");
    assert!(
        reply.to_ascii_lowercase().contains("x-breakpoint: edited"),
        "{reply}"
    );
    wait_event(&events, |e| {
        matches!(e, ProxyEvent::InterceptResolved { .. }).then_some(())
    })
    .await;
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
