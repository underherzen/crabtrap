//! Track S сквозь движок: скрипт меняет ответ (статус, заголовок,
//! JSON-тело) — клиент видит правки, в трафике тег; Mock API отвечает без
//! origin; зависший скрипт не вешает обмен; console.log приходит событием.

use std::sync::Arc;
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::proxy_config::EngineCommand;
use domain::entities::rule::RuleSet;
use domain::entities::script_rule::ScriptRule;
use domain::value_objects::{RuleId, TrafficMatch};
use engine::EngineConfig;
use infrastructure::services::{
    BoaScriptRuntime, RulesInterceptionPipeline, RustBodyDecoder, ScriptingDeps,
    ScriptingInterceptionPipeline,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;
use common::*;

/// Origin с JSON-ответом; считает обращения.
async fn spawn_json_origin() -> (std::net::SocketAddr, Arc<std::sync::Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(std::sync::Mutex::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            *counter.lock().expect("hits") += 1;
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let body = r#"{"user":{"name":"origin"}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (addr, hits)
}

fn script(source: &str, mock_api: bool) -> ScriptRule {
    ScriptRule {
        id: RuleId::new(),
        name: "e2e".into(),
        r#match: TrafficMatch {
            host_glob: Some("127.0.0.1".into()),
            ..Default::default()
        },
        enabled: true,
        run_on_request: true,
        run_on_response: true,
        mock_api,
        source: source.into(),
    }
}

async fn engine_with_script(
    rule: ScriptRule,
    timeout: Duration,
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<EngineCommand>,
) {
    // Пайплайн и движок пишут в один recorder.
    let events = RecordingPublisher::new();
    let pipeline = Arc::new(ScriptingInterceptionPipeline::new(
        Arc::new(RulesInterceptionPipeline::new(RuleSet::default())),
        RuleSet::default(),
        ScriptingDeps {
            runtime: Arc::new(BoaScriptRuntime::with(2, timeout)),
            decoder: Arc::new(RustBodyDecoder::new()),
            events: events.clone(),
        },
    ));
    let (addr, engine_events, cmd_tx) = spawn_engine_cfg_pipeline_with_events(
        EngineConfig::new("127.0.0.1:0", 16),
        pipeline,
        events,
    )
    .await;
    let rules = RuleSet {
        scripts: vec![rule],
        ..Default::default()
    };
    cmd_tx
        .send(EngineCommand::UpdateRules { snapshot: rules })
        .await
        .expect("rules");
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, engine_events, cmd_tx)
}

async fn get(engine: &str, url: &str) -> (String, String) {
    let mut client = TcpStream::connect(engine).await.expect("connect");
    let authority = url.split('/').nth(2).unwrap_or_default();
    let req = format!("GET {url} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    client.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), client.read_to_end(&mut raw)).await;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    (head.to_string(), body.to_string())
}

fn tags(events: &Arc<RecordingPublisher>) -> Vec<String> {
    events
        .snapshot()
        .into_iter()
        .filter_map(|e| match e {
            ProxyEvent::ExchangeTagged { tag, .. } => Some(tag),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn script_rewrites_response_status_header_and_json() {
    let (origin, _) = spawn_json_origin().await;
    let source = r#"
        async function onResponse(context, url, request, response) {
          console.log("before", response.body.user.name);
          response.statusCode = 500;
          response.headers["X-Scripted"] = "yes";
          response.body.user.name = "crabtrap";
          return response;
        }"#;
    let (engine, events, cmd_tx) =
        engine_with_script(script(source, false), Duration::from_secs(10)).await;

    let (head, body) = get(&engine, &format!("http://{origin}/user")).await;
    assert!(head.starts_with("HTTP/1.1 500"), "{head}");
    assert!(
        head.to_ascii_lowercase().contains("x-scripted: yes"),
        "{head}"
    );
    assert_eq!(body, r#"{"user":{"name":"crabtrap"}}"#);

    wait_event(&events, |e| {
        matches!(e, ProxyEvent::ExchangeCompleted { .. }).then_some(())
    })
    .await;
    let log = wait_event(&events, |e| match e {
        ProxyEvent::ScriptLog { message, .. } => Some(message.clone()),
        _ => None,
    })
    .await;
    assert_eq!(log, "before origin");
    let tags = tags(&events);
    assert!(
        tags.contains(&"scripted".to_string()) && tags.contains(&"modified".to_string()),
        "{tags:?}"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn mock_api_answers_without_origin() {
    let (origin, hits) = spawn_json_origin().await;
    let source = r#"
        function onResponse(context, url, request, response) {
          response.headers["Content-Type"] = "application/json";
          response.body = { mocked: true, path: request.path };
          return response;
        }"#;
    let (engine, events, cmd_tx) =
        engine_with_script(script(source, true), Duration::from_secs(10)).await;

    let (head, body) = get(&engine, &format!("http://{origin}/mock/me")).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(body, r#"{"mocked":true,"path":"/mock/me"}"#);
    assert_eq!(*hits.lock().expect("hits"), 0, "origin must not be hit");
    wait_event(&events, |e| {
        matches!(e, ProxyEvent::ExchangeCompleted { .. }).then_some(())
    })
    .await;
    assert!(tags(&events).contains(&"mocked".to_string()));
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn hanging_script_times_out_and_exchange_passes_unchanged() {
    let (origin, _) = spawn_json_origin().await;
    let source = "function onResponse(c,u,q,r){ let x = 0; while (true) { x++; } }";
    let (engine, events, cmd_tx) =
        engine_with_script(script(source, false), Duration::from_millis(200)).await;

    let (head, body) = get(&engine, &format!("http://{origin}/slow")).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(body, r#"{"user":{"name":"origin"}}"#);
    wait_event(&events, |e| match e {
        ProxyEvent::ScriptLog { message, .. } if message.contains("timed out") => Some(()),
        _ => None,
    })
    .await;
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
