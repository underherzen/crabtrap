//! «Copy as fetch» / «Copy as fetch (Node.js)» — как в Chrome DevTools.
//! Браузерный вариант без заголовков, которые fetch выставить не даёт
//! (cookie, referer, host, user-agent…): Referer — полем `referrer`, куки —
//! `credentials: "include"`. Node.js — те же заголовки плюс `cookie` и
//! `Referer` (там запретов нет, а куки браузера не подставятся сами).
//! Строки — JSON-литералы (валидный JS); бинарное тело — `Uint8Array`.

use domain::value_objects::http_vo::HeaderEntry;

/// Для какой среды фрагмент.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchStyle {
    Browser,
    NodeJs,
}

/// Заголовки, которые Chrome не переносит в fetch (запрещённые для fetch
/// или выставляемые рантаймом сам).
const IGNORED_HEADERS: [&str; 22] = [
    "accept-charset",
    "accept-encoding",
    "access-control-request-headers",
    "access-control-request-method",
    "connection",
    "content-length",
    "cookie",
    "cookie2",
    "date",
    "dnt",
    "expect",
    "host",
    "keep-alive",
    "origin",
    "referer",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "via",
    "user-agent",
    "proxy-connection",
];

/// С этими заголовками браузерный вариант шлёт `credentials: "include"`.
const CREDENTIAL_HEADERS: [&str; 2] = ["cookie", "authorization"];

/// Вызов `fetch(…)` для запроса.
pub fn fetch_snippet(
    style: FetchStyle,
    method: &str,
    url: &str,
    headers: &[HeaderEntry],
    body: Option<&[u8]>,
) -> String {
    let referrer = HeaderEntry::find(headers, "referer").map(|h| h.value.as_str());
    let kept = merged(headers.iter().filter(|h| !is_ignored(h)));
    let extra: Vec<(String, String)> = match style {
        FetchStyle::Browser => Vec::new(),
        FetchStyle::NodeJs => merged(headers.iter().filter(|h| is_named(h, "cookie")))
            .into_iter()
            .chain(referrer.map(|r| ("Referer".to_string(), r.to_string())))
            .collect(),
    };
    let header_pairs: Vec<(String, String)> = kept.into_iter().chain(extra).collect();
    let browser_referrer = referrer.filter(|_| style == FetchStyle::Browser);
    let fields: Vec<String> = [
        Some(format!("\"headers\": {}", headers_object(&header_pairs))),
        browser_referrer.map(|r| format!("\"referrer\": {}", js_string(r))),
        Some(format!("\"body\": {}", body_literal(body))),
        Some(format!(
            "\"method\": {}",
            js_string(&method.to_ascii_uppercase())
        )),
    ]
    .into_iter()
    .flatten()
    .chain(browser_options(style, headers))
    .collect();
    format!(
        "fetch({}, {{\n  {}\n}});",
        js_string(url),
        fields.join(",\n  ")
    )
}

/// `mode` и `credentials` — только для браузера.
fn browser_options(style: FetchStyle, headers: &[HeaderEntry]) -> Vec<String> {
    if style == FetchStyle::NodeJs {
        return Vec::new();
    }
    let with_credentials = headers
        .iter()
        .any(|h| CREDENTIAL_HEADERS.iter().any(|name| is_named(h, name)));
    vec![
        "\"mode\": \"cors\"".to_string(),
        format!(
            "\"credentials\": \"{}\"",
            if with_credentials { "include" } else { "omit" }
        ),
    ]
}

fn is_named(header: &HeaderEntry, name: &str) -> bool {
    header.name.eq_ignore_ascii_case(name)
}

/// Игнорируемые и псевдозаголовки HTTP/2 (`:authority`…).
fn is_ignored(header: &HeaderEntry) -> bool {
    header.name.starts_with(':') || IGNORED_HEADERS.iter().any(|name| is_named(header, name))
}

/// Повторы одного имени — в одно значение (у объекта ключ один): через
/// `, `, у cookie — через `; `. Порядок — по первому вхождению.
fn merged<'a>(headers: impl Iterator<Item = &'a HeaderEntry>) -> Vec<(String, String)> {
    let headers: Vec<&HeaderEntry> = headers.collect();
    headers
        .iter()
        .enumerate()
        .filter(|(i, h)| !headers[..*i].iter().any(|p| is_named(p, &h.name)))
        .map(|(_, h)| {
            let separator = if is_named(h, "cookie") { "; " } else { ", " };
            let value = headers
                .iter()
                .filter(|p| is_named(p, &h.name))
                .map(|p| p.value.as_str())
                .collect::<Vec<_>>()
                .join(separator);
            (h.name.clone(), value)
        })
        .collect()
}

fn headers_object(pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return "{}".into();
    }
    let lines: Vec<String> = pairs
        .iter()
        .map(|(name, value)| format!("    {}: {}", js_string(name), js_string(value)))
        .collect();
    format!("{{\n{}\n  }}", lines.join(",\n"))
}

/// `null`, строка или `new Uint8Array([…])` для не-UTF-8.
fn body_literal(body: Option<&[u8]>) -> String {
    match body
        .filter(|b| !b.is_empty())
        .map(|b| (b, std::str::from_utf8(b)))
    {
        None => "null".into(),
        Some((_, Ok(text))) => js_string(text),
        Some((bytes, Err(_))) => {
            let list: Vec<String> = bytes.iter().map(u8::to_string).collect();
            format!("new Uint8Array([{}])", list.join(", "))
        }
    }
}

/// JSON-строка — валидный строковый литерал JS (ES2019+).
fn js_string(text: &str) -> String {
    serde_json::Value::String(text.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(list: &[(&str, &str)]) -> Vec<HeaderEntry> {
        list.iter().map(|(n, v)| HeaderEntry::new(*n, *v)).collect()
    }

    fn browser_request() -> Vec<HeaderEntry> {
        headers(&[
            ("Host", "api.test"),
            ("accept", "application/json"),
            ("User-Agent", "Mozilla/5.0"),
            ("Cookie", "a=1"),
            ("cookie", "b=2"),
            ("Referer", "https://app.test/page"),
            ("Accept-Encoding", "gzip"),
        ])
    }

    #[test]
    fn browser_get_matches_devtools_layout() {
        let snippet = fetch_snippet(
            FetchStyle::Browser,
            "get",
            "https://api.test/v1?q=1",
            &browser_request(),
            None,
        );
        assert_eq!(
            snippet,
            "fetch(\"https://api.test/v1?q=1\", {\n  \"headers\": {\n    \"accept\": \"application/json\"\n  },\n  \"referrer\": \"https://app.test/page\",\n  \"body\": null,\n  \"method\": \"GET\",\n  \"mode\": \"cors\",\n  \"credentials\": \"include\"\n});"
        );
    }

    #[test]
    fn node_keeps_cookie_and_referer_without_browser_options() {
        let snippet = fetch_snippet(
            FetchStyle::NodeJs,
            "GET",
            "https://api.test/",
            &browser_request(),
            None,
        );
        assert_eq!(
            snippet,
            "fetch(\"https://api.test/\", {\n  \"headers\": {\n    \"accept\": \"application/json\",\n    \"Cookie\": \"a=1; b=2\",\n    \"Referer\": \"https://app.test/page\"\n  },\n  \"body\": null,\n  \"method\": \"GET\"\n});"
        );
    }

    #[test]
    fn text_body_is_escaped_and_duplicates_are_joined() {
        let snippet = fetch_snippet(
            FetchStyle::Browser,
            "POST",
            "https://api.test/items",
            &headers(&[
                ("Content-Type", "application/json"),
                ("Content-Length", "20"),
                ("X-Tag", "a"),
                ("x-tag", "b"),
            ]),
            Some("{\"name\":\"O'Neil\"}\n".as_bytes()),
        );
        assert!(snippet.contains(r#""X-Tag": "a, b""#), "{snippet}");
        assert!(!snippet.contains("Content-Length"), "{snippet}");
        assert!(
            snippet.contains(r#""body": "{\"name\":\"O'Neil\"}\n","#),
            "{snippet}"
        );
        assert!(snippet.contains(r#""credentials": "omit""#), "{snippet}");
        assert!(!snippet.contains("referrer"), "{snippet}");
    }

    #[test]
    fn binary_body_becomes_uint8array_and_empty_headers_are_braces() {
        let snippet = fetch_snippet(
            FetchStyle::NodeJs,
            "PUT",
            "http://a.test/x",
            &[],
            Some(&[0x00, 0xff, b'a']),
        );
        assert!(snippet.contains("\"headers\": {},"), "{snippet}");
        assert!(
            snippet.contains("\"body\": new Uint8Array([0, 255, 97]),"),
            "{snippet}"
        );
    }

    #[test]
    fn authorization_means_credentials_and_pseudo_headers_are_dropped() {
        let snippet = fetch_snippet(
            FetchStyle::Browser,
            "DELETE",
            "https://api.test/1",
            &headers(&[(":authority", "api.test"), ("Authorization", "Bearer t")]),
            Some(b""),
        );
        assert!(!snippet.contains(":authority"), "{snippet}");
        assert!(
            snippet.contains(r#""Authorization": "Bearer t""#),
            "{snippet}"
        );
        assert!(snippet.contains(r#""credentials": "include""#), "{snippet}");
        assert!(snippet.contains("\"body\": null"), "{snippet}");
    }
}
