//! Модель формы диалога «Edit Location» (Tools): поля Host / Path / Method
//! (или regex по URL), «Ignore query», имя GraphQL-операции — вместо одной
//! строки с неочевидным синтаксисом. Ошибки — по полям, чтобы UI показывал
//! их под нужным полем.

use domain::value_objects::TrafficMatch;
use domain::value_objects::regex_cache::whole_match_check;

use super::edit::parse_location;
use super::update_rule_set::MAX_URL_REGEX_CHARS;
use crate::dto::capture::ExchangeSummaryDto;
use crate::matching::RequestTarget;

/// Методы для выпадающего списка (`None` — любой).
pub const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// Значения полей location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationForm {
    pub host: String,
    pub path: String,
    pub method: Option<String>,
    /// Режим Regex: условие — `url_regex` по полному URL, Host/Path не нужны.
    pub use_regex: bool,
    pub url_regex: String,
    /// Path сравнивается без query (`/users` ловит `/users?page=2`).
    pub ignore_query: bool,
    /// Имя GraphQL-операции (glob); пусто — не проверять.
    pub graphql_operation: String,
}

/// Новое правило: query по умолчанию не учитывается — так ведут себя
/// Proxyman/Charles, и `/users` ловит `/users?page=2`.
impl Default for LocationForm {
    fn default() -> Self {
        Self {
            host: String::new(),
            path: String::new(),
            method: None,
            use_regex: false,
            url_regex: String::new(),
            ignore_query: true,
            graphql_operation: String::new(),
        }
    }
}

/// Ошибки по полям (None — поле в порядке).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocationErrors {
    pub host: Option<String>,
    pub path: Option<String>,
    pub url_regex: Option<String>,
    pub graphql_operation: Option<String>,
}

impl LocationErrors {
    pub fn is_empty(&self) -> bool {
        self.host.is_none()
            && self.path.is_none()
            && self.url_regex.is_none()
            && self.graphql_operation.is_none()
    }

    /// Все ошибки одной строкой (для окна без полей под каждую ошибку).
    pub fn summary(&self) -> String {
        [
            &self.host,
            &self.path,
            &self.url_regex,
            &self.graphql_operation,
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join("; ")
    }
}

/// Итог проверки URL правилом («Test URL» в диалоге). `checked` — URL в
/// том виде, в каком его видит правило (хост в нижнем регистре, без порта
/// по умолчанию, `/` у пустого пути): regex сверяется именно с ним.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlCheck {
    Matches {
        checked: String,
    },
    NoMatch {
        checked: String,
    },
    /// Поля формы или URL с ошибкой — текст для пользователя.
    Invalid(String),
}

impl LocationForm {
    /// Поля из сохранённого условия (правка правила).
    pub fn from_match(m: &TrafficMatch) -> Self {
        Self {
            host: m.host_glob.clone().unwrap_or_default(),
            path: m.path_glob.clone().unwrap_or_default(),
            method: m.method.clone(),
            use_regex: m.url_regex.is_some(),
            url_regex: m.url_regex.clone().unwrap_or_default(),
            ignore_query: m.ignore_query,
            graphql_operation: m.graphql_operation.clone().unwrap_or_default(),
        }
    }

    /// Подстановка из обмена в трафике (правый клик): точный хост, path
    /// без query (и «Ignore query» — правило ловит этот путь с любым query),
    /// метод запроса.
    pub fn from_exchange(exchange: &ExchangeSummaryDto) -> Self {
        let path = exchange.path.split('?').next().unwrap_or_default();
        Self {
            host: exchange.host.clone(),
            path: path.to_string(),
            method: Some(exchange.method.to_ascii_uppercase()),
            ..Self::default()
        }
    }

    /// Вставили в Host полный URL (`https://api.x.com/v1/*`) — разложить на
    /// Host и Path; иначе форма как есть.
    pub fn normalized(self) -> Self {
        let host = self.host.trim();
        if !(host.contains("://") || host.contains('/')) {
            // Порт отбрасывается: движок сравнивает хост без порта.
            return Self {
                host: strip_port(host).to_string(),
                ..self
            };
        }
        match parse_location(host) {
            Ok(m) => Self {
                host: m.host_glob.unwrap_or_default(),
                path: m.path_glob.unwrap_or(self.path),
                ..self
            },
            Err(_) => self,
        }
    }

    pub fn validate(&self) -> LocationErrors {
        let graphql_operation = self
            .graphql_operation
            .trim()
            .contains(char::is_whitespace)
            .then(|| "GraphQL operation is one name, e.g. GetUser or Get*".to_string());
        if self.use_regex {
            return LocationErrors {
                url_regex: self.regex_error(),
                graphql_operation,
                ..LocationErrors::default()
            };
        }
        let host = self.host.trim();
        let path = self.path.trim();
        LocationErrors {
            graphql_operation,
            host: if host.is_empty() {
                Some("Host is required (e.g. api.example.com or *.example.com)".into())
            } else if host.contains(char::is_whitespace) || host.contains('/') {
                Some("Host must not contain spaces or '/'".into())
            } else {
                None
            },
            path: (!path.is_empty() && !path.starts_with('/') && !path.starts_with('*'))
                .then(|| "Path must start with '/' (e.g. /api/*)".into()),
            url_regex: None,
        }
    }

    fn regex_error(&self) -> Option<String> {
        let pattern = self.url_regex.trim();
        if pattern.is_empty() {
            return Some(
                "URL regex is required (e.g. https://api\\.example\\.com/v[12]/.*)".into(),
            );
        }
        if pattern.chars().count() > MAX_URL_REGEX_CHARS {
            return Some(format!(
                "URL regex is longer than {MAX_URL_REGEX_CHARS} characters"
            ));
        }
        whole_match_check(pattern, false)
            .err()
            .map(|e| format!("Invalid regex: {e}"))
    }

    /// Условие правила; `Err` — ошибки по полям.
    pub fn to_match(&self) -> Result<TrafficMatch, LocationErrors> {
        let form = self.clone().normalized();
        let errors = form.validate();
        if !errors.is_empty() {
            return Err(errors);
        }
        let graphql_operation =
            Some(form.graphql_operation.trim().to_string()).filter(|op| !op.is_empty());
        let method = form.method.clone().filter(|m| !m.is_empty());
        if form.use_regex {
            return Ok(TrafficMatch {
                url_regex: Some(form.url_regex.trim().to_string()),
                method,
                graphql_operation,
                ..Default::default()
            });
        }
        let path = form.path.trim();
        Ok(TrafficMatch {
            host_glob: Some(form.host.trim().to_ascii_lowercase()),
            path_glob: match path {
                "" | "/*" | "*" => None,
                other => Some(other.to_string()),
            },
            ignore_query: form.ignore_query,
            method,
            graphql_operation,
            ..Default::default()
        })
    }

    /// Условие, где пустые Host и Path значат «любой запрос» (Rewrite):
    /// остаются метод и GraphQL-операция; ничего не задано — `None`.
    pub fn to_optional_match(&self) -> Result<Option<TrafficMatch>, LocationErrors> {
        if self.use_regex || !(self.host.trim().is_empty() && self.path.trim().is_empty()) {
            return self.to_match().map(Some);
        }
        let errors = LocationErrors {
            graphql_operation: self.validate().graphql_operation,
            ..LocationErrors::default()
        };
        if !errors.is_empty() {
            return Err(errors);
        }
        let m = TrafficMatch {
            method: self.method.clone().filter(|m| !m.is_empty()),
            graphql_operation: Some(self.graphql_operation.trim().to_string())
                .filter(|op| !op.is_empty()),
            ..TrafficMatch::default()
        };
        Ok((m != TrafficMatch::default()).then_some(m))
    }

    /// Совпадёт ли правило с URL — проверяется только URL (метод считается
    /// подходящим). Имя GraphQL-операции по URL не проверить — оно в теле.
    pub fn check_url(&self, url: &str) -> UrlCheck {
        let Ok(m) = self.to_match() else {
            return UrlCheck::Invalid("Fix the fields above first".into());
        };
        let method = self.method.as_deref().unwrap_or("GET");
        match RequestTarget::parse(url.trim(), method) {
            Some(target) => {
                let checked = target.url().to_string();
                match m.matches_except_graphql(&target.facts().target()) {
                    true => UrlCheck::Matches { checked },
                    false => UrlCheck::NoMatch { checked },
                }
            }
            None => UrlCheck::Invalid(
                "Enter a full URL, e.g. https://api.example.com/v1/users?page=2".into(),
            ),
        }
    }

    /// «GET api.example.com/v1/*» — что именно будет совпадать.
    pub fn preview(&self) -> String {
        let graphql = match self.graphql_operation.trim() {
            "" => String::new(),
            op => format!(" · GraphQL {op}"),
        };
        let method = self.method.as_deref().unwrap_or("ANY");
        if self.use_regex {
            return format!("{method} URL ~ {}{graphql}", self.url_regex.trim());
        }
        let form = self.clone().normalized();
        let host = if form.host.trim().is_empty() {
            "*"
        } else {
            form.host.trim()
        };
        let path = if form.path.trim().is_empty() {
            "/*"
        } else {
            form.path.trim()
        };
        let query = if form.ignore_query && !path.ends_with('*') {
            " (any query)"
        } else {
            ""
        };
        format!("{method} {host}{path}{query}{graphql}")
    }
}

/// `api.com:8443` → `api.com`; IPv6 в скобках и голый IPv6 не трогаются.
fn strip_port(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((h, port))
            if !h.is_empty() && !h.contains(':') && port.chars().all(|c| c.is_ascii_digit()) =>
        {
            h
        }
        _ => host,
    }
}

/// Цель Map Remote из полей (как в Charles): Protocol / Host / Port / Path.
/// `query` полей не имеет, но сохраняется при правке (иначе молча терялся).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTargetForm {
    pub https: bool,
    pub host: String,
    pub port: String,
    pub path: String,
    pub query: String,
}

impl RemoteTargetForm {
    /// Разобрать сохранённый URL цели обратно в поля.
    pub fn from_url(remote: &str) -> Self {
        // Без схемы («localhost:3000») Url видит схему «localhost» и пустой
        // хост — дополняем http://.
        let parsed = url::Url::parse(remote)
            .ok()
            .filter(|u| u.host_str().is_some_and(|h| !h.is_empty()))
            .or_else(|| url::Url::parse(&format!("http://{remote}")).ok());
        match parsed {
            Some(u) => Self {
                https: matches!(u.scheme(), "https" | "wss"),
                host: u.host_str().unwrap_or_default().to_string(),
                port: u.port().map(|p| p.to_string()).unwrap_or_default(),
                path: match u.path() {
                    "/" => String::new(),
                    p => p.to_string(),
                },
                query: u.query().unwrap_or_default().to_string(),
            },
            None => Self {
                https: false,
                host: remote.to_string(),
                port: String::new(),
                path: String::new(),
                query: String::new(),
            },
        }
    }

    /// URL цели; `Err` — понятная ошибка для поля.
    pub fn to_url(&self) -> Result<String, String> {
        let host = self.host.trim();
        if host.is_empty() || host.contains(char::is_whitespace) || host.contains('/') {
            return Err("Target host is required (e.g. localhost or staging.example.com)".into());
        }
        let port = self.port.trim();
        if !port.is_empty() && port.parse::<u16>().is_err() {
            return Err("Port must be a number 1–65535".into());
        }
        let path = self.path.trim();
        if !path.is_empty() && !path.starts_with('/') {
            return Err("Target path must start with '/'".into());
        }
        let scheme = if self.https { "https" } else { "http" };
        let port = if port.is_empty() {
            String::new()
        } else {
            format!(":{port}")
        };
        let query = match self.query.trim() {
            "" => String::new(),
            q => format!("?{q}"),
        };
        // Query без path: «http://x?q» — Url сам вставит «/».
        let path = if path.is_empty() && !query.is_empty() {
            "/"
        } else {
            path
        };
        Ok(format!("{scheme}://{host}{port}{path}{query}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use domain::value_objects::http_vo::Protocol;
    use domain::value_objects::{ConnectionId, ExchangeId};

    fn form(host: &str, path: &str) -> LocationForm {
        LocationForm {
            host: host.into(),
            path: path.into(),
            ignore_query: false,
            ..LocationForm::default()
        }
    }

    fn regex(pattern: &str) -> LocationForm {
        LocationForm {
            use_regex: true,
            url_regex: pattern.into(),
            ..LocationForm::default()
        }
    }

    #[test]
    fn new_rules_ignore_the_query_by_default() {
        assert!(LocationForm::default().ignore_query);
        let m = LocationForm {
            host: "api.com".into(),
            path: "/users".into(),
            ..LocationForm::default()
        }
        .to_match()
        .expect("valid");
        assert!(m.ignore_query);
    }

    #[test]
    fn regex_mode_builds_url_regex_without_host_and_path() {
        let m = LocationForm {
            method: Some("POST".into()),
            graphql_operation: " GetUser ".into(),
            host: "ignored.com".into(),
            ..regex(r" https://api\.com/.* ")
        }
        .to_match()
        .expect("valid");
        assert_eq!(m.url_regex.as_deref(), Some(r"https://api\.com/.*"));
        assert_eq!(
            (m.host_glob.as_deref(), m.path_glob.as_deref()),
            (None, None)
        );
        assert_eq!(m.method.as_deref(), Some("POST"));
        assert_eq!(m.graphql_operation.as_deref(), Some("GetUser"));
        assert_eq!(
            LocationForm::from_match(&m).url_regex,
            r"https://api\.com/.*"
        );
        assert!(LocationForm::from_match(&m).use_regex);
    }

    #[test]
    fn regex_and_graphql_errors_point_at_their_fields() {
        assert!(regex("").validate().url_regex.is_some());
        assert!(regex("(open").validate().url_regex.is_some());
        let errors = regex("(open").validate();
        assert!(errors.host.is_none(), "host is not used in regex mode");
        let spaced = LocationForm {
            graphql_operation: "Get User".into(),
            ..form("api.com", "")
        };
        assert!(spaced.validate().graphql_operation.is_some());
        assert!(spaced.to_match().is_err());
    }

    #[test]
    fn optional_match_keeps_method_and_graphql_without_a_location() {
        assert_eq!(LocationForm::default().to_optional_match(), Ok(None));
        let graphql_only = LocationForm {
            graphql_operation: "Get*".into(),
            ..LocationForm::default()
        }
        .to_optional_match()
        .expect("valid")
        .expect("a condition");
        assert_eq!(graphql_only.graphql_operation.as_deref(), Some("Get*"));
        assert_eq!(graphql_only.host_glob, None);
        assert!(
            regex("(open").to_optional_match().is_err(),
            "regex mode always needs a valid regex"
        );
    }

    #[test]
    fn check_url_tests_the_rule_against_a_url() {
        let users = LocationForm {
            host: "api.com".into(),
            path: "/v1/users".into(),
            ..LocationForm::default()
        };
        assert!(matches!(
            users.check_url("https://api.com/v1/users?page=2"),
            UrlCheck::Matches { .. }
        ));
        assert!(matches!(
            users.check_url("https://api.com/v1/orders"),
            UrlCheck::NoMatch { .. }
        ));
        assert!(matches!(
            users.check_url("api.com/v1"),
            UrlCheck::Invalid(_)
        ));
        assert!(matches!(
            form("", "").check_url("https://a.com/"),
            UrlCheck::Invalid(_)
        ));
        assert!(matches!(
            regex(r"https://api\.com/v\d+/.*").check_url("https://api.com/v2/x"),
            UrlCheck::Matches { .. }
        ));
    }

    #[test]
    fn check_url_reports_the_normalized_url_the_regex_sees() {
        let with_port = regex(r"https://api\.com:443/.*");
        assert_eq!(
            with_port.check_url("https://API.com:443"),
            UrlCheck::NoMatch {
                checked: "https://api.com/".into()
            },
            "the default port is dropped, the host lowercased, `/` added"
        );
    }

    #[test]
    fn pasted_url_in_host_is_split() {
        let f = form("https://API.example.com:8443/v1/users*", "").normalized();
        assert_eq!(
            (f.host.as_str(), f.path.as_str()),
            ("api.example.com", "/v1/users*")
        );
    }

    #[test]
    fn field_errors_point_at_the_field() {
        let errors = form("", "api").validate();
        assert!(errors.host.is_some());
        assert!(errors.path.is_some());
        assert!(form("api.com", "/x/*").validate().is_empty());
        assert!(form("a b", "").validate().host.is_some());
    }

    #[test]
    fn to_match_maps_wildcard_path_to_none_and_keeps_method() {
        let m = LocationForm {
            method: Some("POST".into()),
            ..form("Api.com", "/*")
        }
        .to_match()
        .expect("valid");
        assert_eq!(m.host_glob.as_deref(), Some("api.com"));
        assert_eq!(m.path_glob, None);
        assert_eq!(m.method.as_deref(), Some("POST"));
        assert!(form("", "").to_match().is_err());
    }

    #[test]
    fn round_trips_through_match() {
        let original = LocationForm {
            method: Some("GET".into()),
            ..form("api.com", "/v1/*")
        };
        let m = original.to_match().expect("valid");
        assert_eq!(LocationForm::from_match(&m), original);
    }

    #[test]
    fn preview_reads_like_a_rule() {
        assert_eq!(form("api.com", "").preview(), "ANY api.com/*");
        let f = LocationForm {
            method: Some("GET".into()),
            ..form("api.com", "/v1/*")
        };
        assert_eq!(f.preview(), "GET api.com/v1/*");
        let any_query = LocationForm {
            ignore_query: true,
            graphql_operation: "GetUser".into(),
            ..form("api.com", "/graphql")
        };
        assert_eq!(
            any_query.preview(),
            "ANY api.com/graphql (any query) · GraphQL GetUser"
        );
        assert_eq!(
            regex("https://a\\.com/.*").preview(),
            "ANY URL ~ https://a\\.com/.*"
        );
    }

    #[test]
    fn prefill_from_exchange_drops_query() {
        let summary = ExchangeSummaryDto {
            id: ExchangeId::new(),
            connection_id: ConnectionId::new(),
            started_at: Utc::now(),
            method: "post".into(),
            scheme: "https".into(),
            host: "api.com".into(),
            path: "/v1/users?id=7".into(),
            status: Some(200),
            req_size: 0,
            resp_size: 0,
            duration_ms: 0.0,
            protocol: Protocol::Http1,
            tls_mitm: Some(true),
            tags: Vec::new(),
        };
        let f = LocationForm::from_exchange(&summary);
        // Query отброшен, «Ignore query» — правило ловит этот путь с любым query.
        assert_eq!(
            (f.host.as_str(), f.path.as_str(), f.method.as_deref()),
            ("api.com", "/v1/users", Some("POST"))
        );
        assert!(f.ignore_query);
        let plain = LocationForm::from_exchange(&ExchangeSummaryDto {
            path: "/v1/users".into(),
            ..summary
        });
        assert_eq!(plain.path, "/v1/users");
    }

    #[test]
    fn host_port_is_stripped_like_the_engine_sees_it() {
        let f = form("api.com:8443", "/x").normalized();
        assert_eq!(f.host, "api.com");
        assert_eq!(
            form("api.com:8443", "")
                .to_match()
                .expect("ok")
                .host_glob
                .as_deref(),
            Some("api.com")
        );
    }

    #[test]
    fn remote_target_keeps_query_and_handles_missing_scheme() {
        let url = "http://x.com/mock?debug=1";
        assert_eq!(RemoteTargetForm::from_url(url).to_url().as_deref(), Ok(url));
        let bare = RemoteTargetForm::from_url("localhost:3000");
        assert_eq!(
            (bare.host.as_str(), bare.port.as_str()),
            ("localhost", "3000")
        );
        assert_eq!(bare.to_url().as_deref(), Ok("http://localhost:3000"));
    }

    #[test]
    fn remote_target_fields_build_and_parse_url() {
        let target = RemoteTargetForm {
            https: false,
            host: "localhost".into(),
            port: "3000".into(),
            path: "/mock".into(),
            query: String::new(),
        };
        assert_eq!(target.to_url().as_deref(), Ok("http://localhost:3000/mock"));
        assert_eq!(
            RemoteTargetForm::from_url("http://localhost:3000/mock"),
            target
        );
        assert!(
            RemoteTargetForm {
                port: "99999".into(),
                ..target.clone()
            }
            .to_url()
            .is_err()
        );
        assert!(
            RemoteTargetForm {
                host: "".into(),
                ..target
            }
            .to_url()
            .is_err()
        );
    }
}
