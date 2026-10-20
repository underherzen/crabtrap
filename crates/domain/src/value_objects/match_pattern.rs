//! Шаблон соответствия трафика (общий для всех правил RuleSet).

use serde::{Deserialize, Serialize};

use crate::value_objects::regex_cache::{compiled, whole_match};

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

/// Что известно об обмене для матча правила. `url` — абсолютный URL
/// (`https://api.test:8443/v1/x?y=1`), `path` — path + query.
/// `graphql_operation` — имя операции из тела/query (`None` — не GraphQL или
/// тело не загружено); статус и Content-Type — только на фазе ответа.
#[derive(Debug, Clone, Copy, Default)]
pub struct MatchTarget<'a> {
    pub url: &'a str,
    pub host: &'a str,
    pub path: &'a str,
    pub method: &'a str,
    pub status: Option<u16>,
    pub content_type: Option<&'a str>,
    pub graphql_operation: Option<&'a str>,
}

/// Условие отбора трафика для правил (throttle, map, block, breakpoints).
///
/// Пустые поля/`None` — wildcard (соответствует всему); заданные условия
/// складываются по И. `body_regex` проверяется отдельным методом
/// [`TrafficMatch::matches_body`], т.к. тело грузится лениво.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrafficMatch {
    /// Glob для хоста, например `*.example.com`.
    pub host_glob: Option<String>,
    /// Glob для path+query (или только path при `ignore_query`), например `/api/v2/*`.
    pub path_glob: Option<String>,
    /// Сравнивать `path_glob` с путём без query (`/users` ловит `/users?page=2`).
    pub ignore_query: bool,
    /// Regex по полному URL; должен совпасть со всей строкой.
    pub url_regex: Option<String>,
    /// Точное имя метода (GET, POST, ...).
    pub method: Option<String>,
    /// Glob по имени GraphQL-операции (`operationName` / `query Name {…}`).
    pub graphql_operation: Option<String>,
    /// Точный код статуса ответа.
    pub status_code: Option<u16>,
    /// Glob для Content-Type ответа, например `text/*`.
    pub content_type: Option<String>,
    /// Regex по телу (проверяется только при загруженном теле).
    pub body_regex: Option<String>,
    /// Учитывать регистр в glob/regex/method. По умолчанию false.
    pub case_sensitive: bool,
}

impl TrafficMatch {
    /// Проверить обмен «заголовочной» частью условия (без `body_regex`).
    pub fn matches(&self, target: &MatchTarget<'_>) -> bool {
        self.matches_except_graphql(target) && self.graphql_matches(target.graphql_operation)
    }

    /// Всё, кроме имени GraphQL-операции (оно в теле запроса).
    pub fn matches_except_graphql(&self, target: &MatchTarget<'_>) -> bool {
        self.matches_request_line(target)
            && self
                .status_code
                .is_none_or(|want| target.status == Some(want))
            && self.content_type.as_ref().is_none_or(|want| {
                target
                    .content_type
                    .is_some_and(|ct| glob_match(want, ct, self.case_sensitive))
            })
    }

    /// Только то, что известно до загрузки тела и до ответа: URL, хост,
    /// path, метод. Так решается, грузить ли тело ради GraphQL-операции —
    /// правилу фазы ответа (статус, Content-Type) оно тоже понадобится.
    pub fn matches_request_line(&self, target: &MatchTarget<'_>) -> bool {
        let glob = |pattern: &Option<String>, text: &str| {
            pattern
                .as_ref()
                .is_none_or(|p| glob_match(p, text, self.case_sensitive))
        };
        let path = match self.ignore_query {
            true => target.path.split('?').next().unwrap_or_default(),
            false => target.path,
        };
        glob(&self.host_glob, target.host)
            && glob(&self.path_glob, path)
            && self.url_regex_matches(target.url)
            && self.method_matches(target.method)
    }

    fn url_regex_matches(&self, url: &str) -> bool {
        let Some(pattern) = &self.url_regex else {
            return true;
        };
        // Битый regex — «не соответствует» (валидация — на входе в
        // UpdateRuleSetUseCase).
        whole_match(pattern, self.case_sensitive).is_some_and(|re| re.is_match(url))
    }

    fn method_matches(&self, method: &str) -> bool {
        self.method
            .as_ref()
            .is_none_or(|want| match self.case_sensitive {
                true => want == method,
                false => want.eq_ignore_ascii_case(method),
            })
    }

    fn graphql_matches(&self, operation: Option<&str>) -> bool {
        self.graphql_operation.as_ref().is_none_or(|want| {
            operation.is_some_and(|op| glob_match(want, op, self.case_sensitive))
        })
    }

    /// Проверить `body_regex` по загруженному телу.
    /// Битый regex трактуется как «не соответствует» (валидация — на входе
    /// в `UpdateRuleSetUseCase`).
    pub fn matches_body(&self, body: &[u8]) -> bool {
        let Some(pattern) = &self.body_regex else {
            return true;
        };
        compiled(pattern).is_some_and(|re| re.is_match(&String::from_utf8_lossy(body)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> TrafficMatch {
        TrafficMatch::default()
    }

    /// Цель матча без URL/GraphQL (URL собирается из хоста и пути).
    fn t<'a>(
        host: &'a str,
        path: &'a str,
        method: &'a str,
        status: Option<u16>,
        content_type: Option<&'a str>,
    ) -> MatchTarget<'a> {
        MatchTarget {
            url: "",
            host,
            path,
            method,
            status,
            content_type,
            graphql_operation: None,
        }
    }

    fn request<'a>(url: &'a str, path: &'a str, graphql: Option<&'a str>) -> MatchTarget<'a> {
        MatchTarget {
            url,
            host: "api.test",
            path,
            method: "POST",
            graphql_operation: graphql,
            ..MatchTarget::default()
        }
    }

    #[test]
    fn ignore_query_compares_path_only() {
        let strict = TrafficMatch {
            path_glob: Some("/api/users".into()),
            ..Default::default()
        };
        let loose = TrafficMatch {
            ignore_query: true,
            ..strict.clone()
        };
        let with_query = request("", "/api/users?page=2", None);
        assert!(
            !strict.matches(&with_query),
            "query is part of the path by default"
        );
        assert!(loose.matches(&with_query));
        assert!(loose.matches(&request("", "/api/users", None)));
        assert!(!loose.matches(&request("", "/api/users/1?x", None)));
    }

    #[test]
    fn url_regex_must_match_the_whole_url() {
        let tm = TrafficMatch {
            url_regex: Some(r"https://api\.test/v[12]/users(\?.*)?".into()),
            ..Default::default()
        };
        assert!(tm.matches(&request("https://api.test/v1/users", "", None)));
        assert!(tm.matches(&request("https://api.test/v2/users?page=2", "", None)));
        assert!(!tm.matches(&request("https://api.test/v3/users", "", None)));
        assert!(
            !tm.matches(&request("https://api.test/v1/users/7", "", None)),
            "partial match is not a match"
        );
        assert!(
            tm.matches(&request("HTTPS://API.TEST/V1/USERS", "", None)),
            "case-insensitive by default"
        );
        let sensitive = TrafficMatch {
            case_sensitive: true,
            ..tm
        };
        assert!(!sensitive.matches(&request("HTTPS://API.TEST/V1/USERS", "", None)));
    }

    #[test]
    fn invalid_url_regex_is_non_match() {
        let tm = TrafficMatch {
            url_regex: Some("(unclosed".into()),
            ..Default::default()
        };
        assert!(!tm.matches(&request("https://api.test/", "/", None)));
    }

    #[test]
    fn graphql_operation_needs_a_known_operation() {
        let tm = TrafficMatch {
            host_glob: Some("api.test".into()),
            graphql_operation: Some("Get*".into()),
            ..Default::default()
        };
        assert!(tm.matches(&request("", "/graphql", Some("GetUser"))));
        assert!(tm.matches(&request("", "/graphql", Some("getuser"))));
        assert!(!tm.matches(&request("", "/graphql", Some("UpdateUser"))));
        assert!(
            !tm.matches(&request("", "/graphql", None)),
            "unknown operation"
        );
        assert!(
            tm.matches_except_graphql(&request("", "/graphql", None)),
            "the rest of the rule matches — the body is worth loading"
        );
    }

    #[test]
    fn legacy_json_has_new_fields_off() {
        let tm: TrafficMatch =
            serde_json::from_str(r#"{"host_glob":"a.com","path_glob":"/x"}"#).expect("json");
        assert!(!tm.ignore_query);
        assert_eq!(tm.url_regex, None);
        assert_eq!(tm.graphql_operation, None);
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
        assert!(tm.matches(&t("host", "/path", "GET", None, None)));
    }

    #[test]
    fn host_glob_filters() {
        let tm = TrafficMatch {
            host_glob: Some("*.example.com".into()),
            ..Default::default()
        };
        assert!(tm.matches(&t("api.example.com", "/", "GET", None, None)));
        assert!(!tm.matches(&t("example.org", "/", "GET", None, None)));
    }

    #[test]
    fn method_and_status_must_match() {
        let tm = TrafficMatch {
            method: Some("POST".into()),
            status_code: Some(500),
            ..Default::default()
        };
        assert!(tm.matches(&t("h", "/p", "post", Some(500), None)));
        assert!(!tm.matches(&t("h", "/p", "GET", Some(500), None)));
        assert!(!tm.matches(&t("h", "/p", "POST", Some(200), None)));
        assert!(!tm.matches(&t("h", "/p", "POST", None, None)));
    }

    #[test]
    fn content_type_missing_response_never_matches() {
        let tm = TrafficMatch {
            content_type: Some("text/*".into()),
            ..Default::default()
        };
        assert!(tm.matches(&t("h", "/p", "GET", Some(200), Some("text/html"))));
        assert!(!tm.matches(&t("h", "/p", "GET", Some(200), Some("application/json"))));
        assert!(!tm.matches(&t("h", "/p", "GET", None, None)));
    }

    #[test]
    fn body_regex_checked_separately() {
        let tm = TrafficMatch {
            body_regex: Some(r"token=\w+".into()),
            ..Default::default()
        };
        // Матч заголовков проходит независимо от body_regex…
        assert!(tm.matches(&t("h", "/p", "GET", None, None)));
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
