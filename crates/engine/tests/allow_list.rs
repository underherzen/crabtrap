//! Allow List: трафик хостов не из списка проксируется мимо — без записи,
//! правил, расшифровки; хосты из списка — как обычно.

use std::sync::Arc;
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::proxy_config::EngineCommand;
use domain::entities::rule::{AllowEntry, BlockAction, BlockRule, MapRemoteRule, RuleSet};
use domain::value_objects::{RuleId, TrafficMatch};
use infrastructure::services::RulesInterceptionPipeline;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;
use common::*;

/// Origin: на любой запрос — 200 «origin».
async fn spawn_origin() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\norigin",
                    )
                    .await;
            });
        }
    });
    addr
}

async fn engine_with(
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
        .expect("rules");
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, events, cmd_tx)
}

/// Allow List только для `allowed`, плюс Map Remote /mapped → мёртвый порт
/// (правило применилось — 502) и Block на /blocked.
fn rules(allowed: &str) -> RuleSet {
    RuleSet {
        allow_enabled: true,
        allow_list: vec![AllowEntry::new(allowed)],
        map_remote: vec![MapRemoteRule {
            id: RuleId::new(),
            r#match: TrafficMatch {
                host_glob: Some("127.0.0.1".into()),
                path_glob: Some("/mapped".into()),
                ..Default::default()
            },
            remote: "http://127.0.0.1:9".into(),
            preserve_host: false,
            enabled: true,
        }],
        blocks: vec![BlockRule {
            id: RuleId::new(),
            r#match: TrafficMatch {
                host_glob: Some("127.0.0.1".into()),
                path_glob: Some("/blocked".into()),
                ..Default::default()
            },
            action: BlockAction::Status(403),
            enabled: true,
        }],
        ..RuleSet::default()
    }
}

async fn get(engine: &str, origin: &std::net::SocketAddr, path: &str) -> String {
    let mut client = TcpStream::connect(engine).await.expect("connect");
    let req = format!(
        "GET http://{origin}{path} HTTP/1.1\r\nHost: {origin}\r\nConnection: close\r\n\r\n"
    );
    client.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    client.read_to_end(&mut raw).await.expect("read");
    String::from_utf8_lossy(&raw).to_string()
}

fn started(events: &Arc<RecordingPublisher>) -> usize {
    events
        .snapshot()
        .iter()
        .filter(|e| matches!(e, ProxyEvent::ExchangeStarted { .. }))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn hosts_outside_the_list_are_proxied_unrecorded_and_untouched() {
    let origin = spawn_origin().await;
    let (engine, events, cmd_tx) = engine_with(rules("only.example.test")).await;

    let mapped = get(&engine, &origin, "/mapped").await;
    assert!(
        mapped.starts_with("HTTP/1.1 200"),
        "rules don't apply: {mapped}"
    );
    assert!(mapped.ends_with("origin"), "{mapped}");

    let blocked = get(&engine, &origin, "/blocked").await;
    assert!(
        blocked.starts_with("HTTP/1.1 403"),
        "Block List applies to every host: {blocked}"
    );

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(started(&events), 0, "nothing is recorded");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn hosts_in_the_list_are_recorded_and_ruled() {
    let origin = spawn_origin().await;
    let (engine, events, cmd_tx) = engine_with(rules("127.0.0.1")).await;

    let reply = get(&engine, &origin, "/blocked").await;

    assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
    wait_event(&events, |e| {
        matches!(e, ProxyEvent::ExchangeStarted { .. }).then_some(())
    })
    .await;
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_to_a_host_outside_the_list_is_an_unrecorded_tunnel() {
    let origin = spawn_origin().await;
    let (engine, events, cmd_tx) = engine_with(rules("only.example.test")).await;

    let mut client = TcpStream::connect(&engine).await.expect("connect");
    let connect = format!("CONNECT {origin} HTTP/1.1\r\nHost: {origin}\r\n\r\n");
    client.write_all(connect.as_bytes()).await.expect("write");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    client
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .expect("tunnel write");
    let mut raw = Vec::new();
    client.read_to_end(&mut raw).await.expect("tunnel read");
    assert!(String::from_utf8_lossy(&raw).ends_with("origin"));

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(started(&events), 0, "the tunnel is not recorded");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
