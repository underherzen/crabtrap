//! DTO инспектора обмена (Phase 5).

use serde::{Deserialize, Serialize};

use domain::entities::BodyRef;
use domain::entities::exchange::{HttpRequest, HttpResponse, RequestOrigin};
use domain::value_objects::ConnectionId;
use domain::value_objects::http_vo::HttpVersionVo;
use domain::value_objects::timing::TimingPhases;
use domain::value_objects::tls::TlsInfo;

use crate::dto::capture::{ExchangeSummaryDto, HeaderEntryDto};

/// Полный обмен для панели деталей.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExchangeDetailDto {
    pub summary: ExchangeSummaryDto,
    pub request: RequestDto,
    pub response: Option<ResponseDto>,
    pub timing: TimingPhases,
    pub tls: Option<TlsInfo>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestDto {
    pub method: String,
    pub uri: String,
    pub version: HttpVersionVo,
    pub headers: Vec<HeaderEntryDto>,
    #[serde(default)]
    pub body: Option<BodyRef>,
    pub raw_size: u64,
    pub origin: RequestOrigin,
}

impl From<HttpRequest> for RequestDto {
    fn from(req: HttpRequest) -> Self {
        Self {
            method: req.method.to_string(),
            uri: req.uri,
            version: req.version,
            headers: req.headers.into_iter().map(Into::into).collect(),
            body: req.body,
            raw_size: req.raw_size,
            origin: req.origin,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseDto {
    pub status: u16,
    pub reason: Option<String>,
    pub version: HttpVersionVo,
    pub headers: Vec<HeaderEntryDto>,
    #[serde(default)]
    pub body: Option<BodyRef>,
    pub raw_size: u64,
}

impl From<HttpResponse> for ResponseDto {
    fn from(resp: HttpResponse) -> Self {
        Self {
            status: resp.status,
            reason: resp.reason,
            version: resp.version,
            headers: resp.headers.into_iter().map(Into::into).collect(),
            body: resp.body,
            raw_size: resp.raw_size,
        }
    }
}

/// Идентификатор соединения (для группировки обменов одного коннекта).
pub fn connection_of(summary: &ExchangeSummaryDto) -> ConnectionId {
    summary.connection_id
}

// ---------------------------------------------------------------------------
// Cookies (Phase 5): парсинг Cookie (request) и Set-Cookie (response).
//
// Живут в application DTO, а не в domain: это презентационная проекция
// заголовков для вкладки Cookies (домену cookie-семантика не нужна — правила
// их не матчат; заголовки в HttpExchange остаются нетронутыми строками).
// ---------------------------------------------------------------------------

/// Один cookie: имя/значение + атрибуты (только для Set-Cookie).
///
/// Атрибуты — пары (имя, значение) как на проводе; флаги без значения
/// (Secure, HttpOnly) идут с пустой строкой. Регистр имён атрибутов
/// сохранён как пришёл (матчинг — case-insensitive по месту).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CookieEntry {
    pub name: String,
    pub value: String,
    pub attrs: Vec<(String, String)>,
}

impl CookieEntry {
    /// Значение атрибута по имени (case-insensitive); `Some("")` для флага.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn is_secure(&self) -> bool {
        self.attr("secure").is_some()
    }

    pub fn is_http_only(&self) -> bool {
        self.attr("httponly").is_some()
    }
}

/// Обёртывающие двойные кавычки значения снимаем для отображения
/// (`a="b"` → `b`; RFC 6265 допускает DQUOTE вокруг cookie-value).
fn unquote(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

/// Парсинг заголовка `Cookie` (request): `name1=value1; name2=value2`.
///
/// Сегменты без `=` пропускаются (RFC 6265 §5.4 их игнорирует).
/// Атрибутов у request-cookie нет — `attrs` пуст.
pub fn parse_cookies(header: &str) -> Vec<CookieEntry> {
    header
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(name, _)| !name.trim().is_empty())
        .map(|(name, value)| CookieEntry {
            name: name.trim().to_string(),
            value: unquote(value).to_string(),
            attrs: Vec::new(),
        })
        .collect()
}

/// Парсинг заголовка `Set-Cookie` (response):
/// `name=value; Expires=...; Path=/; Domain=...; Secure; HttpOnly; SameSite=Lax`.
///
/// Первый сегмент — имя/значение; остальные — атрибуты: `Key=Value` или
/// флаг `Key` (значение ""). Непарсимый первый сегмент (нет `=`) → None.
pub fn parse_set_cookie(header: &str) -> Option<CookieEntry> {
    let mut segments = header.split(';');
    let (name, value) = segments.next()?.split_once('=')?;
    if name.trim().is_empty() {
        return None;
    }
    let attrs = segments
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| match s.split_once('=') {
            Some((k, v)) => (k.trim().to_string(), unquote(v).to_string()),
            None => (s.to_string(), String::new()),
        })
        .collect();
    Some(CookieEntry {
        name: name.trim().to_string(),
        value: unquote(value).to_string(),
        attrs,
    })
}

/// Стандартная reason-фраза статуса (HTTP/1.1, RFC 9110 и друзья).
///
/// Крошечная match-таблица вместо зависимости `http`: крейт `http` не
/// входит в зависимости application, а ради статических строк тянуть его
/// незачем (engine уже зависит от `http` для своих нужд). None — редкий/
/// нестандартный код: UI показывает только число.
pub fn canonical_reason(status: u16) -> Option<&'static str> {
    Some(match status {
        // 2xx
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        // 3xx
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        // 4xx
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "I'm a teapot",
        422 => "Unprocessable Content",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        // 5xx
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_cookie_header() {
        let cookies = parse_cookies("session=abc123; theme=dark; empty=");
        assert_eq!(cookies.len(), 3);
        assert_eq!(cookies[0].name, "session");
        assert_eq!(cookies[0].value, "abc123");
        assert!(cookies[0].attrs.is_empty());
        assert_eq!(cookies[2].value, "");
    }

    #[test]
    fn request_cookie_skips_malformed_and_strips_quotes() {
        let cookies = parse_cookies("bogus; a=1; quoted=\"two words\"");
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[1].name, "quoted");
        assert_eq!(cookies[1].value, "two words");
    }

    #[test]
    fn set_cookie_with_all_attrs() {
        let c = parse_set_cookie(
            "id=u7; Expires=Wed, 21 Oct 2026 07:28:00 GMT; Path=/api; \
             Domain=example.com; Secure; HttpOnly; SameSite=Lax",
        )
        .expect("valid set-cookie");
        assert_eq!(c.name, "id");
        assert_eq!(c.value, "u7");
        assert_eq!(c.attr("expires"), Some("Wed, 21 Oct 2026 07:28:00 GMT"));
        assert_eq!(c.attr("path"), Some("/api"));
        assert_eq!(c.attr("domain"), Some("example.com"));
        assert_eq!(c.attr("samesite"), Some("Lax"));
        assert!(c.is_secure());
        assert!(c.is_http_only());
    }

    #[test]
    fn set_cookie_value_and_attr_unquote() {
        // `;` не может встречаться внутри значения (cookie-octet, RFC 6265),
        // поэтому split(';') корректен и для кавычек.
        let c = parse_set_cookie("token=\"a b\"; Comment=\"hello world\"").unwrap();
        assert_eq!(c.value, "a b");
        assert_eq!(c.attr("comment"), Some("hello world"));
    }

    #[test]
    fn set_cookie_minimal_and_invalid() {
        let minimal = parse_set_cookie("k=v").expect("minimal set-cookie");
        assert_eq!((minimal.name.as_str(), minimal.value.as_str()), ("k", "v"));
        assert!(parse_set_cookie("no-value").is_none());
        assert!(parse_set_cookie("").is_none());
        // Имя из пробелов — невалидно.
        assert!(parse_set_cookie(" =v").is_none());
    }

    #[test]
    fn reasons_known_and_unknown() {
        assert_eq!(canonical_reason(200), Some("OK"));
        assert_eq!(canonical_reason(304), Some("Not Modified"));
        assert_eq!(canonical_reason(404), Some("Not Found"));
        assert_eq!(canonical_reason(418), Some("I'm a teapot"));
        assert_eq!(canonical_reason(599), None);
        assert_eq!(canonical_reason(0), None);
    }
}
