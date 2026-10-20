//! Поля ответа Map Local в диалоге: Status, Content-Type и Headers. Пустой
//! Status — 200, пустой Content-Type — по расширению файла, Headers — строки
//! `Name: value`. Ошибки — по полям.

use std::ops::RangeInclusive;

use domain::value_objects::http_vo::HeaderEntry;

pub use super::edit::MapLocalResponse;
use crate::use_cases::compose::form::{is_token, parse_header_lines};

/// Коды ответа Map Local: только финальные (1xx клиенту не отдать).
pub const MAP_LOCAL_STATUS_RANGE: RangeInclusive<u16> = 200..=599;

/// Заголовки, которые выставляет только движок: фрейминг и hop-by-hop
/// (иначе клиент неверно прочтёт тело/соединение).
const RESERVED_MAP_LOCAL_HEADERS: [&str; 8] = [
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
];

/// Значения полей (как введены).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseForm {
    pub status: String,
    pub content_type: String,
    /// Строки `Name: value`.
    pub headers: String,
}

/// Ошибки по полям (None — поле в порядке).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResponseErrors {
    pub status: Option<String>,
    pub content_type: Option<String>,
    pub headers: Option<String>,
}

impl Default for ResponseForm {
    fn default() -> Self {
        Self::from_response(&MapLocalResponse::default())
    }
}

impl ResponseForm {
    pub fn from_response(response: &MapLocalResponse) -> Self {
        Self {
            status: response.status.to_string(),
            content_type: response.content_type.clone().unwrap_or_default(),
            headers: response
                .headers
                .iter()
                .map(|h| format!("{}: {}\n", h.name, h.value))
                .collect(),
        }
    }

    /// Ответ из полей; `Err` — ошибки под нужными полями.
    pub fn to_response(&self) -> Result<MapLocalResponse, ResponseErrors> {
        let status = match self.status.trim() {
            "" => Ok(MapLocalResponse::default().status),
            s => s
                .parse::<u16>()
                .ok()
                .filter(|code| MAP_LOCAL_STATUS_RANGE.contains(code))
                .ok_or_else(|| "Status must be a number 200–599".to_string()),
        };
        let content_type = match self.content_type.trim() {
            "" => Ok(None),
            ct => content_type_error(ct)
                .map_or_else(|| Ok(Some(ct.to_string())), |e| Err(e.to_string())),
        };
        let headers = parse_headers(&self.headers);
        match (status, content_type, headers) {
            (Ok(status), Ok(content_type), Ok(headers)) => Ok(MapLocalResponse {
                status,
                content_type,
                headers,
            }),
            (status, content_type, headers) => Err(ResponseErrors {
                status: status.err(),
                content_type: content_type.err(),
                headers: headers.err(),
            }),
        }
    }
}

/// Строки `Name: value` → заголовки; ошибка называет строку.
fn parse_headers(text: &str) -> Result<Vec<HeaderEntry>, String> {
    parse_header_lines(text)?
        .into_iter()
        .enumerate()
        .map(
            |(index, h)| match map_local_header_error(&h.name, &h.value) {
                Some(e) => Err(format!("Header line {}: {e}", index + 1)),
                None => Ok(HeaderEntry::new(h.name, h.value)),
            },
        )
        .collect()
}

/// Почему заголовок нельзя добавить в ответ Map Local (`None` — можно):
/// имя — HTTP-токен не из зарезервированных (Content-Type — своё поле),
/// значение — печатный ASCII (и Tab) без переводов строк.
pub fn map_local_header_error(name: &str, value: &str) -> Option<String> {
    if !is_token(name) {
        return Some(format!("invalid header name '{name}'"));
    }
    if name.eq_ignore_ascii_case("content-type") {
        return Some("set Content-Type in its own field".into());
    }
    if RESERVED_MAP_LOCAL_HEADERS
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
    {
        return Some(format!("{name} is set by crabtrap and can't be overridden"));
    }
    if !value
        .bytes()
        .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
    {
        return Some(format!("{name}: value must be plain ASCII on one line"));
    }
    None
}

/// Почему значение не годится в заголовок Content-Type (`None` — годится):
/// печатный ASCII без переводов строк и `type/subtype` с непустыми частями.
pub fn content_type_error(value: &str) -> Option<&'static str> {
    if !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Some("Content-Type must be plain ASCII on one line");
    }
    let mime = value.split(';').next().unwrap_or_default().trim();
    match mime.split_once('/') {
        Some((kind, sub)) if !kind.is_empty() && !sub.is_empty() && !mime.contains(' ') => None,
        _ => Some("Content-Type looks like type/subtype, e.g. application/json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(status: &str, content_type: &str) -> ResponseForm {
        ResponseForm {
            status: status.into(),
            content_type: content_type.into(),
            headers: String::new(),
        }
    }

    #[test]
    fn default_form_is_200_with_auto_content_type() {
        assert_eq!(
            ResponseForm::default().to_response(),
            Ok(MapLocalResponse::default())
        );
        assert_eq!(
            MapLocalResponse::default(),
            MapLocalResponse {
                status: 200,
                content_type: None,
                headers: Vec::new(),
            }
        );
    }

    #[test]
    fn fields_are_trimmed_and_empty_means_default() {
        assert_eq!(
            form(" 404 ", " text/plain; charset=utf-8 ").to_response(),
            Ok(MapLocalResponse {
                status: 404,
                content_type: Some("text/plain; charset=utf-8".into()),
                headers: Vec::new(),
            })
        );
        assert_eq!(form("", "").to_response(), Ok(MapLocalResponse::default()));
    }

    #[test]
    fn bad_status_is_reported_on_its_field() {
        for status in ["abc", "99", "101", "600", "-1"] {
            let errors = form(status, "").to_response().expect_err(status);
            assert!(errors.status.is_some(), "{status}");
            assert!(errors.content_type.is_none(), "{status}");
        }
    }

    #[test]
    fn bad_content_type_is_reported_on_its_field() {
        for ct in ["json", "text/plain\r\nX-Evil: 1", "тип/x", "/json", "text/"] {
            let errors = form("200", ct).to_response().expect_err(ct);
            assert!(errors.content_type.is_some(), "{ct:?}");
            assert!(errors.status.is_none(), "{ct:?}");
        }
    }

    #[test]
    fn round_trips_through_response() {
        let response = MapLocalResponse {
            status: 500,
            content_type: Some("application/problem+json".into()),
            headers: Vec::new(),
        };
        assert_eq!(
            ResponseForm::from_response(&response).to_response(),
            Ok(response)
        );
        assert_eq!(
            ResponseForm::from_response(&MapLocalResponse::default()),
            ResponseForm::default()
        );
    }

    #[test]
    fn header_lines_become_ordered_headers() {
        let form = ResponseForm {
            headers: "Access-Control-Allow-Origin: *\n\n  Set-Cookie: a=1 \nSet-Cookie: b=2".into(),
            ..ResponseForm::default()
        };
        assert_eq!(
            form.to_response().map(|r| r.headers),
            Ok(vec![
                HeaderEntry::new("Access-Control-Allow-Origin", "*"),
                HeaderEntry::new("Set-Cookie", "a=1"),
                HeaderEntry::new("Set-Cookie", "b=2"),
            ])
        );
    }

    #[test]
    fn bad_header_lines_are_reported_on_their_field() {
        for headers in [
            "no colon here",
            "Bad Name: x",
            "Content-Length: 5",
            "transfer-encoding: chunked",
            "Connection: close",
            "Content-Type: text/plain",
            "X-Ok: caf\u{e9}",
        ] {
            let form = ResponseForm {
                headers: headers.into(),
                ..ResponseForm::default()
            };
            let errors = form.to_response().expect_err(headers);
            assert!(errors.headers.is_some(), "{headers:?}");
            assert!(errors.status.is_none(), "{headers:?}");
        }
    }

    #[test]
    fn headers_round_trip_through_response() {
        let response = MapLocalResponse {
            headers: vec![
                HeaderEntry::new("Cache-Control", "no-store"),
                HeaderEntry::new("X-Mock", "1"),
            ],
            ..MapLocalResponse::default()
        };
        assert_eq!(
            ResponseForm::from_response(&response).to_response(),
            Ok(response)
        );
    }

    #[test]
    fn framing_and_content_type_headers_are_reserved() {
        for name in [
            "Content-Length",
            "TRANSFER-ENCODING",
            "Connection",
            "Keep-Alive",
            "Proxy-Connection",
            "TE",
            "Trailer",
            "Upgrade",
            "Content-Type",
        ] {
            assert!(map_local_header_error(name, "x").is_some(), "{name}");
        }
        assert_eq!(map_local_header_error("Content-Encoding", "gzip"), None);
        assert_eq!(map_local_header_error("X-Tab", "a\tb"), None);
        assert!(map_local_header_error("X-Evil", "a\r\nX-Injected: 1").is_some());
        assert!(map_local_header_error("", "x").is_some());
    }

    #[test]
    fn content_type_check_is_shared_with_rule_validation() {
        assert_eq!(content_type_error("application/json"), None);
        assert!(content_type_error("").is_some());
    }
}
