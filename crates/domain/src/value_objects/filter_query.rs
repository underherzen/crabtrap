//! Фильтр списка обменов (панель фильтров UI).

use serde::{Deserialize, Serialize};

use crate::entities::HttpExchange;
use crate::value_objects::http_vo::HeaderEntry;
use crate::value_objects::match_pattern::glob_match;

/// Запрос фильтрации таблицы трафика. Пустые поля — не фильтруют.
///
/// `text` и `regex` ищут по host/URI (по телам — лениво, в фазе 5,
/// когда UI имеет доступ к загруженным телам).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterQuery {
    /// Подстрока (case-insensitive) в host/URI.
    pub text: Option<String>,
    /// Glob-список хостов: обмен подходит, если совпал хотя бы с одним.
    pub hosts: Vec<String>,
    /// Список кодов статуса.
    pub status_codes: Vec<u16>,
    /// Список методов.
    pub methods: Vec<String>,
    /// Glob-список Content-Type ответа.
    pub content_types: Vec<String>,
    /// Regex по host/URI (битый — не соответствует ничему).
    pub regex: Option<String>,
}

impl FilterQuery {
    /// Пустой фильтр (всё подходит).
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Подходит ли обмен под фильтр.
    pub fn matches(&self, exchange: &HttpExchange) -> bool {
        if !self.hosts.is_empty()
            && !self
                .hosts
                .iter()
                .any(|glob| glob_match(glob, exchange.host.as_str(), false))
        {
            return false;
        }

        if !self.methods.is_empty()
            && !self
                .methods
                .iter()
                .any(|m| m.eq_ignore_ascii_case(exchange.request.method.as_str()))
        {
            return false;
        }

        if !self.status_codes.is_empty() {
            match exchange.response.as_ref() {
                Some(resp) if self.status_codes.contains(&resp.status) => {}
                _ => return false,
            }
        }

        if !self.content_types.is_empty() {
            let ct = exchange
                .response
                .as_ref()
                .and_then(|r| HeaderEntry::find(&r.headers, "content-type"))
                .map(|h| h.value.as_str());
            let Some(ct) = ct else {
                return false;
            };
            if !self
                .content_types
                .iter()
                .any(|glob| glob_match(glob, ct, false))
            {
                return false;
            }
        }

        if let Some(text) = &self.text {
            let needle = text.to_lowercase();
            if !exchange.host.as_str().to_lowercase().contains(&needle)
                && !exchange.request.uri.to_lowercase().contains(&needle)
            {
                return false;
            }
        }

        if let Some(pattern) = &self.regex {
            let Ok(re) = regex::Regex::new(pattern) else {
                return false;
            };
            let haystack = format!("{}{}", exchange.host, exchange.request.uri);
            if !re.is_match(&haystack) {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::ExchangeStatus;
    use crate::entities::exchange::{HttpRequest, HttpResponse};
    use crate::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
    use crate::value_objects::timing::TimingPhases;
    use crate::value_objects::{ConnectionId, ExchangeId, Host};

    fn exchange(host: &str, uri: &str, method: &str, status: Option<u16>) -> HttpExchange {
        let response = status.map(|status| HttpResponse {
            status,
            reason: None,
            headers: vec![HeaderEntry::new(
                "Content-Type",
                "application/json; charset=utf-8",
            )],
            body: None,
            raw_size: 128,
            version: HttpVersionVo::Http11,
        });
        HttpExchange {
            id: ExchangeId::new(),
            connection_id: ConnectionId::new(),
            started_at: chrono::Utc::now(),
            request: HttpRequest {
                method: Method::new(method),
                uri: uri.to_string(),
                version: HttpVersionVo::Http11,
                headers: Vec::new(),
                body: None,
                raw_size: 64,
                origin: Default::default(),
            },
            response,
            status: ExchangeStatus::Pending,
            timing: TimingPhases::default(),
            tls: None,
            protocol: Protocol::Http1,
            tags: Vec::new(),
            host: Host::new(host).expect("test host"),
            note: None,
            client_request: None,
        }
    }

    #[test]
    fn empty_filter_matches_everything() {
        let q = FilterQuery::default();
        assert!(q.is_empty());
        assert!(q.matches(&exchange("a.com", "http://a.com/x", "GET", None)));
    }

    #[test]
    fn filter_by_host_glob_and_status() {
        let q = FilterQuery {
            hosts: vec!["*.example.com".into()],
            status_codes: vec![200, 204],
            ..Default::default()
        };
        assert!(q.matches(&exchange(
            "api.example.com",
            "https://api.example.com/v1",
            "GET",
            Some(200)
        )));
        assert!(!q.matches(&exchange(
            "api.example.com",
            "https://api.example.com/v1",
            "GET",
            Some(500)
        )));
        assert!(!q.matches(&exchange(
            "other.org",
            "https://other.org",
            "GET",
            Some(200)
        )));
    }

    #[test]
    fn filter_requires_response_when_status_set() {
        let q = FilterQuery {
            status_codes: vec![200],
            ..Default::default()
        };
        assert!(!q.matches(&exchange("a.com", "http://a.com/", "GET", None)));
    }

    #[test]
    fn filter_by_text_is_case_insensitive() {
        let q = FilterQuery {
            text: Some("API.EXAMPLE".into()),
            ..Default::default()
        };
        assert!(q.matches(&exchange(
            "api.example.com",
            "https://api.example.com/v1/users",
            "GET",
            None
        )));
        assert!(!q.matches(&exchange("other.org", "https://other.org", "GET", None)));
    }

    #[test]
    fn filter_by_content_type_glob() {
        let q = FilterQuery {
            content_types: vec!["application/*".into()],
            ..Default::default()
        };
        assert!(q.matches(&exchange("a.com", "http://a.com/", "GET", Some(200))));
    }

    #[test]
    fn filter_by_regex_over_host_and_uri() {
        let q = FilterQuery {
            regex: Some(r"/v2/\d+".into()),
            ..Default::default()
        };
        assert!(q.matches(&exchange("a.com", "https://a.com/v2/42/items", "GET", None)));
        assert!(!q.matches(&exchange("a.com", "https://a.com/v1/items", "GET", None)));
    }
}
