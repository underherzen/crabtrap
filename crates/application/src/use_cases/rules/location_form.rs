//! Модель формы диалога «Edit Location» (Tools): поля Host / Path / Method
//! вместо одной строки с неочевидным синтаксисом. Ошибки — по полям, чтобы UI
//! показывал их под нужным полем.

use domain::value_objects::TrafficMatch;

use super::edit::parse_location;
use crate::dto::capture::ExchangeSummaryDto;

/// Методы для выпадающего списка (`None` — любой).
pub const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// Значения полей location.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocationForm {
    pub host: String,
    pub path: String,
    pub method: Option<String>,
}

/// Ошибки по полям (None — поле в порядке).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocationErrors {
    pub host: Option<String>,
    pub path: Option<String>,
}

impl LocationErrors {
    pub fn is_empty(&self) -> bool {
        self.host.is_none() && self.path.is_none()
    }
}

impl LocationForm {
    /// Поля из сохранённого условия (правка правила).
    pub fn from_match(m: &TrafficMatch) -> Self {
        Self {
            host: m.host_glob.clone().unwrap_or_default(),
            path: m.path_glob.clone().unwrap_or_default(),
            method: m.method.clone(),
        }
    }

    /// Подстановка из обмена в трафике (правый клик): точный хост, path
    /// без query, метод запроса.
    pub fn from_exchange(exchange: &ExchangeSummaryDto) -> Self {
        // Движок сверяет path+query целиком (full match): у запроса с query
        // хвост «*», иначе правило не поймало бы этот же запрос.
        let path = match exchange.path.split_once('?') {
            Some((path, _)) => format!("{path}*"),
            None => exchange.path.clone(),
        };
        Self {
            host: exchange.host.clone(),
            path,
            method: Some(exchange.method.to_ascii_uppercase()),
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
                method: self.method,
            },
            Err(_) => self,
        }
    }

    pub fn validate(&self) -> LocationErrors {
        let host = self.host.trim();
        let path = self.path.trim();
        LocationErrors {
            host: if host.is_empty() {
                Some("Host is required (e.g. api.example.com or *.example.com)".into())
            } else if host.contains(char::is_whitespace) || host.contains('/') {
                Some("Host must not contain spaces or '/'".into())
            } else {
                None
            },
            path: (!path.is_empty() && !path.starts_with('/') && !path.starts_with('*'))
                .then(|| "Path must start with '/' (e.g. /api/*)".into()),
        }
    }

    /// Условие правила; `Err` — ошибки по полям.
    pub fn to_match(&self) -> Result<TrafficMatch, LocationErrors> {
        let form = self.clone().normalized();
        let errors = form.validate();
        if !errors.is_empty() {
            return Err(errors);
        }
        let path = form.path.trim();
        Ok(TrafficMatch {
            host_glob: Some(form.host.trim().to_ascii_lowercase()),
            path_glob: match path {
                "" | "/*" | "*" => None,
                other => Some(other.to_string()),
            },
            method: form.method.filter(|m| !m.is_empty()),
            ..Default::default()
        })
    }

    /// «GET api.example.com/v1/*» — что именно будет совпадать.
    pub fn preview(&self) -> String {
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
        let method = form.method.as_deref().unwrap_or("ANY");
        format!("{method} {host}{path}")
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
                https: u.scheme() == "https",
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
            method: None,
        }
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
        // Движок сверяет path+query целиком: «*» нужен, чтобы правило ловило
        // этот же запрос с любым query.
        assert_eq!(
            (f.host.as_str(), f.path.as_str(), f.method.as_deref()),
            ("api.com", "/v1/users*", Some("POST"))
        );
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
