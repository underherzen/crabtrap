//! Экспорт на реальных адаптерах: cURL, fetch и HAR из обменов в памяти,
//! gzip-тело ответа распаковано, файл записан на диск.

use std::io::Write as _;
use std::sync::Arc;

use application::use_cases::export::{CopyFormat, ExportTrafficUseCase, HarScope};
use chrono::{Duration, Utc};
use domain::entities::ExchangeStatus;
use domain::entities::body::BodyDescriptor;
use domain::entities::exchange::{HttpExchange, HttpRequest, HttpResponse};
use domain::repositories::{BodyRepository, ExchangeRepository};
use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
use domain::value_objects::timing::TimingPhases;
use domain::value_objects::{BodyId, ConnectionId, ExchangeId, Host};
use infrastructure::repositories::{
    FsExportFileWriter, InMemoryExchangeRepository, MemorySpillBodyStore,
};
use infrastructure::services::RustBodyDecoder;

struct Fixture {
    exchanges: Arc<InMemoryExchangeRepository>,
    bodies: Arc<MemorySpillBodyStore>,
    use_case: ExportTrafficUseCase,
    out: tempfile::TempDir,
    _cache: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let out = tempfile::tempdir().expect("out");
    let cache = tempfile::tempdir().expect("cache");
    let exchanges = Arc::new(InMemoryExchangeRepository::new(16));
    let bodies = Arc::new(MemorySpillBodyStore::new(
        1 << 20,
        1 << 20,
        cache.path().to_path_buf(),
    ));
    let use_case = ExportTrafficUseCase::new(
        exchanges.clone(),
        bodies.clone(),
        Arc::new(RustBodyDecoder::new()),
        Arc::new(FsExportFileWriter),
    );
    Fixture {
        exchanges,
        bodies,
        use_case,
        out,
        _cache: cache,
    }
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).expect("gzip");
    encoder.finish().expect("gzip")
}

async fn put(fx: &Fixture, bytes: &[u8], encoding: Option<&str>) -> domain::entities::BodyRef {
    fx.bodies
        .put(
            BodyId::new(),
            bytes.to_vec(),
            BodyDescriptor {
                size: bytes.len() as u64,
                content_type: None,
                encoding: encoding.map(Into::into),
            },
        )
        .await
        .expect("put")
}

/// POST /items с JSON-телом и gzip-ответом; `respond = false` — без ответа.
async fn record(fx: &Fixture, path: &str, offset_ms: i64, respond: bool) -> ExchangeId {
    let request_body = put(fx, br#"{"name":"crab"}"#, None).await;
    let mut exchange = HttpExchange::new_pending(
        ConnectionId::new(),
        Host::new("api.test").expect("host"),
        HttpRequest {
            method: Method::new("POST"),
            uri: format!("https://api.test{path}"),
            version: HttpVersionVo::Http11,
            headers: vec![
                HeaderEntry::new("Content-Type", "application/json"),
                HeaderEntry::new("Content-Length", "15"),
            ],
            body: Some(request_body),
            raw_size: 15,
            origin: Default::default(),
        },
        Protocol::Http1,
        Utc::now() + Duration::milliseconds(offset_ms),
    );
    let id = exchange.id;
    exchange.response = None;
    fx.exchanges.append(exchange).await.expect("append");
    if respond {
        let wire = gzip(br#"{"id":7}"#);
        let body = put(fx, &wire, Some("gzip")).await;
        let response = HttpResponse {
            status: 201,
            reason: Some("Created".into()),
            headers: vec![
                HeaderEntry::new("Content-Type", "application/json"),
                HeaderEntry::new("Content-Encoding", "gzip"),
            ],
            body: Some(body),
            raw_size: wire.len() as u64,
            version: HttpVersionVo::Http11,
        };
        fx.exchanges
            .update_response(&id, response, TimingPhases::default())
            .await
            .expect("response");
        fx.exchanges
            .update_status(&id, ExchangeStatus::Completed)
            .await
            .expect("status");
    }
    id
}

#[tokio::test]
async fn curl_repeats_the_sent_request_with_its_body() {
    let fx = fixture();
    let id = record(&fx, "/items?draft=1", 0, true).await;

    let curl = fx
        .use_case
        .copy_as(&id, CopyFormat::Curl)
        .await
        .expect("curl");

    assert!(curl.contains("-X 'POST'"), "{curl}");
    assert!(curl.contains("'https://api.test/items?draft=1'"), "{curl}");
    assert!(
        curl.contains("-H 'Content-Type: application/json'"),
        "{curl}"
    );
    assert!(!curl.contains("Content-Length"), "{curl}");
    assert!(curl.ends_with(r#"--data-raw '{"name":"crab"}'"#), "{curl}");
}

#[tokio::test]
async fn fetch_variants_carry_the_sent_body() {
    let fx = fixture();
    let id = record(&fx, "/items", 0, true).await;

    let browser = fx
        .use_case
        .copy_as(&id, CopyFormat::Fetch)
        .await
        .expect("fetch");
    let node = fx
        .use_case
        .copy_as(&id, CopyFormat::FetchNodeJs)
        .await
        .expect("fetch node");

    assert!(
        browser.starts_with(r#"fetch("https://api.test/items", {"#),
        "{browser}"
    );
    assert!(
        browser.contains(r#""body": "{\"name\":\"crab\"}""#),
        "{browser}"
    );
    assert!(browser.contains(r#""method": "POST""#), "{browser}");
    assert!(browser.contains(r#""mode": "cors""#), "{browser}");
    assert!(!browser.contains("Content-Length"), "{browser}");
    assert!(!node.contains("\"mode\""), "{node}");
    assert!(
        node.contains(r#""Content-Type": "application/json""#),
        "{node}"
    );
}

#[tokio::test]
async fn session_har_is_chronological_decoded_and_skips_pending() {
    let fx = fixture();
    record(&fx, "/second", 10, true).await;
    record(&fx, "/first", 0, true).await;
    record(&fx, "/pending", 20, false).await;
    let path = fx.out.path().join("session.har");

    let count = fx
        .use_case
        .har(HarScope::Session, true, &path)
        .await
        .expect("export");

    assert_eq!(count, 2);
    let har: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    let entries = har["log"]["entries"].as_array().expect("entries");
    assert_eq!(entries[0]["request"]["url"], "https://api.test/first");
    assert_eq!(entries[1]["request"]["url"], "https://api.test/second");
    assert_eq!(entries[0]["response"]["content"]["text"], r#"{"id":7}"#);
    assert_eq!(
        entries[0]["request"]["postData"]["text"],
        r#"{"name":"crab"}"#
    );
}

#[tokio::test]
async fn har_without_bodies_and_single_exchange() {
    let fx = fixture();
    let id = record(&fx, "/one", 0, true).await;
    record(&fx, "/other", 5, true).await;
    let path = fx.out.path().join("one.har");

    let count = fx
        .use_case
        .har(HarScope::Exchange(id), false, &path)
        .await
        .expect("export");

    assert_eq!(count, 1);
    let har: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    let entry = &har["log"]["entries"][0];
    assert_eq!(entry["request"]["url"], "https://api.test/one");
    assert!(entry["response"]["content"].get("text").is_none());
    assert!(entry["request"].get("postData").is_none());

    let pending = record(&fx, "/pending", 9, false).await;
    assert!(
        fx.use_case
            .har(
                HarScope::Exchange(pending),
                true,
                &fx.out.path().join("p.har")
            )
            .await
            .is_err(),
        "nothing to export"
    );
}
