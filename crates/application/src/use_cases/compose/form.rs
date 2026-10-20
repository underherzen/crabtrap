//! Форма окна Compose: метод, URL, заголовки строками `Name: value`, тело
//! текстом. Edit & Resend заполняет её из захваченного обмена.

use domain::entities::exchange::{RequestHead, RequestOrigin};

use crate::dto::capture::HeaderEntryDto;
use crate::dto::compose::ComposedRequestDto;

/// Заголовки, которые прокси выставит сам (из захвата их не переносим).
const MANAGED_HEADERS: &[&str] = &[
    "content-length",
    "transfer-encoding",
    "proxy-connection",
    "connection",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposeForm {
    pub method: String,
    pub url: String,
    /// По строке на заголовок: `Name: value`.
    pub headers: String,
    pub body: String,
    /// Тело, которое нельзя править текстом (двоичное или сжатое по
    /// Content-Encoding): уходит как захвачено, `body` игнорируется.
    pub fixed_body: Option<Vec<u8>>,
}

impl ComposeForm {
    /// Пустой запрос.
    pub fn blank() -> Self {
        Self {
            method: "GET".into(),
            url: "https://".into(),
            headers: "Accept: */*".into(),
            body: String::new(),
            fixed_body: None,
        }
    }

    /// Из захваченного запроса (`body` — байты тела запроса).
    pub fn from_head(head: &RequestHead, body: Option<&[u8]>) -> Self {
        let encoded = head.headers.iter().any(|h| {
            h.name.eq_ignore_ascii_case("content-encoding")
                && !h.value.trim().eq_ignore_ascii_case("identity")
        });
        let text = body
            .filter(|_| !encoded)
            .and_then(|b| std::str::from_utf8(b).ok());
        let headers = head
            .headers
            .iter()
            .filter(|h| !MANAGED_HEADERS.contains(&h.name.to_ascii_lowercase().as_str()))
            .map(|h| format!("{}: {}", h.name, h.value))
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            method: head.method.to_string(),
            url: head.uri.clone(),
            headers,
            body: text.unwrap_or_default().to_string(),
            fixed_body: match text {
                Some(_) => None,
                None => body.filter(|b| !b.is_empty()).map(<[u8]>::to_vec),
            },
        }
    }

    /// Запрос для отправки или текст ошибки.
    pub fn to_request(&self, origin: RequestOrigin) -> Result<ComposedRequestDto, String> {
        let method = self.method.trim().to_ascii_uppercase();
        if !is_token(&method) {
            return Err("Method must be an HTTP token, e.g. GET".into());
        }
        let url = self.url.trim();
        let parsed = url::Url::parse(url).map_err(|e| format!("URL is invalid: {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err("URL must be http(s)://host/…".into());
        }
        let headers = parse_header_lines(&self.headers)?;
        Ok(ComposedRequestDto {
            method,
            url: url.to_string(),
            headers,
            body: match &self.fixed_body {
                Some(bytes) => Some(bytes.clone()),
                None => (!self.body.is_empty()).then(|| self.body.clone().into_bytes()),
            },
            origin,
        })
    }
}

/// HTTP-токен (метод): `GET`, `M-SEARCH`, …
pub fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c))
}

/// Строки `Name: value` (пустые — пропуск).
pub fn parse_header_lines(text: &str) -> Result<Vec<HeaderEntryDto>, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(index, line)| {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| format!("Header line {} needs 'Name: value'", index + 1))?;
            let name = name.trim();
            if name.is_empty() || name.contains(char::is_whitespace) {
                return Err(format!("Header line {}: invalid name '{name}'", index + 1));
            }
            Ok(HeaderEntryDto {
                name: name.to_string(),
                value: value.trim().to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use domain::entities::exchange::{HttpExchange, HttpRequest};
    use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
    use domain::value_objects::{ConnectionId, Host};

    #[test]
    fn exchange_to_form_and_back() {
        let exchange = HttpExchange::new_pending(
            ConnectionId::new(),
            Host::new("api.test").expect("host"),
            HttpRequest {
                method: Method::new("POST"),
                uri: "https://api.test/items".into(),
                version: HttpVersionVo::Http11,
                headers: vec![
                    HeaderEntry {
                        name: "Content-Type".into(),
                        value: "application/json".into(),
                    },
                    HeaderEntry {
                        name: "Content-Length".into(),
                        value: "7".into(),
                    },
                ],
                body: None,
                raw_size: 0,
                origin: Default::default(),
            },
            Protocol::Http1,
            Utc::now(),
        );
        let form = ComposeForm::from_head(&exchange.replay_head(), Some(br#"{"a":1}"#));
        assert_eq!(form.headers, "Content-Type: application/json");
        let request = form.to_request(RequestOrigin::Compose).expect("valid");
        assert_eq!(request.method, "POST");
        assert_eq!(request.body.as_deref(), Some(&br#"{"a":1}"#[..]));
        assert_eq!(request.headers.len(), 1);
    }

    #[test]
    fn invalid_inputs_are_reported() {
        let form = |method: &str, url: &str, headers: &str| ComposeForm {
            method: method.into(),
            url: url.into(),
            headers: headers.into(),
            body: String::new(),
            fixed_body: None,
        };
        assert!(
            form("GE T", "https://a.test/", "")
                .to_request(RequestOrigin::Compose)
                .is_err()
        );
        assert!(
            form("GET", "ftp://a.test/", "")
                .to_request(RequestOrigin::Compose)
                .is_err()
        );
        assert!(
            form("GET", "https://a.test/", "no colon")
                .to_request(RequestOrigin::Compose)
                .is_err()
        );
        assert!(
            form("get", "https://a.test/", "\n X-A: 1 \n\n")
                .to_request(RequestOrigin::Compose)
                .is_ok()
        );
        assert!(
            form("M-SEARCH", "https://a.test/", "")
                .to_request(RequestOrigin::Compose)
                .is_ok()
        );
    }

    #[test]
    fn binary_or_compressed_body_is_kept_as_captured() {
        let head = |headers: Vec<HeaderEntry>| RequestHead {
            method: Method::new("POST"),
            uri: "https://a.test/u".into(),
            headers,
            body: None,
            body_lost: false,
        };
        let binary = ComposeForm::from_head(&head(Vec::new()), Some(&[0xff, 0x00]));
        assert_eq!(binary.fixed_body.as_deref(), Some(&[0xff, 0x00][..]));
        let gz = HeaderEntry {
            name: "Content-Encoding".into(),
            value: "gzip".into(),
        };
        let compressed = ComposeForm::from_head(&head(vec![gz]), Some(b"abc"));
        assert_eq!(compressed.body, "");
        let request = compressed.to_request(RequestOrigin::Repeat).expect("valid");
        assert_eq!(request.body.as_deref(), Some(&b"abc"[..]));
    }
}
