//! Mirror на реальных адаптерах: ответ хоста из списка (gzip) пишется
//! распакованным в `<папка>/<host>/<path>`; хост вне списка, выключенный
//! инструмент и незавершённый обмен — не пишутся.

use std::io::Write as _;
use std::sync::Arc;

use application::use_cases::mirror::MirrorResponseUseCase;
use chrono::Utc;
use domain::entities::ExchangeStatus;
use domain::entities::body::BodyDescriptor;
use domain::entities::exchange::{HttpExchange, HttpRequest, HttpResponse};
use domain::entities::rule::{MirrorEntry, RuleSet};
use domain::repositories::{BodyRepository, ExchangeRepository, RuleRepository};
use domain::value_objects::http_vo::{HttpVersionVo, Method, Protocol};
use domain::value_objects::timing::TimingPhases;
use domain::value_objects::{BodyId, ConnectionId, ExchangeId, Host};
use infrastructure::repositories::{
    FileRuleRepository, FsMirrorFileWriter, InMemoryExchangeRepository, MemorySpillBodyStore,
};
use infrastructure::services::RustBodyDecoder;

struct Fixture {
    exchanges: Arc<InMemoryExchangeRepository>,
    bodies: Arc<MemorySpillBodyStore>,
    rules: Arc<FileRuleRepository>,
    use_case: MirrorResponseUseCase,
    mirror: tempfile::TempDir,
    _state: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let state = tempfile::tempdir().expect("state");
    let mirror = tempfile::tempdir().expect("mirror");
    let exchanges = Arc::new(InMemoryExchangeRepository::new(16));
    let bodies = Arc::new(MemorySpillBodyStore::new(
        1 << 20,
        1 << 20,
        state.path().join("cache"),
    ));
    let rules = Arc::new(FileRuleRepository::new(state.path().join("rules.json")));
    rules
        .update(RuleSet {
            mirror: vec![MirrorEntry::new("*.example.test", mirror.path())],
            ..RuleSet::default()
        })
        .await
        .expect("rules");
    let use_case = MirrorResponseUseCase::new(
        exchanges.clone(),
        bodies.clone(),
        Arc::new(RustBodyDecoder::new()),
        rules.clone(),
        Arc::new(FsMirrorFileWriter),
    );
    Fixture {
        exchanges,
        bodies,
        rules,
        use_case,
        mirror,
        _state: state,
    }
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).expect("gzip");
    encoder.finish().expect("gzip")
}

/// GET `uri` с gzip-ответом `body`; `complete` — завершён ли обмен.
async fn record(fx: &Fixture, uri: &str, body: &[u8], complete: bool) -> ExchangeId {
    record_with(fx, "GET", uri, 200, body, complete).await
}

async fn record_with(
    fx: &Fixture,
    method: &str,
    uri: &str,
    status: u16,
    body: &[u8],
    complete: bool,
) -> ExchangeId {
    let url = url::Url::parse(uri).expect("url");
    let exchange = HttpExchange::new_pending(
        ConnectionId::new(),
        Host::new(url.host_str().expect("host")).expect("host"),
        HttpRequest {
            method: Method::new(method),
            uri: uri.into(),
            version: HttpVersionVo::Http11,
            headers: Vec::new(),
            body: None,
            raw_size: 0,
            origin: Default::default(),
        },
        Protocol::Http1,
        Utc::now(),
    );
    let id = exchange.id;
    fx.exchanges.append(exchange).await.expect("append");
    let wire = gzip(body);
    let body_ref = fx
        .bodies
        .put(
            BodyId::new(),
            wire.clone(),
            BodyDescriptor {
                size: wire.len() as u64,
                content_type: Some("application/json".into()),
                encoding: Some("gzip".into()),
            },
        )
        .await
        .expect("put");
    fx.exchanges
        .update_response(
            &id,
            HttpResponse {
                status,
                reason: None,
                headers: Vec::new(),
                body: Some(body_ref),
                raw_size: wire.len() as u64,
                version: HttpVersionVo::Http11,
            },
            TimingPhases::default(),
        )
        .await
        .expect("response");
    if complete {
        fx.exchanges
            .update_status(&id, ExchangeStatus::Completed)
            .await
            .expect("status");
    }
    id
}

#[tokio::test]
async fn responses_of_listed_hosts_are_written_decompressed() {
    let fx = fixture().await;
    let users = record(
        &fx,
        "https://api.example.test/v1/users?page=2",
        b"[1,2]",
        true,
    )
    .await;
    let root = record(&fx, "https://api.example.test/", b"<html>", true).await;

    let written = fx.use_case.mirror(&users).await.expect("mirror");
    fx.use_case.mirror(&root).await.expect("mirror");

    let users_file = fx.mirror.path().join("api.example.test/v1/users");
    assert_eq!(written.as_deref(), Some(users_file.as_path()));
    assert_eq!(std::fs::read(&users_file).expect("read"), b"[1,2]");
    let index = fx.mirror.path().join("api.example.test/index.html");
    assert_eq!(std::fs::read(index).expect("read"), b"<html>");
}

#[tokio::test]
async fn other_hosts_pending_exchanges_and_a_disabled_tool_are_skipped() {
    let fx = fixture().await;
    let other = record(&fx, "https://elsewhere.test/x", b"x", true).await;
    let pending = record(&fx, "https://api.example.test/pending", b"x", false).await;
    let listed = record(&fx, "https://api.example.test/later", b"x", true).await;

    assert_eq!(fx.use_case.mirror(&other).await.expect("other"), None);
    assert_eq!(fx.use_case.mirror(&pending).await.expect("pending"), None);
    let rules = fx.rules.get().await.expect("rules");
    fx.rules
        .update(RuleSet {
            mirror_enabled: false,
            ..rules
        })
        .await
        .expect("off");
    assert_eq!(fx.use_case.mirror(&listed).await.expect("off"), None);
    assert_eq!(
        std::fs::read_dir(fx.mirror.path()).expect("dir").count(),
        0,
        "nothing written"
    );
}

#[tokio::test]
async fn errors_and_non_get_responses_keep_the_good_copy() {
    let fx = fixture().await;
    let good = record(&fx, "https://api.example.test/items", b"[1]", true).await;
    let missing = record_with(
        &fx,
        "GET",
        "https://api.example.test/items",
        404,
        b"{}",
        true,
    )
    .await;
    let posted = record_with(
        &fx,
        "POST",
        "https://api.example.test/items",
        201,
        b"{}",
        true,
    )
    .await;
    let partial = record_with(
        &fx,
        "GET",
        "https://api.example.test/items",
        206,
        b"[",
        true,
    )
    .await;

    fx.use_case.mirror(&good).await.expect("good");
    assert_eq!(fx.use_case.mirror(&missing).await.expect("404"), None);
    assert_eq!(fx.use_case.mirror(&posted).await.expect("post"), None);
    assert_eq!(fx.use_case.mirror(&partial).await.expect("206"), None);

    let file = fx.mirror.path().join("api.example.test/items");
    assert_eq!(std::fs::read(file).expect("read"), b"[1]");
}
