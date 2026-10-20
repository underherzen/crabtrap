//! Движок применяет правки пайплайна (Track S1, основа Phase 7): заголовки,
//! URI и тело запроса; статус, заголовки и тело ответа; ответ без origin
//! (`RouteDecision::Respond`); тело отдаётся пайплайну только по
//! `wants_body` и только в пределах капа.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::proxy_config::EngineCommand;
use application::services::{
    ConnectDecision, InterceptContext, InterceptOutcome, InterceptionPipeline, RouteDecision,
};
use async_trait::async_trait;
use domain::entities::ClientBody;
use domain::value_objects::Host;
use domain::value_objects::http_vo::HeaderEntry;
use engine::EngineConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;
use common::*;

type Edit = Box<dyn Fn(InterceptContext) -> InterceptOutcome + Send + Sync>;

/// Пайплайн-двойник: правки задаются замыканиями, контексты запоминаются.
struct EditingPipeline {
    wants_request_body: bool,
    wants_response_body: bool,
    on_request: Edit,
    on_response: Edit,
    seen: Mutex<Vec<InterceptContext>>,
}

impl EditingPipeline {
    fn new() -> Self {
        Self {
            wants_request_body: false,
            wants_response_body: false,
            on_request: Box::new(proceed),
            on_response: Box::new(proceed),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<InterceptContext> {
        self.seen.lock().expect("seen lock").clone()
    }
}

fn proceed(context: InterceptContext) -> InterceptOutcome {
    InterceptOutcome::Proceed {
        message: context.message,
        route: RouteDecision::Origin,
    }
}

#[async_trait]
impl InterceptionPipeline for EditingPipeline {
    async fn on_connect(&self, _host: &Host, _port: u16) -> ConnectDecision {
        ConnectDecision::Passthrough
    }

    fn wants_body(&self, context: &InterceptContext) -> bool {
        match context.phase {
            domain::entities::rule::BreakPhase::Request => self.wants_request_body,
            _ => self.wants_response_body,
        }
    }

    async fn on_request(&self, context: InterceptContext) -> InterceptOutcome {
        self.seen.lock().expect("seen lock").push(context.clone());
        (self.on_request)(context)
    }

    async fn on_response(&self, context: InterceptContext) -> InterceptOutcome {
        self.seen.lock().expect("seen lock").push(context.clone());
        (self.on_response)(context)
    }
}

/// Origin-эхо: отвечает телом `<голова запроса>\n\n<тело запроса>`;
/// счётчик обращений — проверить, что mock до origin не дошёл.
async fn spawn_echo_origin() -> (std::net::SocketAddr, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(Mutex::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            *counter.lock().expect("hits lock") += 1;
            tokio::spawn(async move {
                let (head, body) = read_request(&mut sock).await;
                let payload = format!("{head}\n\n{body}");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Origin: yes\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (addr, hits)
}

/// Голова (строки без пустой) и тело запроса по Content-Length.
async fn read_request(sock: &mut TcpStream) -> (String, String) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(idx) = find_head_end(&buf) {
            break idx;
        }
        let n = sock.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            return (String::from_utf8_lossy(&buf).into_owned(), String::new());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let length = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let n = sock.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    (head, String::from_utf8_lossy(&body).into_owned())
}

/// Ответ прокси: статус, голова, тело.
struct Reply {
    status: u16,
    head: String,
    body: String,
}

async fn send(engine: &str, method: &str, url: &str, body: &str) -> Reply {
    let mut client = TcpStream::connect(engine).await.expect("connect");
    let authority = url.split('/').nth(2).unwrap_or_default();
    let req = format!(
        "{method} {url} HTTP/1.1\r\nHost: {authority}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    client.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut raw)).await;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    Reply {
        status: head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        head: head.to_string(),
        body: body.to_string(),
    }
}

async fn engine_with(
    pipeline: Arc<EditingPipeline>,
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<EngineCommand>,
) {
    spawn_engine_cfg_pipeline(EngineConfig::new("127.0.0.1:0", 16), pipeline).await
}

fn set_header(headers: &mut Vec<HeaderEntry>, name: &str, value: &str) {
    headers.retain(|h| !h.name.eq_ignore_ascii_case(name));
    headers.push(HeaderEntry {
        name: name.to_string(),
        value: value.to_string(),
    });
}

fn has_tag(events: &[ProxyEvent], wanted: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(e, ProxyEvent::ExchangeTagged { tag, .. } if tag == wanted))
}

async fn wait_completed(events: &Arc<RecordingPublisher>) {
    wait_event(events, |e| {
        matches!(e, ProxyEvent::ExchangeCompleted { .. }).then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn request_header_and_uri_edits_reach_origin() {
    let (origin, _) = spawn_echo_origin().await;
    let mut pipeline = EditingPipeline::new();
    pipeline.on_request = Box::new(|mut ctx| {
        set_header(&mut ctx.message.headers, "x-script", "1");
        ctx.message.method = Some("PUT".into());
        ctx.message.uri = ctx.message.uri.map(|u| u.replace("/a?x=1", "/b?y=2"));
        proceed(ctx)
    });
    let (engine, events, cmd_tx) = engine_with(Arc::new(pipeline)).await;

    let reply = send(&engine, "GET", &format!("http://{origin}/a?x=1"), "").await;
    assert_eq!(reply.status, 200);
    assert!(
        reply.body.starts_with("PUT /b?y=2 HTTP/1.1"),
        "{}",
        reply.body
    );
    assert!(
        reply.body.to_ascii_lowercase().contains("x-script: 1"),
        "{}",
        reply.body
    );
    wait_completed(&events).await;
    assert!(has_tag(&events.snapshot(), "modified"));
    let (method, path, client_body) = wait_event(&events, |e| match e {
        ProxyEvent::RequestEdited {
            method,
            path,
            client_body,
            ..
        } => Some((method.clone(), path.clone(), client_body.clone())),
        _ => None,
    })
    .await;
    assert_eq!((method.as_str(), path.as_str()), ("PUT", "/b?y=2"));
    assert_eq!(
        client_body,
        ClientBody::Unchanged,
        "body untouched — nothing extra stored"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn untouched_message_is_not_tagged_and_body_not_loaded() {
    let (origin, _) = spawn_echo_origin().await;
    let pipeline = Arc::new(EditingPipeline::new());
    let (engine, events, cmd_tx) = engine_with(pipeline.clone()).await;

    let reply = send(&engine, "POST", &format!("http://{origin}/p"), "abc").await;
    assert_eq!(reply.status, 200);
    assert!(reply.body.ends_with("\n\nabc"), "{}", reply.body);
    wait_completed(&events).await;
    assert!(!has_tag(&events.snapshot(), "modified"));
    assert!(pipeline.seen().iter().all(|c| c.message.body.is_none()));
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn request_body_is_loaded_and_replaced_with_fixed_length() {
    let (origin, _) = spawn_echo_origin().await;
    let mut pipeline = EditingPipeline::new();
    pipeline.wants_request_body = true;
    pipeline.on_request = Box::new(|mut ctx| {
        let body = ctx.message.body.take().unwrap_or_default();
        let upper = String::from_utf8_lossy(&body).to_uppercase();
        ctx.message.body = Some(format!("{upper} WORLD").into_bytes());
        proceed(ctx)
    });
    let pipeline = Arc::new(pipeline);
    let (engine, events, cmd_tx) = engine_with(pipeline.clone()).await;

    let reply = send(&engine, "POST", &format!("http://{origin}/p"), "hello").await;
    assert!(reply.body.ends_with("\n\nHELLO WORLD"), "{}", reply.body);
    assert!(
        reply
            .body
            .to_ascii_lowercase()
            .contains("content-length: 11"),
        "{}",
        reply.body
    );
    assert_eq!(
        pipeline.seen()[0].message.body.as_deref(),
        Some(&b"hello"[..])
    );
    wait_completed(&events).await;
    let client_body = wait_event(&events, |e| match e {
        ProxyEvent::RequestEdited { client_body, .. } => Some(client_body.clone()),
        _ => None,
    })
    .await;
    let ClientBody::Stored(client_body) = client_body else {
        panic!("the client's body is kept for Repeat, got {client_body:?}");
    };
    assert_eq!(
        client_body.size, 5,
        "the original \"hello\", not the edited body"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn response_status_headers_and_body_edits_reach_client() {
    let (origin, _) = spawn_echo_origin().await;
    let mut pipeline = EditingPipeline::new();
    pipeline.wants_response_body = true;
    pipeline.on_response = Box::new(|mut ctx| {
        ctx.message.status = Some(503);
        set_header(&mut ctx.message.headers, "x-patched", "yes");
        ctx.message.body = Some(b"patched".to_vec());
        proceed(ctx)
    });
    let pipeline = Arc::new(pipeline);
    let (engine, events, cmd_tx) = engine_with(pipeline.clone()).await;

    let url = format!("http://{origin}/r");
    let reply = send(&engine, "GET", &url, "").await;
    assert_eq!(reply.status, 503);
    assert_eq!(reply.body, "patched");
    let head = reply.head.to_ascii_lowercase();
    assert!(
        head.contains("x-patched: yes") && head.contains("content-length: 7"),
        "{head}"
    );

    // Пайплайн ответа видел тело origin и итоговый запрос.
    let response_ctx = pipeline.seen().pop().expect("response context");
    let origin_body = String::from_utf8(response_ctx.message.body.expect("body")).expect("utf8");
    assert!(origin_body.starts_with("GET /r HTTP/1.1"), "{origin_body}");
    assert_eq!(response_ctx.request.and_then(|r| r.uri), Some(url));

    // В трафике — изменённый ответ.
    let status = wait_event(&events, |e| match e {
        ProxyEvent::ResponseHeaders { status, .. } => Some(*status),
        _ => None,
    })
    .await;
    assert_eq!(status, 503);
    wait_completed(&events).await;
    assert!(has_tag(&events.snapshot(), "modified"));
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn respond_route_answers_without_origin() {
    let (origin, hits) = spawn_echo_origin().await;
    let mut pipeline = EditingPipeline::new();
    pipeline.on_request = Box::new(|ctx| InterceptOutcome::Proceed {
        message: ctx.message,
        route: RouteDecision::Respond {
            status: 201,
            headers: vec![HeaderEntry {
                name: "Content-Type".into(),
                value: "application/json".into(),
            }],
            body: br#"{"mock":true}"#.to_vec(),
        },
    });
    let (engine, events, cmd_tx) = engine_with(Arc::new(pipeline)).await;

    let reply = send(&engine, "GET", &format!("http://{origin}/m"), "").await;
    assert_eq!(
        (reply.status, reply.body.as_str()),
        (201, r#"{"mock":true}"#)
    );
    assert!(
        reply
            .head
            .to_ascii_lowercase()
            .contains("content-type: application/json")
    );
    assert_eq!(
        *hits.lock().expect("hits lock"),
        0,
        "origin must not be hit"
    );
    wait_completed(&events).await;
    assert!(has_tag(&events.snapshot(), "mocked"));
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn body_over_cap_streams_through_unloaded() {
    let (origin, _) = spawn_echo_origin().await;
    let mut pipeline = EditingPipeline::new();
    pipeline.wants_request_body = true;
    pipeline.wants_response_body = true;
    let pipeline = Arc::new(pipeline);
    let cfg = EngineConfig::new("127.0.0.1:0", 16).with_intercept_body_cap(4);
    let (engine, events, cmd_tx) = spawn_engine_cfg_pipeline(cfg, pipeline.clone()).await;

    let reply = send(
        &engine,
        "POST",
        &format!("http://{origin}/big"),
        "0123456789",
    )
    .await;
    assert_eq!(reply.status, 200);
    assert!(reply.body.ends_with("\n\n0123456789"), "{}", reply.body);
    assert!(pipeline.seen().iter().all(|c| c.message.body.is_none()));
    wait_completed(&events).await;
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

/// Origin с chunked-ответом без Content-Length (длина заранее неизвестна).
async fn spawn_chunked_origin() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = read_request(&mut sock).await;
                let resp = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
                            5\r\n01234\r\n5\r\n56789\r\n0\r\n\r\n";
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn chunked_body_over_cap_is_replayed_unchanged() {
    let origin = spawn_chunked_origin().await;
    let mut pipeline = EditingPipeline::new();
    pipeline.wants_response_body = true;
    pipeline.on_response = Box::new(|mut ctx| {
        set_header(&mut ctx.message.headers, "x-seen", "1");
        proceed(ctx)
    });
    let pipeline = Arc::new(pipeline);
    let cfg = EngineConfig::new("127.0.0.1:0", 16).with_intercept_body_cap(4);
    let (engine, events, cmd_tx) = spawn_engine_cfg_pipeline(cfg, pipeline.clone()).await;

    let reply = send(&engine, "GET", &format!("http://{origin}/c"), "").await;
    assert_eq!(reply.status, 200);
    assert!(
        reply.head.to_ascii_lowercase().contains("x-seen: 1"),
        "{}",
        reply.head
    );
    let decoded: String = reply
        .body
        .split("\r\n")
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s)
        .collect();
    assert_eq!(decoded, "0123456789", "raw: {:?}", reply.body);
    assert!(pipeline.seen().iter().all(|c| c.message.body.is_none()));
    wait_completed(&events).await;
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn host_header_set_by_pipeline_is_kept() {
    let (origin, _) = spawn_echo_origin().await;
    let mut pipeline = EditingPipeline::new();
    pipeline.on_request = Box::new(|mut ctx| {
        set_header(&mut ctx.message.headers, "host", "virtual.test");
        proceed(ctx)
    });
    let (engine, events, cmd_tx) = engine_with(Arc::new(pipeline)).await;

    let reply = send(&engine, "GET", &format!("http://{origin}/v"), "").await;
    assert!(
        reply
            .body
            .to_ascii_lowercase()
            .contains("host: virtual.test"),
        "{}",
        reply.body
    );
    wait_completed(&events).await;
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
