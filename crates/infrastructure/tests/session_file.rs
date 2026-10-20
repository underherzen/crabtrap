//! Save / Open Session на реальных адаптерах: трафик с телами и
//! сообщениями WebSocket сохраняется в файл и открывается в другом
//! хранилище — с новыми id, тегом `imported` и теми же байтами.

use std::sync::Arc;

use application::use_cases::sessions::{IMPORTED_TAG, SessionFileUseCase};
use chrono::Utc;
use domain::entities::ExchangeStatus;
use domain::entities::body::BodyDescriptor;
use domain::entities::exchange::{HttpExchange, HttpRequest, HttpResponse};
use domain::entities::websocket::{WebSocketMessage, WsDirection, WsOpcode};
use domain::repositories::{BodyRepository, ExchangeRepository};
use domain::value_objects::filter_query::FilterQuery;
use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
use domain::value_objects::timing::TimingPhases;
use domain::value_objects::{BodyId, ConnectionId, Host};
use infrastructure::repositories::{
    FsExportFileWriter, FsImportFileReader, InMemoryExchangeRepository, MemorySpillBodyStore,
};

struct Store {
    exchanges: Arc<InMemoryExchangeRepository>,
    bodies: Arc<MemorySpillBodyStore>,
    use_case: SessionFileUseCase,
    _cache: tempfile::TempDir,
}

fn store() -> Store {
    store_with_capacity(64)
}

fn store_with_capacity(capacity: usize) -> Store {
    let cache = tempfile::tempdir().expect("cache");
    let exchanges = Arc::new(InMemoryExchangeRepository::new(capacity));
    let bodies = Arc::new(MemorySpillBodyStore::new(
        1 << 20,
        1 << 20,
        cache.path().to_path_buf(),
    ));
    let use_case = SessionFileUseCase::new(
        exchanges.clone(),
        bodies.clone(),
        Arc::new(FsExportFileWriter),
        Arc::new(FsImportFileReader),
    );
    Store {
        exchanges,
        bodies,
        use_case,
        _cache: cache,
    }
}

async fn put(store: &Store, bytes: &[u8]) -> domain::entities::BodyRef {
    store
        .bodies
        .put(
            BodyId::new(),
            bytes.to_vec(),
            BodyDescriptor {
                size: bytes.len() as u64,
                content_type: Some("application/octet-stream".into()),
                encoding: None,
            },
        )
        .await
        .expect("put")
}

/// POST с бинарным телом и ответом; второй обмен — WebSocket с сообщением.
async fn fill(store: &Store) {
    let request_body = put(store, &[0, 1, 2, 255]).await;
    let response_body = put(store, b"{\"ok\":true}").await;
    let mut exchange = HttpExchange::new_pending(
        ConnectionId::new(),
        Host::new("api.test").expect("host"),
        HttpRequest {
            method: Method::new("POST"),
            uri: "https://api.test/upload".into(),
            version: HttpVersionVo::Http11,
            headers: vec![HeaderEntry::new("Content-Type", "application/octet-stream")],
            body: Some(request_body),
            raw_size: 4,
            origin: Default::default(),
        },
        Protocol::Http1,
        Utc::now(),
    );
    exchange.tags = vec!["map-local".into()];
    let id = exchange.id;
    store.exchanges.append(exchange).await.expect("append");
    store
        .exchanges
        .update_response(
            &id,
            HttpResponse {
                status: 200,
                reason: Some("OK".into()),
                headers: Vec::new(),
                body: Some(response_body),
                raw_size: 11,
                version: HttpVersionVo::Http11,
            },
            TimingPhases::default(),
        )
        .await
        .expect("response");
    store
        .exchanges
        .update_status(&id, ExchangeStatus::Completed)
        .await
        .expect("status");

    let socket = HttpExchange::new_pending(
        ConnectionId::new(),
        Host::new("ws.test").expect("host"),
        HttpRequest {
            method: Method::new("GET"),
            uri: "https://ws.test/chat".into(),
            version: HttpVersionVo::Http11,
            headers: Vec::new(),
            body: None,
            raw_size: 0,
            origin: Default::default(),
        },
        Protocol::WebSocket,
        Utc::now(),
    );
    let socket_id = socket.id;
    store.exchanges.append(socket).await.expect("append");
    store
        .exchanges
        .append_ws_message(
            &socket_id,
            WebSocketMessage {
                direction: WsDirection::Received,
                opcode: WsOpcode::Text,
                payload: b"hello".to_vec(),
                size: 5,
                at: Utc::now(),
                injected: false,
            },
        )
        .await
        .expect("ws");
}

#[tokio::test]
async fn saved_traffic_opens_in_another_store_with_bodies_and_messages() {
    let source = store();
    fill(&source).await;
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("session.crabtrap");

    let saved = source.use_case.save(&path).await.expect("save");
    let target = store();
    let outcome = target.use_case.open(&path).await;

    assert_eq!(saved, 2);
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.imported.len(), 2);
    let opened = target
        .exchanges
        .list(FilterQuery::default(), 0, usize::MAX)
        .await
        .expect("list");
    let upload = opened
        .iter()
        .find(|e| e.request.uri.ends_with("/upload"))
        .expect("upload");
    assert!(upload.tags.contains(&IMPORTED_TAG.to_string()));
    assert!(upload.tags.contains(&"map-local".to_string()), "tags kept");
    assert_eq!(upload.status, ExchangeStatus::Completed);
    let request_body = target
        .bodies
        .get(upload.request.body.as_ref().expect("request ref"))
        .await
        .expect("get");
    assert_eq!(request_body, Some(vec![0, 1, 2, 255]));
    let response_ref = upload
        .response
        .as_ref()
        .and_then(|r| r.body.as_ref())
        .expect("response ref");
    assert_eq!(
        target.bodies.get(response_ref).await.expect("get"),
        Some(b"{\"ok\":true}".to_vec())
    );

    let socket = opened
        .iter()
        .find(|e| e.protocol == Protocol::WebSocket)
        .expect("socket");
    assert_eq!(
        socket.status,
        ExchangeStatus::Aborted,
        "pending can't finish"
    );
    let page = target
        .exchanges
        .ws_messages(&socket.id, 0)
        .await
        .expect("messages");
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].text().as_deref(), Some("hello"));
}

#[tokio::test]
async fn foreign_files_and_empty_traffic_are_refused() {
    let dir = tempfile::tempdir().expect("dir");
    let har = dir.path().join("x.har");
    std::fs::write(&har, b"{\"log\":{\"entries\":[]}}").expect("write");
    let target = store();

    let outcome = target.use_case.open(&har).await;
    let empty = target
        .use_case
        .save(&dir.path().join("empty.crabtrap"))
        .await;

    assert!(outcome.imported.is_empty());
    let error = outcome.error.expect("refused").to_string();
    assert!(error.contains("not a crabtrap session"), "{error}");
    assert!(empty.is_err(), "nothing to save");
    assert!(!dir.path().join("empty.crabtrap").exists());
}

#[tokio::test]
async fn har_entries_are_imported_with_decoded_bodies() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("chrome.har");
    std::fs::write(
        &path,
        r#"{"log":{"entries":[
          {"startedDateTime":"2026-10-20T10:00:00Z","time":50,
           "request":{"method":"GET","url":"https://api.test/users?page=2","httpVersion":"HTTP/1.1","headers":[]},
           "response":{"status":200,"statusText":"OK","httpVersion":"HTTP/1.1",
             "headers":[{"name":"Content-Type","value":"application/json"},{"name":"Content-Encoding","value":"br"}],
             "content":{"size":2,"mimeType":"application/json","text":"[]"}}},
          {"request":{"method":"GET","url":"not a url"},"response":{"status":200}}
        ]}}"#,
    )
    .expect("write");
    let target = store();

    let outcome = target.use_case.import_har(&path).await;

    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.imported.len(), 1);
    assert_eq!(outcome.skipped, 1);
    let row = &outcome.imported[0];
    assert_eq!(row.content_type.as_deref(), Some("application/json"));
    let exchange = target
        .exchanges
        .get(&row.summary.id)
        .await
        .expect("get")
        .expect("stored");
    assert!(exchange.tags.contains(&IMPORTED_TAG.to_string()));
    let body_ref = exchange
        .response
        .as_ref()
        .and_then(|r| r.body.as_ref())
        .expect("body");
    assert_eq!(body_ref.encoding, None, "HAR text is already decoded");
    assert_eq!(
        target.bodies.get(body_ref).await.expect("get"),
        Some(b"[]".to_vec())
    );
}

#[tokio::test]
async fn bodies_of_exchanges_pushed_out_by_a_big_session_are_released() {
    let source = store();
    fill(&source).await;
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("session.crabtrap");
    source.use_case.save(&path).await.expect("save");
    let small = store_with_capacity(1);

    let outcome = small.use_case.open(&path).await;

    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(small.exchanges.count().await.expect("count"), 1);
    // Загрузка (с телами) открылась первой и вытеснена сокетом без тел.
    assert_eq!(small.bodies.total_size().await.expect("size"), 0);
}
