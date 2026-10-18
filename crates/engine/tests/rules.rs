//! Правила Phase 6 в движке: Block List (код / обрыв), Map Local (файл,
//! отсутствующий файл), Map Remote (другой origin, Host), теги обменов.
//! Правила доставляются как в приложении — командой UpdateRules.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::proxy_config::EngineCommand;
use domain::entities::rule::{BlockAction, BlockRule, MapLocalRule, MapRemoteRule, RuleSet};
use domain::value_objects::{RuleId, TrafficMatch};
use infrastructure::services::RulesInterceptionPipeline;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;
use common::*;

/// Origin, отвечающий своим именем и запоминающий Host входящих запросов.
async fn spawn_named_origin(
    name: &'static str,
) -> (std::net::SocketAddr, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let hosts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = hosts.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                if let Some(host) = head.lines().find_map(|l| {
                    l.strip_prefix("host: ")
                        .or_else(|| l.strip_prefix("Host: "))
                }) {
                    seen.lock().expect("lock").push(host.trim().to_string());
                }
                let body = format!("from {name}");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (addr, hosts)
}

fn match_host_path(host: &str, path: Option<&str>) -> TrafficMatch {
    TrafficMatch {
        host_glob: Some(host.into()),
        path_glob: path.map(str::to_string),
        ..Default::default()
    }
}

async fn engine_with_rules(
    rules: RuleSet,
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<EngineCommand>,
) {
    let pipeline = Arc::new(RulesInterceptionPipeline::new(RuleSet::default()));
    let (addr, events, cmd_tx) = spawn_engine_with_pipeline(pipeline).await;
    cmd_tx
        .send(EngineCommand::UpdateRules { snapshot: rules })
        .await
        .expect("send rules");
    // Команда применяется циклом движка асинхронно.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, events, cmd_tx)
}

/// GET через прокси; возвращает (статус, тело) или None, если соединение
/// закрыто без ответа.
async fn proxy_get(engine_addr: &str, url: &str) -> Option<(u16, String)> {
    let mut client = TcpStream::connect(engine_addr).await.expect("connect");
    let authority = url.split('/').nth(2).unwrap_or_default();
    let req = format!("GET {url} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    client.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut raw)).await;
    if raw.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text.split_whitespace().nth(1)?.parse().ok()?;
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    Some((status, body))
}

async fn tag_of(events: &Arc<RecordingPublisher>) -> String {
    wait_event(events, |e| match e {
        ProxyEvent::ExchangeTagged { tag, .. } => Some(tag.clone()),
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn block_rule_answers_status_without_touching_origin() {
    let (origin, hosts) = spawn_named_origin("origin").await;
    let rules = RuleSet {
        blocks: vec![BlockRule {
            id: RuleId::new(),
            r#match: match_host_path("127.0.0.1", Some("/ads*")),
            action: BlockAction::Status(451),
            enabled: true,
        }],
        ..Default::default()
    };
    let (engine, events, cmd_tx) = engine_with_rules(rules).await;

    let (status, _) = proxy_get(&engine, &format!("http://{origin}/ads/banner"))
        .await
        .expect("response");
    assert_eq!(status, 451);
    assert!(
        hosts.lock().expect("lock").is_empty(),
        "origin must not be hit"
    );
    assert_eq!(tag_of(&events).await, "blocked");
    wait_event(&events, |e| {
        matches!(e, ProxyEvent::ExchangeCompleted { .. }).then_some(())
    })
    .await;

    let (status, body) = proxy_get(&engine, &format!("http://{origin}/news"))
        .await
        .expect("response");
    assert_eq!(
        (status, body.as_str()),
        (200, "from origin"),
        "other paths pass"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn block_rule_with_close_drops_connection() {
    let (origin, _) = spawn_named_origin("origin").await;
    let rules = RuleSet {
        blocks: vec![BlockRule {
            id: RuleId::new(),
            r#match: match_host_path("127.0.0.1", None),
            action: BlockAction::Close,
            enabled: true,
        }],
        ..Default::default()
    };
    let (engine, _events, cmd_tx) = engine_with_rules(rules).await;
    assert!(
        proxy_get(&engine, &format!("http://{origin}/x"))
            .await
            .is_none(),
        "connection must be closed without a response"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn map_local_serves_file_with_content_type_and_404_when_missing() {
    let (origin, hosts) = spawn_named_origin("origin").await;
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("users.json");
    std::fs::write(&file, br#"[{"id":1}]"#).expect("write");
    let rule = |path: PathBuf, glob: &str| MapLocalRule {
        id: RuleId::new(),
        r#match: match_host_path("127.0.0.1", Some(glob)),
        local_path: path,
        status: 200,
        content_type: None,
        enabled: true,
    };
    let rules = RuleSet {
        map_local: vec![
            rule(file.clone(), "/api/users*"),
            rule(dir.path().join("missing.json"), "/api/missing*"),
        ],
        ..Default::default()
    };
    let (engine, events, cmd_tx) = engine_with_rules(rules).await;

    let mut client = TcpStream::connect(&engine).await.expect("connect");
    let req = format!(
        "GET http://{origin}/api/users HTTP/1.1\r\nHost: {origin}\r\nConnection: close\r\n\r\n"
    );
    client.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    client.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw).to_lowercase();
    assert!(text.starts_with("http/1.1 200"), "{text}");
    assert!(text.contains("content-type: application/json"), "{text}");
    assert!(text.ends_with(r#"[{"id":1}]"#), "{text}");
    assert_eq!(tag_of(&events).await, "map-local");

    let (status, _) = proxy_get(&engine, &format!("http://{origin}/api/missing"))
        .await
        .expect("response");
    assert_eq!(status, 404);
    assert!(
        hosts.lock().expect("lock").is_empty(),
        "origin must not be hit"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

/// Сырой ответ прокси на GET (заголовки в нижнем регистре).
async fn raw_get(engine: &str, origin: &std::net::SocketAddr, path: &str) -> String {
    let mut client = TcpStream::connect(engine).await.expect("connect");
    let req = format!(
        "GET http://{origin}{path} HTTP/1.1\r\nHost: {origin}\r\nConnection: close\r\n\r\n"
    );
    client.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    client.read_to_end(&mut raw).await.expect("read");
    String::from_utf8_lossy(&raw).to_lowercase()
}

#[tokio::test(flavor = "multi_thread")]
async fn map_local_uses_rule_status_and_content_type() {
    let (origin, hosts) = spawn_named_origin("origin").await;
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("error.json");
    std::fs::write(&file, br#"{"error":"nope"}"#).expect("write");
    let rule = |glob: &str, status: u16, content_type: Option<&str>| MapLocalRule {
        id: RuleId::new(),
        r#match: match_host_path("127.0.0.1", Some(glob)),
        local_path: file.clone(),
        status,
        content_type: content_type.map(Into::into),
        enabled: true,
    };
    let rules = RuleSet {
        map_local: vec![
            rule("/fail*", 500, Some("application/problem+json")),
            rule("/empty*", 204, None),
            rule("/gone*", 404, None),
        ],
        ..Default::default()
    };
    let (engine, _events, cmd_tx) = engine_with_rules(rules).await;

    let fail = raw_get(&engine, &origin, "/fail").await;
    assert!(fail.starts_with("http/1.1 500"), "{fail}");
    assert!(
        fail.contains("content-type: application/problem+json"),
        "{fail}"
    );
    assert!(
        !fail.contains("content-type: application/json"),
        "override replaces auto: {fail}"
    );
    assert!(fail.ends_with(r#"{"error":"nope"}"#), "{fail}");

    let empty = raw_get(&engine, &origin, "/empty").await;
    assert!(empty.starts_with("http/1.1 204"), "{empty}");
    assert!(empty.ends_with("\r\n\r\n"), "204 carries no body: {empty}");

    let gone = raw_get(&engine, &origin, "/gone").await;
    assert!(gone.starts_with("http/1.1 404"), "{gone}");
    assert!(
        gone.contains("content-type: application/json"),
        "auto type by extension: {gone}"
    );
    assert!(gone.ends_with(r#"{"error":"nope"}"#), "{gone}");
    assert!(
        hosts.lock().expect("lock").is_empty(),
        "origin must not be hit"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn map_remote_sends_request_to_other_origin() {
    let (source, source_hosts) = spawn_named_origin("source").await;
    let (target, target_hosts) = spawn_named_origin("target").await;
    let rules = RuleSet {
        map_remote: vec![MapRemoteRule {
            id: RuleId::new(),
            r#match: match_host_path("127.0.0.1", Some("/v1/*")),
            remote: format!("http://{target}"),
            preserve_host: false,
            enabled: true,
        }],
        ..Default::default()
    };
    let (engine, events, cmd_tx) = engine_with_rules(rules).await;

    let (status, body) = proxy_get(&engine, &format!("http://{source}/v1/users"))
        .await
        .expect("response");
    assert_eq!((status, body.as_str()), (200, "from target"));
    assert!(source_hosts.lock().expect("lock").is_empty());
    assert_eq!(
        target_hosts.lock().expect("lock").as_slice(),
        &[target.to_string()],
        "Host rewritten to the remote authority"
    );
    assert_eq!(tag_of(&events).await, "map-remote");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn throttle_adds_latency_limits_rate_and_tags_exchange() {
    use application::dto::rules::ThrottleSpecDto;
    // Origin с телом 3000 B.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let origin = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let body = vec![b'x'; 3000];
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
            });
        }
    });
    let (engine, events, cmd_tx) = engine_with_rules(RuleSet::default()).await;
    cmd_tx
        .send(EngineCommand::UpdateThrottle {
            spec: Some(ThrottleSpecDto {
                name: "slow".into(),
                down_bps: Some(10_000),
                up_bps: None,
                latency_ms: 300,
                r#match: Some(match_host_path("127.0.0.1", None)),
            }),
        })
        .await
        .expect("send throttle");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = std::time::Instant::now();
    let (status, body) = proxy_get(&engine, &format!("http://{origin}/big"))
        .await
        .expect("response");
    let elapsed = started.elapsed();
    assert_eq!(status, 200);
    assert_eq!(body.len(), 3000, "body intact");
    // 300 мс задержки + ~250 мс на 3000 B при 10 000 B/s.
    assert!(
        elapsed >= Duration::from_millis(450),
        "not throttled: {elapsed:?}"
    );
    assert_eq!(tag_of(&events).await, "throttled");

    // Хост вне match — без задержки.
    cmd_tx
        .send(EngineCommand::UpdateThrottle {
            spec: Some(ThrottleSpecDto {
                name: "scoped".into(),
                down_bps: None,
                up_bps: None,
                latency_ms: 2_000,
                r#match: Some(match_host_path("elsewhere.example", None)),
            }),
        })
        .await
        .expect("send throttle");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = std::time::Instant::now();
    proxy_get(&engine, &format!("http://{origin}/big"))
        .await
        .expect("response");
    assert!(
        started.elapsed() < Duration::from_millis(1_500),
        "unmatched host throttled"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn map_local_directory_is_confined_and_decodes_names() {
    let (origin, _) = spawn_named_origin("origin").await;
    let outside = tempfile::tempdir().expect("outside");
    std::fs::write(outside.path().join("secret.txt"), b"TOP SECRET").expect("write");
    let dir = tempfile::tempdir().expect("mocks");
    std::fs::write(dir.path().join("my file.json"), b"{}").expect("write");
    std::fs::write(dir.path().join(".env"), b"KEY=1").expect("write");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("symlink");

    let rules = RuleSet {
        map_local: vec![MapLocalRule {
            id: RuleId::new(),
            r#match: match_host_path("127.0.0.1", None),
            local_path: dir.path().to_path_buf(),
            status: 200,
            content_type: None,
            enabled: true,
        }],
        ..Default::default()
    };
    let (engine, _events, cmd_tx) = engine_with_rules(rules).await;

    let (status, body) = proxy_get(&engine, &format!("http://{origin}/my%20file.json"))
        .await
        .expect("response");
    assert_eq!(
        (status, body.as_str()),
        (200, "{}"),
        "percent-decoded name served"
    );

    let (status, body) = proxy_get(&engine, &format!("http://{origin}/link/secret.txt"))
        .await
        .expect("response");
    assert_eq!(status, 404, "symlink escaping the directory is refused");
    assert!(!body.contains("TOP SECRET"));

    let (status, _) = proxy_get(&engine, &format!("http://{origin}/.env"))
        .await
        .expect("response");
    assert_eq!(status, 404, "dotfiles are not served");

    let (status, body) = proxy_get(&engine, &format!("http://{origin}/missing.json"))
        .await
        .expect("response");
    assert_eq!(status, 404);
    assert!(
        !body.contains(&dir.path().display().to_string()),
        "404 must not leak the local path: {body}"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_block_list_lets_requests_through() {
    let (origin, _) = spawn_named_origin("origin").await;
    let rules = RuleSet {
        blocks_enabled: false,
        blocks: vec![BlockRule {
            id: RuleId::new(),
            r#match: match_host_path("127.0.0.1", None),
            action: BlockAction::Status(403),
            enabled: true,
        }],
        ..Default::default()
    };
    let (engine, _events, cmd_tx) = engine_with_rules(rules).await;
    let (status, body) = proxy_get(&engine, &format!("http://{origin}/x"))
        .await
        .expect("response");
    assert_eq!((status, body.as_str()), (200, "from origin"));
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
