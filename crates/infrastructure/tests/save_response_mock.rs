//! «Save Response & Map Local» на реальных адаптерах: обмен в памяти,
//! gzip-тело в BodyStore, распаковка RustBodyDecoder, файл на диске.

use std::io::Write as _;
use std::sync::Arc;

use application::use_cases::rules::save_response_mock::SaveResponseMockUseCase;
use chrono::Utc;
use domain::entities::ExchangeStatus;
use domain::entities::body::BodyDescriptor;
use domain::entities::exchange::{HttpExchange, HttpRequest, HttpResponse};
use domain::repositories::{BodyRepository, ExchangeRepository};
use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
use domain::value_objects::timing::TimingPhases;
use domain::value_objects::{BodyId, ConnectionId, Host};
use infrastructure::repositories::{
    DirectoryMockFileStore, InMemoryExchangeRepository, MemorySpillBodyStore,
};
use infrastructure::services::RustBodyDecoder;

const BODY: &[u8] = br#"{"users":[{"id":1}]}"#;

struct Fixture {
    exchanges: Arc<InMemoryExchangeRepository>,
    bodies: Arc<MemorySpillBodyStore>,
    use_case: SaveResponseMockUseCase,
    mocks: tempfile::TempDir,
    _cache: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let mocks = tempfile::tempdir().expect("mocks");
    let cache = tempfile::tempdir().expect("cache");
    let exchanges = Arc::new(InMemoryExchangeRepository::new(16));
    let bodies = Arc::new(MemorySpillBodyStore::new(
        1 << 20,
        1 << 20,
        cache.path().to_path_buf(),
    ));
    let use_case = SaveResponseMockUseCase::new(
        exchanges.clone(),
        bodies.clone(),
        Arc::new(RustBodyDecoder::new()),
        Arc::new(DirectoryMockFileStore::new(mocks.path().to_path_buf())),
    );
    Fixture {
        exchanges,
        bodies,
        use_case,
        mocks,
        _cache: cache,
    }
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).expect("gzip");
    encoder.finish().expect("gzip")
}

/// Обмен `GET https://api.test/v1/users?page=2` с ответом `headers`.
async fn record(
    fx: &Fixture,
    headers: Vec<HeaderEntry>,
    wire: Option<Vec<u8>>,
    encoding: Option<&str>,
    status: ExchangeStatus,
) -> domain::value_objects::ExchangeId {
    let exchange = HttpExchange::new_pending(
        ConnectionId::new(),
        Host::new("api.test").expect("host"),
        HttpRequest {
            method: Method::new("GET"),
            uri: "https://api.test/v1/users?page=2".into(),
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
    let raw_size = wire.as_ref().map_or(0, |w| w.len() as u64);
    let body = match wire {
        Some(bytes) => Some(
            fx.bodies
                .put(
                    BodyId::new(),
                    bytes,
                    BodyDescriptor {
                        size: raw_size,
                        content_type: Some("application/json".into()),
                        encoding: encoding.map(Into::into),
                    },
                )
                .await
                .expect("put"),
        ),
        None => None,
    };
    let response = HttpResponse {
        status: 200,
        reason: None,
        headers,
        body,
        raw_size,
        version: HttpVersionVo::Http11,
    };
    fx.exchanges
        .update_response(&id, response, TimingPhases::default())
        .await
        .expect("response");
    fx.exchanges
        .update_status(&id, status)
        .await
        .expect("status");
    id
}

#[tokio::test]
async fn saves_decoded_body_and_prefills_response() {
    let fx = fixture();
    let headers = vec![
        HeaderEntry::new("Content-Type", "application/json"),
        HeaderEntry::new("Content-Encoding", "gzip"),
        HeaderEntry::new("Access-Control-Allow-Origin", "*"),
    ];
    let id = record(
        &fx,
        headers,
        Some(gzip(BODY)),
        Some("gzip"),
        ExchangeStatus::Completed,
    )
    .await;

    let saved = fx.use_case.execute(&id).await.expect("saved");

    assert_eq!(
        saved.local_path,
        fx.mocks.path().join("api.test/v1/users.json")
    );
    assert_eq!(std::fs::read(&saved.local_path).expect("read"), BODY);
    assert_eq!(saved.response.status, 200);
    assert_eq!(
        saved.response.content_type.as_deref(),
        Some("application/json")
    );
    assert_eq!(
        saved.response.headers,
        vec![HeaderEntry::new("Access-Control-Allow-Origin", "*")]
    );

    let again = fx.use_case.execute(&id).await.expect("saved again");
    assert_eq!(
        again.local_path,
        fx.mocks.path().join("api.test/v1/users-2.json"),
        "an existing (maybe edited) mock is never overwritten"
    );
}

#[tokio::test]
async fn refuses_bodies_in_an_unsupported_encoding() {
    let fx = fixture();
    let headers = vec![HeaderEntry::new("Content-Encoding", "dcb")];
    let id = record(
        &fx,
        headers,
        Some(b"\x00opaque".to_vec()),
        Some("dcb"),
        ExchangeStatus::Completed,
    )
    .await;

    let error = fx.use_case.execute(&id).await.expect_err("refused");

    assert!(error.to_string().contains("dcb"), "{error}");
    assert!(
        std::fs::read_dir(fx.mocks.path())
            .expect("dir")
            .next()
            .is_none(),
        "a still-encoded body is not saved as if it were decoded"
    );
}

#[tokio::test]
async fn refuses_incomplete_or_uncaptured_responses() {
    let fx = fixture();
    let loading = record(&fx, Vec::new(), None, None, ExchangeStatus::Pending).await;
    assert!(fx.use_case.execute(&loading).await.is_err());

    let completed = record(
        &fx,
        Vec::new(),
        Some(BODY.to_vec()),
        None,
        ExchangeStatus::Completed,
    )
    .await;
    let mut exchange = fx
        .exchanges
        .get(&completed)
        .await
        .expect("get")
        .expect("some");
    let mut response = exchange.response.take().expect("response");
    response.body = None;
    fx.exchanges
        .update_response(&completed, response, TimingPhases::default())
        .await
        .expect("drop body");
    assert!(
        fx.use_case.execute(&completed).await.is_err(),
        "body bytes went over the wire but were not recorded"
    );
    assert!(
        std::fs::read_dir(fx.mocks.path())
            .expect("dir")
            .next()
            .is_none(),
        "nothing is written on refusal"
    );
}
