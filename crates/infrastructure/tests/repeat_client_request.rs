//! Repeat / Edit & Resend повторяют запрос клиента целиком — голову и
//! тело, — даже если пайплайн (Rewrite, скрипт) их изменил: правила
//! применятся заново, а не поверх уже применённых.

use std::sync::{Arc, Mutex};

use application::dto::compose::{ComposedRequestDto, SendResultDto};
use application::services::DirectHttpService;
use application::use_cases::compose::RepeatExchangeUseCase;
use async_trait::async_trait;
use chrono::Utc;
use domain::entities::ClientBody;
use domain::entities::body::BodyDescriptor;
use domain::entities::exchange::{HttpExchange, HttpRequest, RequestHead};
use domain::errors::DomainError;
use domain::repositories::{BodyRepository, ExchangeRepository};
use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
use domain::value_objects::{BodyId, ConnectionId, ExchangeId, Host};
use infrastructure::repositories::{InMemoryExchangeRepository, MemorySpillBodyStore};

#[derive(Default)]
struct RecordingDirect {
    sent: Mutex<Vec<ComposedRequestDto>>,
}

#[async_trait]
impl DirectHttpService for RecordingDirect {
    async fn send(&self, request: ComposedRequestDto) -> Result<SendResultDto, DomainError> {
        self.sent.lock().expect("lock").push(request);
        Ok(SendResultDto {
            status: 200,
            headers: Vec::new(),
            body: None,
            timing: Default::default(),
            error: None,
        })
    }
}

struct Fixture {
    exchanges: Arc<InMemoryExchangeRepository>,
    bodies: Arc<MemorySpillBodyStore>,
    direct: Arc<RecordingDirect>,
    repeat: RepeatExchangeUseCase,
    _cache: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let cache = tempfile::tempdir().expect("cache");
    let exchanges = Arc::new(InMemoryExchangeRepository::new(8));
    let bodies = Arc::new(MemorySpillBodyStore::new(
        1 << 20,
        1 << 20,
        cache.path().to_path_buf(),
    ));
    let direct = Arc::new(RecordingDirect::default());
    let repeat = RepeatExchangeUseCase::new(exchanges.clone(), bodies.clone(), direct.clone());
    Fixture {
        exchanges,
        bodies,
        direct,
        repeat,
        _cache: cache,
    }
}

async fn put(fx: &Fixture, bytes: &[u8]) -> domain::entities::BodyRef {
    fx.bodies
        .put(
            BodyId::new(),
            bytes.to_vec(),
            BodyDescriptor {
                size: bytes.len() as u64,
                content_type: None,
                encoding: None,
            },
        )
        .await
        .expect("put")
}

/// Клиент прислал `{"v":1}` на /original; пайплайн отправил `{"v":2}` на
/// /rewritten (если `client_body` — тело клиента сохранено отдельно).
async fn edited_exchange(fx: &Fixture, keep_client_body: bool) -> ExchangeId {
    let exchange = HttpExchange::new_pending(
        ConnectionId::new(),
        Host::new("api.test").expect("host"),
        HttpRequest {
            method: Method::new("POST"),
            uri: "https://api.test/original".into(),
            version: HttpVersionVo::Http11,
            headers: vec![HeaderEntry::new("X-Client", "1")],
            body: None,
            raw_size: 0,
            origin: Default::default(),
        },
        Protocol::Http1,
        Utc::now(),
    );
    let id = exchange.id;
    fx.exchanges.append(exchange).await.expect("append");
    let client_body = match keep_client_body {
        true => ClientBody::Stored(put(fx, br#"{"v":1}"#).await),
        false => ClientBody::Unchanged,
    };
    fx.exchanges
        .update_request_head(
            &id,
            RequestHead {
                method: Method::new("POST"),
                uri: "https://api.test/rewritten".into(),
                headers: vec![HeaderEntry::new("X-Rewritten", "1")],
                body: None,
                body_lost: false,
            },
            client_body,
        )
        .await
        .expect("edit");
    let sent = put(fx, br#"{"v":2}"#).await;
    fx.exchanges
        .update_request_body(&id, Some(sent), 7)
        .await
        .expect("body");
    id
}

#[tokio::test]
async fn repeat_sends_the_client_head_and_body_after_an_edit() {
    let fx = fixture();
    let id = edited_exchange(&fx, true).await;

    fx.repeat.repeat(&id).await.expect("repeat");

    let sent = fx.direct.sent.lock().expect("lock").remove(0);
    assert_eq!(sent.url, "https://api.test/original");
    assert_eq!(sent.headers[0].name, "X-Client");
    assert_eq!(sent.body.as_deref(), Some(&br#"{"v":1}"#[..]));
}

#[tokio::test]
async fn edit_and_resend_starts_from_the_client_body() {
    let fx = fixture();
    let id = edited_exchange(&fx, true).await;

    let form = fx.repeat.edit_form(&id).await.expect("form");

    assert_eq!(form.body, r#"{"v":1}"#);
}

#[tokio::test]
async fn head_only_edit_reuses_the_recorded_body() {
    let fx = fixture();
    let id = edited_exchange(&fx, false).await;

    fx.repeat.repeat(&id).await.expect("repeat");

    let sent = fx.direct.sent.lock().expect("lock").remove(0);
    assert_eq!(sent.url, "https://api.test/original");
    assert_eq!(
        sent.body.as_deref(),
        Some(&br#"{"v":2}"#[..]),
        "the pipeline didn't touch the body — the recorded one is the client's"
    );
}

#[tokio::test]
async fn evicted_client_body_blocks_repeat_but_not_edit_and_resend() {
    let fx = fixture();
    let id = edited_exchange(&fx, true).await;
    let exchange = fx.exchanges.get(&id).await.expect("get").expect("exchange");
    let client = exchange
        .client_request
        .and_then(|c| c.body)
        .expect("client body");
    fx.bodies.delete(client.id).await.expect("evict");

    assert!(
        fx.repeat.repeat(&id).await.is_err(),
        "never resend the edited body as the client's"
    );
    let form = fx.repeat.edit_form(&id).await.expect("form");
    assert_eq!(
        form.body, r#"{"v":2}"#,
        "falls back to the sent body for manual editing"
    );
}

#[tokio::test]
async fn lost_client_body_blocks_repeat() {
    let fx = fixture();
    let exchange = HttpExchange::new_pending(
        ConnectionId::new(),
        Host::new("api.test").expect("host"),
        HttpRequest {
            method: Method::new("POST"),
            uri: "https://api.test/original".into(),
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
    let head = RequestHead {
        method: Method::new("POST"),
        uri: "https://api.test/original".into(),
        headers: Vec::new(),
        body: None,
        body_lost: false,
    };
    fx.exchanges
        .update_request_head(&id, head, ClientBody::Lost)
        .await
        .expect("edit");
    let sent = put(&fx, br#"{"edited":true}"#).await;
    fx.exchanges
        .update_request_body(&id, Some(sent), 15)
        .await
        .expect("body");

    assert!(
        fx.repeat.repeat(&id).await.is_err(),
        "the edited body must not be sent as the client's"
    );
    let form = fx.repeat.edit_form(&id).await.expect("form");
    assert_eq!(form.body, r#"{"edited":true}"#);
}
