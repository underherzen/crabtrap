//! Запрос/ответ ↔ JS-объекты скрипта (через `serde_json::Value`).
//!
//! Форма объектов — как в Proxyman:
//! - `request`: `method`, `url`, `scheme`, `host`, `port`, `path`,
//!   `queries` (`{k: v}`, повтор ключа — массив), `headers`, `body`;
//! - `response`: `statusCode`, `headers`, `body`.
//!
//! Тело: JSON-объект/массив → объект, форма → объект, текст → строка.
//! Двоичное тело — `body: null` и `bodyBase64` (замена двоичного тела —
//! через `bodyBase64`, текстом/объектом — через `body`). Не тронутое
//! скриптом тело возвращается байт-в-байт. Заголовки — объект
//! `{Имя: значение}` (повтор — массив); правка без учёта регистра имени.

use application::dto::scripting::{ScriptRequestDto, ScriptResponseDto};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use domain::value_objects::http_vo::HeaderEntry;
use serde_json::{Map, Value, json};

use super::query::rebuild_query;

/// Поле двоичного тела.
pub const BODY_BASE64: &str = "bodyBase64";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    Json,
    Form,
    Text,
    Binary,
}

fn content_type(headers: &[HeaderEntry]) -> String {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-type"))
        .map(|h| h.value.to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_form(headers: &[HeaderEntry]) -> bool {
    content_type(headers).starts_with("application/x-www-form-urlencoded")
}

/// Заголовки объекта `request`/`response` — форма ли тело (объект `body`
/// тогда кодируется формой, а не JSON).
pub fn js_headers_are_form(object: &Value) -> bool {
    object
        .get("headers")
        .and_then(Value::as_object)
        .and_then(|headers| {
            headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                .map(|(_, value)| scalar_list(value).join(","))
        })
        .is_some_and(|ct| {
            ct.to_ascii_lowercase()
                .starts_with("application/x-www-form-urlencoded")
        })
}

fn body_kind(headers: &[HeaderEntry], body: &[u8]) -> BodyKind {
    let ct = content_type(headers);
    let is_text = std::str::from_utf8(body).is_ok();
    // Только объект/массив: строка-JSON (`"x"`) редактируется как текст.
    let json_container =
        || serde_json::from_slice::<Value>(body).is_ok_and(|v| v.is_object() || v.is_array());
    if (ct.contains("/json") || ct.contains("+json")) && json_container() {
        BodyKind::Json
    } else if is_form(headers) && is_text {
        BodyKind::Form
    } else if is_text {
        BodyKind::Text
    } else {
        BodyKind::Binary
    }
}

/// Тело → поля `body` (+ `bodyBase64` у двоичного).
fn insert_body(object: &mut Map<String, Value>, headers: &[HeaderEntry], body: Option<&[u8]>) {
    let value = match body {
        None => Value::Null,
        Some(body) => match body_kind(headers, body) {
            BodyKind::Json => serde_json::from_slice(body).unwrap_or(Value::Null),
            BodyKind::Form => pairs_to_object(url::form_urlencoded::parse(body)),
            BodyKind::Text => Value::String(String::from_utf8_lossy(body).into_owned()),
            BodyKind::Binary => {
                object.insert(BODY_BASE64.into(), Value::String(BASE64.encode(body)));
                Value::Null
            }
        },
    };
    object.insert("body".into(), value);
}

/// Тело после скрипта: `bodyBase64` (двоичное), иначе `body`, иначе
/// исходное.
fn body_from_js(
    headers: &[HeaderEntry],
    original: Option<&Vec<u8>>,
    changed: impl Fn(&str) -> Option<Value>,
) -> Option<Vec<u8>> {
    if let Some(Value::String(encoded)) = changed(BODY_BASE64) {
        return Some(
            BASE64
                .decode(encoded.trim())
                .unwrap_or_else(|_| encoded.into_bytes()),
        );
    }
    let Some(value) = changed("body") else {
        return original.cloned();
    };
    Some(match value {
        Value::Null => Vec::new(),
        Value::String(text) => text.into_bytes(),
        Value::Object(map) if is_form(headers) => {
            let mut form = url::form_urlencoded::Serializer::new(String::new());
            for (key, value) in &map {
                for item in scalar_list(value) {
                    form.append_pair(key, &item);
                }
            }
            form.finish().into_bytes()
        }
        other => serde_json::to_vec(&other).unwrap_or_default(),
    })
}

/// Пары `k=v` → объект; повтор ключа — массив.
fn pairs_to_object<K, V>(pairs: impl Iterator<Item = (K, V)>) -> Value
where
    K: Into<String>,
    V: Into<String>,
{
    let mut map = Map::new();
    for (key, value) in pairs {
        let (key, value) = (key.into(), Value::String(value.into()));
        match map.get_mut(&key) {
            Some(Value::Array(items)) => items.push(value),
            Some(existing) => *existing = Value::Array(vec![existing.take(), value]),
            None => {
                map.insert(key, value);
            }
        }
    }
    Value::Object(map)
}

/// Значение (строка/число/массив) → список строк; `null` — пусто.
pub fn scalar_list(value: &Value) -> Vec<String> {
    match value {
        Value::Null => Vec::new(),
        Value::Array(items) => items.iter().flat_map(scalar_list).collect(),
        Value::String(text) => vec![text.clone()],
        other => vec![other.to_string()],
    }
}

fn headers_to_js(headers: &[HeaderEntry]) -> Value {
    pairs_to_object(headers.iter().map(|h| (h.name.clone(), h.value.clone())))
}

/// Объект заголовков от скрипта → список. Ключи, отличающиеся только
/// регистром, — один заголовок: берётся изменённый/новый вариант
/// (`headers["Content-Type"] = …` при исходном `content-type`).
fn headers_from_js(original: &[HeaderEntry], value: &Value) -> Vec<HeaderEntry> {
    let Value::Object(map) = value else {
        return original.to_vec();
    };
    let before = headers_to_js(original);
    let before = before.as_object().cloned().unwrap_or_default();
    let mut chosen: Vec<(String, &Value)> = Vec::new();
    for (name, value) in map {
        let changed = before.get(name) != Some(value);
        match chosen
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            Some(ix) if changed => chosen[ix] = (name.clone(), value),
            Some(_) => {}
            None => chosen.push((name.clone(), value)),
        }
    }
    chosen
        .into_iter()
        .flat_map(|(name, value)| {
            scalar_list(value)
                .into_iter()
                .map(move |value| HeaderEntry {
                    name: name.clone(),
                    value,
                })
        })
        .collect()
}

/// Запрос → объект `request`.
pub fn request_to_js(request: &ScriptRequestDto) -> Value {
    let parsed = url::Url::parse(&request.url).ok();
    let mut object = json!({
        "method": request.method,
        "url": request.url,
        "scheme": parsed.as_ref().map(|u| u.scheme().to_string()),
        "host": parsed.as_ref().and_then(|u| u.host_str().map(str::to_string)),
        "port": parsed.as_ref().and_then(url::Url::port_or_known_default),
        "path": parsed.as_ref().map(|u| u.path().to_string()),
        "queries": parsed.as_ref().map_or(Value::Object(Map::new()), |u| pairs_to_object(u.query_pairs())),
        "headers": headers_to_js(&request.headers),
    });
    if let Some(map) = object.as_object_mut() {
        insert_body(map, &request.headers, request.body.as_deref());
    }
    object
}

/// Объект `request` после скрипта → запрос. Поля, которых скрипт не
/// менял, берутся из исходного запроса как есть.
pub fn request_from_js(original: &ScriptRequestDto, value: &Value) -> ScriptRequestDto {
    let before = request_to_js(original);
    let changed = |name: &str| {
        value
            .get(name)
            .filter(|v| before.get(name) != Some(*v))
            .cloned()
    };
    let method = changed("method")
        .and_then(|m| m.as_str().map(|m| m.trim().to_ascii_uppercase()))
        .unwrap_or_else(|| original.method.clone());
    let url = changed("url")
        .and_then(|u| u.as_str().map(str::to_string))
        .unwrap_or_else(|| rebuild_url(original, &before, value));
    let headers = match changed("headers") {
        Some(headers) => headers_from_js(&original.headers, &headers),
        None => original.headers.clone(),
    };
    let body = body_from_js(&headers, original.body.as_ref(), changed);
    ScriptRequestDto {
        method,
        url,
        headers,
        body,
    }
}

/// URL из частей, если скрипт менял scheme/host/port/path/queries.
/// Невалидные значения частей игнорируются (с предупреждением).
fn rebuild_url(original: &ScriptRequestDto, before: &Value, after: &Value) -> String {
    let changed = |name: &str| after.get(name).filter(|v| before.get(name) != Some(*v));
    let Ok(mut url) = url::Url::parse(&original.url) else {
        return original.url.clone();
    };
    if let Some(scheme) = changed("scheme").and_then(Value::as_str)
        && url.set_scheme(scheme).is_err()
    {
        tracing::warn!(scheme, "script: scheme ignored");
    }
    if let Some(host) = changed("host").and_then(Value::as_str) {
        // `host:3000` — хост и порт сразу.
        let (host, port) = match host.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') && p.parse::<u16>().is_ok() => (h, p.parse().ok()),
            _ => (host, None),
        };
        match url.set_host(Some(host)) {
            Ok(()) => {
                if let Some(port) = port {
                    let _ = url.set_port(Some(port));
                }
            }
            Err(error) => tracing::warn!(host, %error, "script: host ignored"),
        }
    }
    if let Some(port) = changed("port") {
        let port = match port {
            Value::Number(n) => n.as_u64().and_then(|p| u16::try_from(p).ok()),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        };
        match port {
            Some(port) => {
                let _ = url.set_port(Some(port));
            }
            None => tracing::warn!("script: port ignored (not a number 0–65535)"),
        }
    }
    if let Some(path) = changed("path").and_then(Value::as_str) {
        url.set_path(path);
    }
    if let Some(queries) = changed("queries") {
        let before_queries = before["queries"].as_object().cloned().unwrap_or_default();
        let query = rebuild_query(url.query(), &before_queries, queries);
        url.set_query(query.as_deref());
    }
    url.to_string()
}

/// Ответ → объект `response`.
pub fn response_to_js(response: &ScriptResponseDto) -> Value {
    let mut object = json!({
        "statusCode": response.status,
        "headers": headers_to_js(&response.headers),
    });
    if let Some(map) = object.as_object_mut() {
        insert_body(map, &response.headers, response.body.as_deref());
    }
    object
}

/// Объект `response` после скрипта → ответ.
pub fn response_from_js(original: &ScriptResponseDto, value: &Value) -> ScriptResponseDto {
    let before = response_to_js(original);
    let changed = |name: &str| {
        value
            .get(name)
            .filter(|v| before.get(name) != Some(*v))
            .cloned()
    };
    let status = changed("statusCode")
        .and_then(|s| s.as_u64())
        .and_then(|s| u16::try_from(s).ok())
        .unwrap_or(original.status);
    let headers = match changed("headers") {
        Some(headers) => headers_from_js(&original.headers, &headers),
        None => original.headers.clone(),
    };
    let body = body_from_js(&headers, original.body.as_ref(), changed);
    ScriptResponseDto {
        status,
        headers,
        body,
    }
}

#[cfg(test)]
#[path = "bindings_tests.rs"]
mod tests;
