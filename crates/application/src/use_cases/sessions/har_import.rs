//! File → Import HAR…: записи HAR 1.2 (Chrome DevTools, Charles, Proxyman,
//! экспорт crabtrap) → записи сессии, которые открываются как `.crabtrap`.
//! Разбор нестрогий: у разных инструментов разные необязательные поля.
//! Тело ответа в HAR уже распаковано — сохраняется без Content-Encoding;
//! сообщения WebSocket Chrome (`_webSocketMessages`) тоже переносятся.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, TimeZone as _, Utc};
use domain::entities::body::BodyRef;
use domain::entities::exchange::{ExchangeStatus, HttpExchange, HttpRequest, HttpResponse};
use domain::entities::websocket::{WebSocketMessage, WsDirection, WsOpcode};
use domain::errors::DomainError;
use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
use domain::value_objects::timing::TimingPhases;
use domain::value_objects::{BodyId, ConnectionId, Host};
use std::io::Read as _;

use serde::Deserialize;

use super::format::{SessionRecord, StoredBody};

/// Потолок файла HAR (читается целиком: JSON).
pub const MAX_HAR_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Deserialize)]
struct HarFile {
    log: HarLog,
}

#[derive(Deserialize)]
struct HarLog {
    #[serde(default, deserialize_with = "null_default")]
    entries: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    #[serde(default, deserialize_with = "null_default")]
    started_date_time: String,
    #[serde(default, deserialize_with = "null_default")]
    time: f64,
    request: Request,
    #[serde(default)]
    response: Option<Response>,
    #[serde(default)]
    timings: Option<Timings>,
    #[serde(
        default,
        rename = "_webSocketMessages",
        deserialize_with = "null_default"
    )]
    web_socket_messages: Vec<WsMessage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    method: String,
    url: String,
    #[serde(default, deserialize_with = "null_default")]
    http_version: String,
    #[serde(default, deserialize_with = "null_default")]
    headers: Vec<NameValue>,
    #[serde(default)]
    post_data: Option<PostData>,
    #[serde(default = "unknown_size", deserialize_with = "null_size")]
    headers_size: f64,
    #[serde(default = "unknown_size", deserialize_with = "null_size")]
    body_size: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostData {
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    encoding: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    #[serde(default, deserialize_with = "null_default")]
    status: f64,
    #[serde(default, deserialize_with = "null_default")]
    status_text: String,
    #[serde(default, deserialize_with = "null_default")]
    http_version: String,
    #[serde(default, deserialize_with = "null_default")]
    headers: Vec<NameValue>,
    #[serde(default)]
    content: Option<Content>,
    #[serde(default = "unknown_size", deserialize_with = "null_size")]
    headers_size: f64,
    #[serde(default = "unknown_size", deserialize_with = "null_size")]
    body_size: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Content {
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    encoding: Option<String>,
}

#[derive(Deserialize)]
struct NameValue {
    name: String,
    #[serde(default, deserialize_with = "null_default")]
    value: String,
}

#[derive(Deserialize, Default)]
struct Timings {
    #[serde(default = "unknown_size", deserialize_with = "null_size")]
    dns: f64,
    #[serde(default = "unknown_size", deserialize_with = "null_size")]
    connect: f64,
    #[serde(default = "unknown_size", deserialize_with = "null_size")]
    ssl: f64,
    #[serde(default = "unknown_size", deserialize_with = "null_size")]
    send: f64,
    #[serde(default = "unknown_size", deserialize_with = "null_size")]
    wait: f64,
    #[serde(default = "unknown_size", deserialize_with = "null_size")]
    receive: f64,
}

#[derive(Deserialize)]
struct WsMessage {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "null_default")]
    time: f64,
    #[serde(default = "text_opcode")]
    opcode: u8,
    #[serde(default, deserialize_with = "null_default")]
    data: String,
}

/// `null` в поле — как отсутствие поля (Charles и другие пишут `null`).
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// То же для размеров: `null` — «неизвестно» (`-1`).
fn null_size<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(-1.0))
}

fn unknown_size() -> f64 {
    -1.0
}

fn text_opcode() -> u8 {
    1
}

/// Записи HAR-файла: годные — записями сессии, негодные — счётчиком.
pub struct ParsedHar {
    pub records: Vec<SessionRecord>,
    pub skipped: usize,
}

/// Разобрать HAR (весь файл — JSON; больше [`MAX_HAR_BYTES`] — отказ).
pub fn parse_har(input: impl std::io::Read) -> Result<ParsedHar, DomainError> {
    let mut bytes = Vec::new();
    std::io::Read::take(input, MAX_HAR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| DomainError::InfrastructureError(format!("read HAR: {e}")))?;
    if bytes.len() as u64 > MAX_HAR_BYTES {
        return Err(DomainError::ValidationError(
            "the HAR file is larger than 1 GiB".into(),
        ));
    }
    let file: HarFile = serde_json::from_slice(&bytes)
        .map_err(|e| DomainError::ValidationError(format!("this is not a HAR file: {e}")))?;
    drop(bytes);
    let mut records = Vec::with_capacity(file.log.entries.len());
    let mut skipped = 0;
    for value in file.log.entries {
        match serde_json::from_value::<Entry>(value).ok().and_then(record) {
            Some(record) => records.push(record),
            None => skipped += 1,
        }
    }
    Ok(ParsedHar { records, skipped })
}

/// Запись HAR → запись сессии; `None` — URL без хоста, хост, который
/// crabtrap не хранит (IPv6), и т.п.
fn record(entry: Entry) -> Option<SessionRecord> {
    let url = url::Url::parse(&entry.request.url).ok()?;
    let host = Host::new(url.host_str()?).ok()?;
    let mut bodies = Vec::new();
    let request_body = entry.request.post_data.and_then(|post| {
        decode(post.text.as_deref(), post.encoding.as_deref())
            .map(|data| stored(&mut bodies, data, post.mime_type))
    });
    let request_bytes = request_body.as_ref().map_or(0, |b| b.size);
    let ws_messages: Vec<WebSocketMessage> = entry
        .web_socket_messages
        .iter()
        .filter_map(ws_message)
        .collect();
    // Статус 0 (запрос не состоялся) и вне 100–599 — ответа не было.
    let response = entry
        .response
        .filter(|r| (100.0..600.0).contains(&r.status))
        .map(|r| response(r, &mut bodies));
    let websocket = !ws_messages.is_empty() || response.as_ref().is_some_and(|r| r.status == 101);
    let request_version = version(&entry.request.http_version);
    let protocol = match (websocket, request_version) {
        (true, _) => Protocol::WebSocket,
        (false, HttpVersionVo::H2 | HttpVersionVo::H3) => Protocol::Http2,
        (false, _) => Protocol::Http1,
    };
    let started_at = DateTime::parse_from_rfc3339(&entry.started_date_time)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let status = match &response {
        Some(_) => ExchangeStatus::Completed,
        None => ExchangeStatus::Failed,
    };
    let exchange = HttpExchange {
        response,
        status,
        timing: timing(entry.timings.unwrap_or_default(), entry.time),
        ..HttpExchange::new_pending(
            ConnectionId::new(),
            host,
            HttpRequest {
                method: Method::new(entry.request.method),
                uri: url.to_string(),
                version: request_version,
                headers: headers(entry.request.headers),
                body: request_body,
                raw_size: wire_size(
                    entry.request.headers_size,
                    entry.request.body_size,
                    request_bytes,
                ),
                origin: Default::default(),
            },
            protocol,
            started_at,
        )
    };
    Some(SessionRecord {
        exchange,
        bodies,
        ws_messages,
    })
}

fn response(r: Response, bodies: &mut Vec<StoredBody>) -> HttpResponse {
    let body = r.content.and_then(|c| {
        decode(c.text.as_deref(), c.encoding.as_deref())
            .map(|data| stored(bodies, data, c.mime_type))
    });
    let body_bytes = body.as_ref().map_or(0, |b| b.size);
    HttpResponse {
        status: r.status as u16,
        reason: Some(r.status_text).filter(|t| !t.is_empty()),
        headers: headers(r.headers),
        body,
        raw_size: wire_size(r.headers_size, r.body_size, body_bytes),
        version: version(&r.http_version),
    }
}

/// Тело в записи: ссылка (без Content-Encoding — HAR хранит распакованное)
/// и байты под её id.
fn stored(bodies: &mut Vec<StoredBody>, data: Vec<u8>, mime: Option<String>) -> BodyRef {
    let body_ref = BodyRef {
        id: BodyId::new(),
        size: data.len() as u64,
        content_type: mime.filter(|m| !m.is_empty()),
        encoding: None,
        storage: Default::default(),
    };
    bodies.push(StoredBody {
        id: body_ref.id,
        data,
    });
    body_ref
}

/// Текст тела: как есть или base64; пустого тела нет.
fn decode(text: Option<&str>, encoding: Option<&str>) -> Option<Vec<u8>> {
    let text = text.filter(|t| !t.is_empty())?;
    match encoding {
        Some(e) if e.eq_ignore_ascii_case("base64") => STANDARD.decode(text.trim()).ok(),
        _ => Some(text.as_bytes().to_vec()),
    }
}

fn headers(list: Vec<NameValue>) -> Vec<HeaderEntry> {
    list.into_iter()
        // Псевдозаголовки HTTP/2 (`:authority`) — не заголовки сообщения.
        .filter(|h| !h.name.starts_with(':'))
        .map(|h| HeaderEntry::new(h.name, h.value))
        .collect()
}

fn version(text: &str) -> HttpVersionVo {
    match text.to_ascii_lowercase().as_str() {
        "http/1.0" => HttpVersionVo::Http10,
        "http/2" | "http/2.0" | "h2" | "h2c" => HttpVersionVo::H2,
        "http/3" | "http/3.0" | "h3" => HttpVersionVo::H3,
        _ => HttpVersionVo::Http11,
    }
}

/// Размер «по проводу»: заголовки + тело, если HAR их знает (`-1` — нет).
fn wire_size(headers: f64, body: f64, body_bytes: u64) -> u64 {
    let known = |v: f64| (v >= 0.0).then_some(v as u64);
    known(headers)
        .unwrap_or(0)
        .saturating_add(known(body).unwrap_or(body_bytes))
}

/// Фазы HAR (`-1` — не измерено; `connect` включает `ssl`).
fn timing(t: Timings, total: f64) -> TimingPhases {
    let phase = |v: f64| v.max(0.0);
    TimingPhases {
        dns_ms: phase(t.dns),
        connect_ms: (phase(t.connect) - phase(t.ssl)).max(0.0),
        tls_ms: phase(t.ssl),
        send_ms: phase(t.send),
        wait_ms: phase(t.wait),
        receive_ms: phase(t.receive),
        total_ms: phase(total),
    }
}

fn ws_message(message: &WsMessage) -> Option<WebSocketMessage> {
    let direction = match message.kind.as_str() {
        "send" => WsDirection::Sent,
        "receive" => WsDirection::Received,
        _ => return None,
    };
    let (opcode, mut payload) = match message.opcode {
        1 => (WsOpcode::Text, message.data.as_bytes().to_vec()),
        2 => (WsOpcode::Binary, STANDARD.decode(&message.data).ok()?),
        _ => return None,
    };
    let at = Utc
        .timestamp_millis_opt((message.time * 1000.0) as i64)
        .single()
        .unwrap_or_else(Utc::now);
    let size = payload.len() as u64;
    payload.truncate(domain::entities::websocket::WS_PAYLOAD_CAP);
    Some(WebSocketMessage {
        direction,
        opcode,
        payload,
        size,
        at,
        injected: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME: &str = r#"{"log":{"version":"1.2","entries":[
      {"startedDateTime":"2026-10-20T10:00:00.000Z","time":120.5,
       "request":{"method":"POST","url":"https://api.test/v1/items?x=1","httpVersion":"http/2.0",
         "headers":[{"name":":authority","value":"api.test"},{"name":"content-type","value":"application/json"}],
         "postData":{"mimeType":"application/json","text":"{\"a\":1}"},"headersSize":-1,"bodySize":7},
       "response":{"status":201,"statusText":"","httpVersion":"http/2.0",
         "headers":[{"name":"content-encoding","value":"gzip"},{"name":"content-type","value":"application/json"}],
         "content":{"size":11,"mimeType":"application/json","text":"eyJvayI6dHJ1ZX0=","encoding":"base64"},
         "headersSize":-1,"bodySize":30},
       "timings":{"blocked":1,"dns":-1,"connect":20,"ssl":15,"send":1,"wait":90,"receive":4}},
      {"startedDateTime":"not a date","time":0,
       "request":{"method":"GET","url":"wss://chat.test/socket","headers":[]},
       "response":{"status":101,"headers":[],"content":{"size":0}},
       "_webSocketMessages":[{"type":"send","time":1760954400.5,"opcode":1,"data":"hi"},
                             {"type":"receive","time":1760954401,"opcode":2,"data":"AP8="}]},
      {"request":{"method":"GET","url":"http://[::1]:8080/"},"response":{"status":200}},
      {"request":{"method":"GET","url":"https://refused.test/"},"response":{"status":0,"_error":"net::ERR_BLOCKED"}},
      {"unexpected":true}
    ]}}"#;

    #[test]
    fn chrome_entries_become_records() {
        let parsed = parse_har(CHROME.as_bytes()).expect("har");
        assert_eq!(parsed.records.len(), 3);
        assert_eq!(parsed.skipped, 2, "IPv6 host and a malformed entry");

        let post = &parsed.records[0];
        let exchange = &post.exchange;
        assert_eq!(exchange.request.method.as_str(), "POST");
        assert_eq!(exchange.protocol, Protocol::Http2);
        assert!(
            !exchange
                .request
                .headers
                .iter()
                .any(|h| h.name.starts_with(':'))
        );
        let response = exchange.response.as_ref().expect("response");
        assert_eq!(response.status, 201);
        let response_ref = response.body.as_ref().expect("body");
        assert_eq!(response_ref.encoding, None, "HAR bodies are decoded");
        let body = post
            .bodies
            .iter()
            .find(|b| b.id == response_ref.id)
            .expect("stored");
        assert_eq!(body.data, b"{\"ok\":true}");
        assert_eq!(exchange.timing.tls_ms, 15.0);
        assert_eq!(exchange.timing.connect_ms, 5.0);
        assert_eq!(exchange.timing.dns_ms, 0.0);

        let socket = &parsed.records[1];
        assert_eq!(socket.exchange.protocol, Protocol::WebSocket);
        assert_eq!(socket.ws_messages.len(), 2);
        assert_eq!(socket.ws_messages[0].text().as_deref(), Some("hi"));
        assert_eq!(socket.ws_messages[1].payload, vec![0, 255]);

        let failed = &parsed.records[2];
        assert_eq!(failed.exchange.status, ExchangeStatus::Failed);
        assert!(failed.exchange.response.is_none());
    }

    #[test]
    fn nulls_and_absurd_numbers_are_tolerated() {
        let har = r#"{"log":{"entries":[
          {"startedDateTime":null,"time":null,
           "request":{"method":"GET","url":"https://api.test/","httpVersion":null,"headers":null,
             "headersSize":1e30,"bodySize":1e30},
           "response":{"status":1e9,"statusText":null,"headers":null}}
        ]}}"#;
        let parsed = parse_har(har.as_bytes()).expect("har");
        assert_eq!(parsed.records.len(), 1, "nulls don't skip the entry");
        let exchange = &parsed.records[0].exchange;
        assert_eq!(
            exchange.request.raw_size,
            u64::MAX,
            "saturates, no overflow"
        );
        assert!(exchange.response.is_none(), "status 1e9 is not a response");
        assert_eq!(exchange.status, ExchangeStatus::Failed);
    }

    #[test]
    fn non_har_input_is_refused() {
        assert!(parse_har(b"not json".as_slice()).is_err());
        assert!(parse_har(br#"{"entries":[]}"#.as_slice()).is_err());
        let empty = parse_har(br#"{"log":{}}"#.as_slice()).expect("empty log");
        assert!(empty.records.is_empty());
    }
}
