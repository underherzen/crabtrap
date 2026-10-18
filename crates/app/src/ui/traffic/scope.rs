//! Выбор в боковой панели («что показывать»): всё, закреплённые домены,
//! домен или раздел домена. Сужает таблицу вместе с фильтром и поиском.

use std::collections::BTreeSet;

use super::store::TrafficRow;

/// Выбранный пункт боковой панели.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    All,
    /// Только закреплённые домены.
    Pinned,
    Host(String),
    /// Раздел домена: путь начинается с `prefix` (первый сегмент, `/v1`).
    Path {
        host: String,
        prefix: String,
    },
    /// Обмены приложения-клиента (по имени).
    App(String),
}

/// Первый сегмент пути без query: `/v1/users?q=1` → `/v1`, `/` → `/`.
pub fn first_segment(path: &str) -> &str {
    let path = path.split(['?', '#']).next().unwrap_or("");
    let rest = path.strip_prefix('/').unwrap_or(path);
    match rest.find('/') {
        Some(end) => &path[..end + 1],
        None if rest.is_empty() => "/",
        None => path,
    }
}

/// Подходит ли строка под выбор панели.
pub fn scope_matches(row: &TrafficRow, scope: &Scope, pinned: &BTreeSet<String>) -> bool {
    let s = &row.summary;
    match scope {
        Scope::All => true,
        Scope::Pinned => pinned.contains(&s.host),
        Scope::Host(host) => s.host == *host,
        Scope::Path { host, prefix } => s.host == *host && first_segment(&s.path) == prefix,
        Scope::App(name) => row.client.as_ref().is_some_and(|c| c.name == *name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::traffic::store::test_row;

    #[test]
    fn first_segment_of_paths() {
        assert_eq!(first_segment("/v1/users/1?q=1"), "/v1");
        assert_eq!(first_segment("/login?x=1"), "/login");
        assert_eq!(first_segment("/"), "/");
        assert_eq!(first_segment(""), "/");
        assert_eq!(first_segment("/a#frag"), "/a");
    }

    #[test]
    fn scopes_narrow_rows() {
        let pinned = BTreeSet::from(["api.test".to_string()]);
        let api = test_row("api.test", "/v1/users");
        let cdn = test_row("cdn.test", "/static/app.js");
        assert!(scope_matches(&cdn, &Scope::All, &pinned));
        assert!(scope_matches(&api, &Scope::Pinned, &pinned));
        assert!(!scope_matches(&cdn, &Scope::Pinned, &pinned));
        assert!(scope_matches(
            &cdn,
            &Scope::Host("cdn.test".into()),
            &pinned
        ));
        assert!(!scope_matches(
            &api,
            &Scope::Host("cdn.test".into()),
            &pinned
        ));
        let v1 = Scope::Path {
            host: "api.test".into(),
            prefix: "/v1".into(),
        };
        assert!(scope_matches(&api, &v1, &pinned));
        assert!(!scope_matches(
            &test_row("api.test", "/v10/x"),
            &v1,
            &pinned
        ));
        let safari = Scope::App("Safari".into());
        assert!(!scope_matches(&api, &safari, &pinned), "unknown client");
        let from_safari = TrafficRow {
            client: Some(std::sync::Arc::new(crate::ui::traffic::apps::ClientApp {
                name: "Safari".into(),
                icon_key: None,
                remote: false,
            })),
            ..test_row("api.test", "/")
        };
        assert!(scope_matches(&from_safari, &safari, &pinned));
    }
}
