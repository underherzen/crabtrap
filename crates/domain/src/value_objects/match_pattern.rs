//! Шаблон соответствия трафика (общий для всех правил RuleSet).

use serde::{Deserialize, Serialize};

use crate::entities::HttpExchange;
use crate::value_objects::http_vo::HeaderEntry;

/// Простой glob-матчер с `*` (любая последовательность) и `?` (один символ).
/// Реализован вручную (алгоритм с бэктрекингом «звёздочки»), без внешних
/// зависимостей.
pub fn glob_match(pattern: &str, text: &str, case_sensitive: bool) -> bool {
    let (pattern, text): (Vec<char>, Vec<char>) = if case_sensitive {
        (pattern.chars().collect(), text.chars().collect())
    } else {
        (
            pattern.to_lowercase().chars().collect(),
            text.to_lowercase().chars().collect(),
        )
    };

    let (mut p, mut t) = (0_usize, 0_usize);
    let (mut star, mut mark) = (usize::MAX, 0_usize);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = p;
            mark = t;
            p += 1;
        } else if star != usize::MAX {
            // Откат к последней звёздочке: она поглощает ещё один символ.
            p = star + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Условие отбора трафика для правил (throttle, map, block, breakpoints).
///
/// Пустые поля/`None` — wildcard (соответствует всему). `body_regex`
/// проверяется отдельным методом [`TrafficMatch::matches_body`], т.к. тело
/// грузится лениво и не участвует в матче заголовков.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrafficMatch {
    /// Glob для хоста, например `*.example.com`.
    pub host_glob: Option<String>,
    /// Glob для path+query, например `/api/v2/*`.
    pub path_glob: Option<String>,
    /// Точное имя метода (GET, POST, ...).
    pub method: Option<String>,
    /// Точный код статуса ответа.
    pub status_code: Option<u16>,
    /// Glob для Content-Type ответа, например `text/*`.
    pub content_type: Option<String>,
    /// Regex по телу (проверяется только при загруженном теле).
    pub body_regex: Option<String>,
    /// Учитывать регистр в glob/method. По умолчанию false.
    pub case_sensitive: bool,
}

impl TrafficMatch {
    /// Проверить обмен «заголовочной» частью условия (без `body_regex`).
    pub fn matches(
        &self,
        host: &str,
        path: &str,
        method: &str,
        status: Option<u16>,
        content_type: Option<&str>,
    ) -> bool {
        if let Some(host_glob) = &self.host_glob
            && !glob_match(host_glob, host, self.case_sensitive)
        {
            return false;
        }
        if let Some(path_glob) = &self.path_glob
            && !glob_match(path_glob, path, self.case_sensitive)
        {
            return false;
        }
        if let Some(want_method) = &self.method {
            let equal = if self.case_sensitive {
                want_method == method
            } else {
                want_method.eq_ignore_ascii_case(method)
            };
            if !equal {
                return false;
            }
        }
        if let Some(want_status) = self.status_code
            && status != Some(want_status)
        {
            return false;
        }
        if let Some(want_ct) = &self.content_type {
            match content_type {
                Some(ct) => {
                    if !glob_match(want_ct, ct, self.case_sensitive) {
                        return false;
                    }
                }
                None => return false,
            }
        }
        true
    }

    /// Проверить `body_regex` по загруженному телу.
    /// Битый regex трактуется как «не соответствует» (валидация — на входе
    /// в `UpdateRuleSetUseCase`).
    pub fn matches_body(&self, body: &[u8]) -> bool {
        let Some(pattern) = &self.body_regex else {
            return true;
        };
        let Ok(re) = regex::Regex::new(pattern) else {
            return false;
        };
        let text = String::from_utf8_lossy(body);
        re.is_match(&text)
    }

    /// Матч по готовому обмену: хост/path/method/status/content-type.
    pub fn matches_exchange(&self, exchange: &HttpExchange) -> bool {
        let content_type = exchange
            .response
            .as_ref()
            .and_then(|r| HeaderEntry::find(&r.headers, "content-type"))
            .map(|h| h.value.as_str());
        self.matches(
            exchange.host.as_str(),
            &exchange.request_path(),
            exchange.request.method.as_str(),
            exchange.response.as_ref().map(|r| r.status),
            content_type,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> TrafficMatch {
        TrafficMatch::default()
    }

    #[test]
    fn glob_matches_star_and_question() {
        assert!(glob_match("*", "anything", false));
        assert!(glob_match("*.example.com", "api.example.com", false));
        assert!(glob_match("*.example.com", "a.b.example.com", false));
        assert!(!glob_match("*.example.com", "example.org", false));
        assert!(glob_match("api?.example.com", "api1.example.com", false));
        assert!(!glob_match("api?.example.com", "api12.example.com", false));
        assert!(glob_match("/api/v2/*", "/api/v2/users?page=1", false));
        assert!(glob_match("exact.host", "exact.host", false));
        assert!(glob_match("", "", false));
        assert!(glob_match("*", "", false));
        assert!(!glob_match("", "x", false));
    }

    #[test]
    fn glob_case_sensitivity_flag() {
        assert!(glob_match("API.Example.COM", "api.example.com", false));
        assert!(!glob_match("API.Example.COM", "api.example.com", true));
    }

    #[test]
    fn empty_match_matches_everything() {
        let tm = m();
        assert!(tm.matches("host", "/path", "GET", None, None));
    }

    #[test]
    fn host_glob_filters() {
        let tm = TrafficMatch {
            host_glob: Some("*.example.com".into()),
            ..Default::default()
        };
        assert!(tm.matches("api.example.com", "/", "GET", None, None));
        assert!(!tm.matches("example.org", "/", "GET", None, None));
    }

    #[test]
    fn method_and_status_must_match() {
        let tm = TrafficMatch {
            method: Some("POST".into()),
            status_code: Some(500),
            ..Default::default()
        };
        assert!(tm.matches("h", "/p", "post", Some(500), None));
        assert!(!tm.matches("h", "/p", "GET", Some(500), None));
        assert!(!tm.matches("h", "/p", "POST", Some(200), None));
        assert!(!tm.matches("h", "/p", "POST", None, None));
    }

    #[test]
    fn content_type_missing_response_never_matches() {
        let tm = TrafficMatch {
            content_type: Some("text/*".into()),
            ..Default::default()
        };
        assert!(tm.matches("h", "/p", "GET", Some(200), Some("text/html")));
        assert!(!tm.matches("h", "/p", "GET", Some(200), Some("application/json")));
        assert!(!tm.matches("h", "/p", "GET", None, None));
    }

    #[test]
    fn body_regex_checked_separately() {
        let tm = TrafficMatch {
            body_regex: Some(r"token=\w+".into()),
            ..Default::default()
        };
        // Матч заголовков проходит независимо от body_regex…
        assert!(tm.matches("h", "/p", "GET", None, None));
        // …тело проверяется явным вызовом.
        assert!(tm.matches_body(b"grant?token=abc123"));
        assert!(!tm.matches_body(b"nothing here"));
    }

    #[test]
    fn invalid_body_regex_is_non_match() {
        let tm = TrafficMatch {
            body_regex: Some("(unclosed".into()),
            ..Default::default()
        };
        assert!(!tm.matches_body(b"any"));
    }
}
