//! HAR 1.2 (http://www.softwareishard.com/blog/har-12-spec/): сериализация
//! захваченных обменов. Тела (ответа уже без Content-Encoding) приносит
//! вызывающий; без тел — только заголовки и размеры. Обмен без ответа в лог
//! не попадает. Документ пишется потоком ([`HarWriter`]): в памяти — одна
//! запись, а не вся сессия.

use std::io::Write;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::SecondsFormat;
use domain::entities::exchange::{HttpExchange, HttpResponse};
use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo};
use domain::value_objects::timing::TimingPhases;
use serde::Serialize;

/// Версия формата.
const HAR_VERSION: &str = "1.2";
/// «Не применимо / не измерено» в HAR.
const NOT_APPLICABLE: f64 = -1.0;

#[derive(Debug, Serialize)]
pub struct HarCreator {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarEntry {
    pub started_date_time: String,
    pub time: f64,
    pub request: HarRequest,
    pub response: HarResponse,
    pub cache: HarCache,
    pub timings: HarTimings,
    pub connection: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarRequest {
    pub method: String,
    pub url: String,
    pub http_version: String,
    pub cookies: Vec<HarCookie>,
    pub headers: Vec<HarNameValue>,
    pub query_string: Vec<HarNameValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_data: Option<HarPostData>,
    pub headers_size: i64,
    pub body_size: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarResponse {
    pub status: u16,
    pub status_text: String,
    pub http_version: String,
    pub cookies: Vec<HarCookie>,
    pub headers: Vec<HarNameValue>,
    pub content: HarContent,
    #[serde(rename = "redirectURL")]
    pub redirect_url: String,
    pub headers_size: i64,
    pub body_size: i64,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct HarNameValue {
    pub name: String,
    pub value: String,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarCookie {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarPostData {
    pub mime_type: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarContent {
    pub size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression: Option<i64>,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct HarCache {}

#[derive(Debug, PartialEq, Serialize)]
pub struct HarTimings {
    pub blocked: f64,
    pub dns: f64,
    pub connect: f64,
    pub ssl: f64,
    pub send: f64,
    pub wait: f64,
    pub receive: f64,
}

/// Обмен и его тела для HAR (`None` — тело не экспортируется или нет).
pub struct HarSource<'a> {
    pub exchange: &'a HttpExchange,
    pub request_body: Option<&'a [u8]>,
    /// Тело ответа без Content-Encoding.
    pub response_body: Option<&'a [u8]>,
}

/// Потоковая запись документа HAR: шапка — в `new`, записи по одной —
/// `push`, закрытие массива и объекта — `finish`.
pub struct HarWriter<W: Write> {
    out: W,
    entries: usize,
}

impl<W: Write> HarWriter<W> {
    pub fn new(mut out: W) -> std::io::Result<Self> {
        let creator = HarCreator {
            name: "crabtrap",
            version: env!("CARGO_PKG_VERSION"),
        };
        write!(out, r#"{{"log":{{"version":"{HAR_VERSION}","creator":"#)?;
        serde_json::to_writer(&mut out, &creator)?;
        out.write_all(br#","entries":["#)?;
        Ok(Self { out, entries: 0 })
    }

    pub fn push(&mut self, entry: &HarEntry) -> std::io::Result<()> {
        if self.entries > 0 {
            self.out.write_all(b",")?;
        }
        serde_json::to_writer(&mut self.out, entry)?;
        self.entries += 1;
        Ok(())
    }

    /// Закрыть документ; возврат — поток и число записей.
    pub fn finish(mut self) -> std::io::Result<(W, usize)> {
        self.out.write_all(b"]}}")?;
        self.out.flush()?;
        Ok((self.out, self.entries))
    }
}

/// Запись HAR; `None` — у обмена нет ответа.
pub fn har_entry(source: &HarSource<'_>) -> Option<HarEntry> {
    let exchange = source.exchange;
    let response = exchange.response.as_ref()?;
    let tags =
        (!exchange.tags.is_empty()).then(|| format!("crabtrap: {}", exchange.tags.join(", ")));
    let timings = har_timings(&exchange.timing);
    Some(HarEntry {
        started_date_time: exchange
            .started_at
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        time: har_time(&timings, &exchange.timing),
        request: har_request(exchange, source.request_body),
        response: har_response(response, source.response_body),
        cache: HarCache {},
        timings,
        connection: exchange.connection_id.to_string(),
        comment: tags,
    })
}

fn har_request(exchange: &HttpExchange, body: Option<&[u8]>) -> HarRequest {
    let request = &exchange.request;
    let url = url::Url::parse(&request.uri).ok();
    let query_string = url
        .as_ref()
        .map(|u| {
            u.query_pairs()
                .map(|(name, value)| HarNameValue {
                    name: name.into_owned(),
                    value: value.into_owned(),
                })
                .collect()
        })
        .unwrap_or_default();
    let post_data = body
        .filter(|b| !b.is_empty())
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(|text| HarPostData {
            mime_type: header(&request.headers, "content-type")
                .unwrap_or_default()
                .to_string(),
            text: text.to_string(),
        });
    HarRequest {
        method: request.method.to_string(),
        url: url.map_or_else(|| request.uri.clone(), String::from),
        http_version: har_version(request.version),
        cookies: request_cookies(&request.headers),
        headers: name_values(&request.headers),
        query_string,
        post_data,
        headers_size: NOT_APPLICABLE as i64,
        body_size: request.body.as_ref().map_or(0, |b| b.size as i64),
    }
}

fn har_response(response: &HttpResponse, body: Option<&[u8]>) -> HarResponse {
    let raw_size = response.raw_size as i64;
    let (text, encoding) = match body.filter(|b| !b.is_empty()) {
        None => (None, None),
        Some(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => (Some(text.to_string()), None),
            Err(_) => (Some(BASE64.encode(bytes)), Some("base64")),
        },
    };
    let size = body.map_or(raw_size, |b| b.len() as i64);
    HarResponse {
        status: response.status,
        status_text: response.reason.clone().unwrap_or_default(),
        http_version: har_version(response.version),
        cookies: response_cookies(&response.headers),
        headers: name_values(&response.headers),
        content: HarContent {
            size,
            compression: body.map(|_| size - raw_size).filter(|saved| *saved > 0),
            mime_type: header(&response.headers, "content-type")
                .unwrap_or_default()
                .to_string(),
            text,
            encoding,
        },
        redirect_url: header(&response.headers, "location")
            .unwrap_or_default()
            .to_string(),
        headers_size: NOT_APPLICABLE as i64,
        body_size: raw_size,
    }
}

/// Версия в виде, принятом в HAR-файлах браузеров (`HTTP/2.0`, не `HTTP/2`).
fn har_version(version: HttpVersionVo) -> String {
    match version {
        HttpVersionVo::H2 => "HTTP/2.0".into(),
        HttpVersionVo::H3 => "HTTP/3.0".into(),
        other => other.to_string(),
    }
}

/// `time` по спецификации — сумма применимых фаз (без `-1`); не замерено
/// ничего — общее время движка.
fn har_time(timings: &HarTimings, timing: &TimingPhases) -> f64 {
    let sum: f64 = [
        timings.blocked,
        timings.dns,
        timings.connect,
        timings.send,
        timings.wait,
        timings.receive,
    ]
    .into_iter()
    .filter(|ms| *ms > 0.0)
    .sum();
    if sum > 0.0 {
        sum
    } else {
        timing.or_summed_total()
    }
}

/// Тайминги HAR: незамеренная фаза (переиспользованное соединение) — `-1`;
/// `connect` по спецификации включает `ssl`.
fn har_timings(timing: &TimingPhases) -> HarTimings {
    let measured = |ms: f64| if ms > 0.0 { ms } else { NOT_APPLICABLE };
    HarTimings {
        blocked: NOT_APPLICABLE,
        dns: measured(timing.dns_ms),
        connect: measured(timing.connect_ms + timing.tls_ms),
        ssl: measured(timing.tls_ms),
        send: timing.send_ms.max(0.0),
        wait: timing.wait_ms.max(0.0),
        receive: timing.receive_ms.max(0.0),
    }
}

fn header<'a>(headers: &'a [HeaderEntry], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

fn name_values(headers: &[HeaderEntry]) -> Vec<HarNameValue> {
    headers
        .iter()
        .map(|h| HarNameValue {
            name: h.name.clone(),
            value: h.value.clone(),
        })
        .collect()
}

/// `Cookie: a=1; b=2` (все заголовки Cookie).
fn request_cookies(headers: &[HeaderEntry]) -> Vec<HarCookie> {
    headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("cookie"))
        .flat_map(|h| h.value.split(';'))
        .filter_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            Some(cookie(name, value))
        })
        .collect()
}

/// `Set-Cookie: name=value; Path=/; Domain=…; HttpOnly; Secure`.
fn response_cookies(headers: &[HeaderEntry]) -> Vec<HarCookie> {
    headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("set-cookie"))
        .filter_map(|h| {
            let mut parts = h.value.split(';').map(str::trim);
            let (name, value) = parts.next()?.split_once('=')?;
            Some(parts.fold(cookie(name, value), |c, attribute| {
                let (key, val) = attribute.split_once('=').unwrap_or((attribute, ""));
                match key.to_ascii_lowercase().as_str() {
                    "path" => HarCookie {
                        path: Some(val.into()),
                        ..c
                    },
                    "domain" => HarCookie {
                        domain: Some(val.into()),
                        ..c
                    },
                    "httponly" => HarCookie {
                        http_only: Some(true),
                        ..c
                    },
                    "secure" => HarCookie {
                        secure: Some(true),
                        ..c
                    },
                    _ => c,
                }
            }))
        })
        .collect()
}

fn cookie(name: &str, value: &str) -> HarCookie {
    HarCookie {
        name: name.trim().to_string(),
        value: value.trim().to_string(),
        path: None,
        domain: None,
        http_only: None,
        secure: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use domain::entities::exchange::HttpRequest;
    use domain::value_objects::http_vo::{HttpVersionVo, Method, Protocol};
    use domain::value_objects::{ConnectionId, Host};

    fn exchange(response: Option<HttpResponse>) -> HttpExchange {
        let mut exchange = HttpExchange::new_pending(
            ConnectionId::new(),
            Host::new("api.test").expect("host"),
            HttpRequest {
                method: Method::new("POST"),
                uri: "https://api.test/v1/items?page=2&q=a%20b".into(),
                version: HttpVersionVo::H2,
                headers: vec![
                    HeaderEntry::new("Content-Type", "application/json"),
                    HeaderEntry::new("Cookie", "sid=1; theme=dark"),
                ],
                body: None,
                raw_size: 0,
                origin: Default::default(),
            },
            Protocol::Http2,
            Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0)
                .single()
                .expect("time"),
        );
        exchange.response = response;
        exchange.timing = TimingPhases {
            dns_ms: 0.0,
            connect_ms: 10.0,
            tls_ms: 5.0,
            send_ms: 1.0,
            wait_ms: 30.0,
            receive_ms: 4.0,
            total_ms: 50.0,
        };
        exchange.tags = vec!["map-local".into()];
        exchange
    }

    fn response() -> HttpResponse {
        HttpResponse {
            status: 302,
            reason: Some("Found".into()),
            headers: vec![
                HeaderEntry::new("Content-Type", "application/json"),
                HeaderEntry::new("Location", "/next"),
                HeaderEntry::new("Set-Cookie", "sid=2; Path=/; HttpOnly; Secure"),
            ],
            body: None,
            raw_size: 20,
            version: HttpVersionVo::H2,
        }
    }

    #[test]
    fn entry_maps_request_response_and_timings() {
        let exchange = exchange(Some(response()));
        let entry = har_entry(&HarSource {
            exchange: &exchange,
            request_body: Some(br#"{"a":1}"#),
            response_body: Some(br#"{"ok":true,"list":[1,2,3]}"#),
        })
        .expect("entry");

        assert_eq!(entry.started_date_time, "2026-10-20T12:00:00.000Z");
        assert_eq!(entry.time, 50.0, "sum of connect, send, wait and receive");
        assert_eq!(entry.request.http_version, "HTTP/2.0");
        assert_eq!(
            entry.request.query_string,
            vec![
                HarNameValue {
                    name: "page".into(),
                    value: "2".into()
                },
                HarNameValue {
                    name: "q".into(),
                    value: "a b".into()
                },
            ]
        );
        assert_eq!(entry.request.cookies.len(), 2);
        let post = entry.request.post_data.expect("post data");
        assert_eq!(
            (post.mime_type.as_str(), post.text.as_str()),
            ("application/json", r#"{"a":1}"#)
        );
        assert_eq!(entry.response.redirect_url, "/next");
        assert_eq!(entry.response.status_text, "Found");
        assert_eq!(entry.response.content.size, 26);
        assert_eq!(entry.response.content.compression, Some(6));
        assert_eq!(entry.response.body_size, 20);
        assert_eq!(
            entry.response.cookies,
            vec![HarCookie {
                path: Some("/".into()),
                http_only: Some(true),
                secure: Some(true),
                ..cookie("sid", "2")
            }]
        );
        assert_eq!(
            entry.timings,
            HarTimings {
                blocked: -1.0,
                dns: -1.0,
                connect: 15.0,
                ssl: 5.0,
                send: 1.0,
                wait: 30.0,
                receive: 4.0
            }
        );
        assert_eq!(entry.comment.as_deref(), Some("crabtrap: map-local"));
    }

    #[test]
    fn binary_body_is_base64_and_no_body_keeps_sizes() {
        let exchange = exchange(Some(response()));
        let binary = har_entry(&HarSource {
            exchange: &exchange,
            request_body: None,
            response_body: Some(&[0xff, 0x00]),
        })
        .expect("entry");
        assert_eq!(binary.response.content.text.as_deref(), Some("/wA="));
        assert_eq!(binary.response.content.encoding, Some("base64"));

        let headers_only = har_entry(&HarSource {
            exchange: &exchange,
            request_body: None,
            response_body: None,
        })
        .expect("entry");
        assert_eq!(headers_only.response.content.text, None);
        assert_eq!(headers_only.response.content.size, 20);
        assert!(headers_only.request.post_data.is_none());
    }

    #[test]
    fn empty_document_is_valid_json() {
        let (bytes, count) = HarWriter::new(Vec::new())
            .and_then(HarWriter::finish)
            .expect("write");
        assert_eq!(count, 0);
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json");
        assert_eq!(json["log"]["entries"], serde_json::json!([]));
    }

    #[test]
    fn exchange_without_response_is_skipped_and_document_is_valid_json() {
        let pending = exchange(None);
        assert!(
            har_entry(&HarSource {
                exchange: &pending,
                request_body: None,
                response_body: None
            })
            .is_none()
        );

        let done = exchange(Some(response()));
        let entry = har_entry(&HarSource {
            exchange: &done,
            request_body: None,
            response_body: None,
        })
        .expect("entry");
        let mut writer = HarWriter::new(Vec::new()).expect("header");
        writer.push(&entry).expect("entry");
        writer.push(&entry).expect("entry");
        let (bytes, count) = writer.finish().expect("finish");
        assert_eq!(count, 2);
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json");
        assert_eq!(json["log"]["entries"].as_array().map(Vec::len), Some(2));
        assert_eq!(json["log"]["version"], "1.2");
        assert_eq!(json["log"]["creator"]["name"], "crabtrap");
        let entry = &json["log"]["entries"][0];
        for key in [
            "startedDateTime",
            "time",
            "request",
            "response",
            "cache",
            "timings",
        ] {
            assert!(entry.get(key).is_some(), "missing {key}");
        }
        assert!(entry["response"].get("redirectURL").is_some());
        assert!(entry["request"].get("postData").is_none());
    }
}
