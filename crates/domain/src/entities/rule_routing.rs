//! Решения маршрутизации по [`RuleSet`]: какое правило ловит запрос.
//!
//! Чистые функции без IO — их зовёт пайплайн перехвата на каждом запросе.
//! Приоритет как в Charles: Block List → Map Local → Map Remote → origin;
//! внутри категории побеждает первое включённое совпавшее правило.

use super::rule::{BlockRule, MapLocalRule, MapRemoteRule, RuleSet, ThrottleProfile};
use crate::value_objects::{MatchTarget, TrafficMatch};

/// Что известно о запросе до похода в origin.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestFacts<'a> {
    /// Абсолютный URL (`https://api.test/v1?x=1`) — для `url_regex`.
    pub url: &'a str,
    pub host: &'a str,
    /// Path + query (`/api/v1?x=1`).
    pub path: &'a str,
    pub method: &'a str,
    /// Имя GraphQL-операции (`None` — не GraphQL или тело не загружено).
    pub graphql_operation: Option<&'a str>,
}

impl<'a> RequestFacts<'a> {
    /// Цель матча на фазе запроса (статуса и Content-Type ответа нет).
    pub fn target(&self) -> MatchTarget<'a> {
        MatchTarget {
            url: self.url,
            host: self.host,
            path: self.path,
            method: self.method,
            status: None,
            content_type: None,
            graphql_operation: self.graphql_operation,
        }
    }
}

/// Правило, которое определяет судьбу запроса.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RequestRoute<'a> {
    Block(&'a BlockRule),
    MapLocal(&'a MapLocalRule),
    MapRemote(&'a MapRemoteRule),
    Origin,
}

/// Матч на фазе запроса: статуса, Content-Type ответа и тела ещё нет,
/// поэтому правила с этими условиями запрос не ловят (а не ловят ВСЁ).
pub(crate) fn matches_request(m: &TrafficMatch, facts: &RequestFacts<'_>) -> bool {
    m.body_regex.is_none() && m.matches(&facts.target())
}

/// Правилу с `graphql_operation` нужно тело запроса: URL/хост/path/метод
/// совпали, а имя операции до загрузки тела неизвестно (статус и
/// Content-Type ответа — тем более).
fn awaits_graphql(m: &TrafficMatch, facts: &RequestFacts<'_>) -> bool {
    m.graphql_operation.is_some()
        && m.body_regex.is_none()
        && m.matches_request_line(&facts.target())
}

impl RuleSet {
    /// Маршрут запроса по правилам.
    pub fn route_request(&self, facts: &RequestFacts<'_>) -> RequestRoute<'_> {
        if let Some(rule) = self.block_for(facts) {
            return RequestRoute::Block(rule);
        }
        if let Some(rule) = self
            .map_local
            .iter()
            .filter(|_| self.map_local_enabled)
            .find(|r| r.enabled && matches_request(&r.r#match, facts))
        {
            return RequestRoute::MapLocal(rule);
        }
        if let Some(rule) = self
            .map_remote
            .iter()
            .filter(|_| self.map_remote_enabled)
            .find(|r| r.enabled && matches_request(&r.r#match, facts))
        {
            return RequestRoute::MapRemote(rule);
        }
        RequestRoute::Origin
    }

    /// Правило Block List, которое ловит запрос (первое включённое).
    pub fn block_for(&self, facts: &RequestFacts<'_>) -> Option<&BlockRule> {
        self.blocks
            .iter()
            .filter(|_| self.blocks_enabled)
            .find(|r| r.enabled && matches_request(&r.r#match, facts))
    }

    /// CONNECT к хосту заблокирован: правило Block, условие которого — только
    /// хост (без path/method/…), т.е. блокирует хост целиком. Правила с path
    /// применяются к расшифрованным запросам, туннель они не рвут.
    pub fn connect_blocked(&self, host: &str) -> Option<&BlockRule> {
        if !self.blocks_enabled {
            return None;
        }
        self.blocks.iter().find(|r| {
            let m = &r.r#match;
            r.enabled
                && m.host_glob.is_some()
                && m.path_glob.is_none()
                && m.method.is_none()
                && m.status_code.is_none()
                && m.content_type.is_none()
                && m.body_regex.is_none()
                && m.url_regex.is_none()
                && m.graphql_operation.is_none()
                && m.matches(&MatchTarget {
                    host,
                    path: "/",
                    method: "CONNECT",
                    ..MatchTarget::default()
                })
        })
    }

    /// Загрузить тело запроса ради имени GraphQL-операции: какое-то
    /// включённое правило (любого инструмента, любой фазы) ждёт его.
    pub fn graphql_body_needed(&self, facts: &RequestFacts<'_>) -> bool {
        self.any_enabled_match(|m| awaits_graphql(m, facts))
    }

    /// Есть ли включённое правило с GraphQL-операцией: только тогда
    /// пайплайнам стоит разбирать тело запроса в поисках её имени.
    pub fn uses_graphql(&self) -> bool {
        self.any_enabled_match(|m| m.graphql_operation.is_some())
    }

    /// Условие хотя бы одного включённого правила (включённого инструмента
    /// с условием на запрос) удовлетворяет `test`.
    fn any_enabled_match(&self, test: impl Fn(&TrafficMatch) -> bool) -> bool {
        let on = |enabled: bool, m: &TrafficMatch| enabled && test(m);
        (self.blocks_enabled && self.blocks.iter().any(|r| on(r.enabled, &r.r#match)))
            || (self.map_local_enabled && self.map_local.iter().any(|r| on(r.enabled, &r.r#match)))
            || (self.map_remote_enabled
                && self.map_remote.iter().any(|r| on(r.enabled, &r.r#match)))
            || (self.breakpoints_enabled
                && self.breakpoints.iter().any(|r| on(r.enabled, &r.r#match)))
            || (self.scripts_enabled && self.scripts.iter().any(|r| on(r.enabled, &r.r#match)))
            || (self.rewrite_enabled
                && self
                    .rewrite
                    .iter()
                    .any(|r| r.r#match.as_ref().is_some_and(|m| on(r.enabled, m))))
    }

    /// Профиль throttle, если он применяется к запросу (`match: None` —
    /// ко всему трафику).
    pub fn throttle_for(&self, facts: &RequestFacts<'_>) -> Option<&ThrottleProfile> {
        if !self.throttle_enabled {
            return None;
        }
        self.throttle.as_ref().filter(|t| match &t.r#match {
            Some(m) => matches_request(m, facts),
            None => true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::rule::BlockAction;
    use crate::value_objects::RuleId;
    use std::path::PathBuf;

    fn host_match(glob: &str) -> TrafficMatch {
        TrafficMatch {
            host_glob: Some(glob.into()),
            ..Default::default()
        }
    }

    fn facts<'a>(host: &'a str, path: &'a str) -> RequestFacts<'a> {
        RequestFacts {
            host,
            path,
            method: "GET",
            ..RequestFacts::default()
        }
    }

    #[test]
    fn graphql_rule_asks_for_the_body_only_when_the_rest_matches() {
        let graphql = TrafficMatch {
            host_glob: Some("api.test".into()),
            graphql_operation: Some("GetUser".into()),
            ..Default::default()
        };
        let rules = RuleSet {
            blocks: vec![block(graphql.clone(), true)],
            ..Default::default()
        };
        assert!(rules.graphql_body_needed(&facts("api.test", "/graphql")));
        assert!(!rules.graphql_body_needed(&facts("other.test", "/graphql")));

        assert!(rules.uses_graphql());
        let disabled = RuleSet {
            blocks: vec![block(graphql, false)],
            ..Default::default()
        };
        assert!(!disabled.graphql_body_needed(&facts("api.test", "/graphql")));
        assert!(!disabled.uses_graphql(), "disabled rules don't count");
        let plain = RuleSet {
            blocks: vec![block(host_match("api.test"), true)],
            ..Default::default()
        };
        assert!(!plain.graphql_body_needed(&facts("api.test", "/graphql")));
        assert!(!plain.uses_graphql());
    }

    #[test]
    fn graphql_rule_with_response_conditions_still_asks_for_the_body() {
        let rules = RuleSet {
            breakpoints: vec![crate::entities::rule::BreakpointRule {
                id: RuleId::new(),
                r#match: TrafficMatch {
                    graphql_operation: Some("GetUser".into()),
                    status_code: Some(500),
                    content_type: Some("application/json".into()),
                    ..Default::default()
                },
                phase: crate::entities::rule::BreakPhase::Response,
                enabled: true,
            }],
            ..Default::default()
        };
        assert!(
            rules.graphql_body_needed(&facts("api.test", "/graphql")),
            "status and Content-Type are unknown before the response"
        );
    }

    #[test]
    fn graphql_rule_routes_by_operation_name() {
        let rules = RuleSet {
            blocks: vec![block(
                TrafficMatch {
                    graphql_operation: Some("Delete*".into()),
                    ..Default::default()
                },
                true,
            )],
            ..Default::default()
        };
        let with_op = |op| RequestFacts {
            graphql_operation: op,
            ..facts("api.test", "/graphql")
        };
        assert!(matches!(
            rules.route_request(&with_op(Some("DeleteUser"))),
            RequestRoute::Block(_)
        ));
        assert_eq!(
            rules.route_request(&with_op(Some("GetUser"))),
            RequestRoute::Origin
        );
        assert_eq!(rules.route_request(&with_op(None)), RequestRoute::Origin);
    }

    fn block(m: TrafficMatch, enabled: bool) -> BlockRule {
        BlockRule {
            id: RuleId::new(),
            r#match: m,
            action: BlockAction::Status(403),
            enabled,
        }
    }

    fn map_local(m: TrafficMatch) -> MapLocalRule {
        MapLocalRule {
            id: RuleId::new(),
            r#match: m,
            local_path: PathBuf::from("/tmp/x.json"),
            status: 200,
            content_type: None,
            headers: Vec::new(),
            enabled: true,
        }
    }

    fn map_remote(m: TrafficMatch) -> MapRemoteRule {
        MapRemoteRule {
            id: RuleId::new(),
            r#match: m,
            remote: "https://staging.example.com".into(),
            preserve_host: false,
            path_mode: crate::entities::RemotePathMode::Replace,
            enabled: true,
        }
    }

    #[test]
    fn no_rules_route_to_origin() {
        assert_eq!(
            RuleSet::default().route_request(&facts("a.com", "/")),
            RequestRoute::Origin
        );
    }

    #[test]
    fn block_beats_map_local_beats_map_remote() {
        let rules = RuleSet {
            blocks: vec![block(host_match("ads.com"), true)],
            map_local: vec![map_local(host_match("*.com"))],
            map_remote: vec![map_remote(host_match("*"))],
            ..Default::default()
        };
        assert!(matches!(
            rules.route_request(&facts("ads.com", "/")),
            RequestRoute::Block(_)
        ));
        assert!(matches!(
            rules.route_request(&facts("api.com", "/")),
            RequestRoute::MapLocal(_)
        ));
        assert!(matches!(
            rules.route_request(&facts("api.org", "/")),
            RequestRoute::MapRemote(_)
        ));
    }

    #[test]
    fn disabled_rules_are_skipped() {
        let rules = RuleSet {
            blocks: vec![block(host_match("ads.com"), false)],
            ..Default::default()
        };
        assert_eq!(
            rules.route_request(&facts("ads.com", "/")),
            RequestRoute::Origin
        );
        assert!(rules.connect_blocked("ads.com").is_none());
    }

    #[test]
    fn path_glob_routes_only_matching_paths() {
        let rules = RuleSet {
            map_local: vec![map_local(TrafficMatch {
                host_glob: Some("api.com".into()),
                path_glob: Some("/v1/users*".into()),
                ..Default::default()
            })],
            ..Default::default()
        };
        assert!(matches!(
            rules.route_request(&facts("api.com", "/v1/users?id=1")),
            RequestRoute::MapLocal(_)
        ));
        assert_eq!(
            rules.route_request(&facts("api.com", "/v1/orders")),
            RequestRoute::Origin
        );
    }

    #[test]
    fn response_only_conditions_never_route_requests() {
        let rules = RuleSet {
            blocks: vec![block(
                TrafficMatch {
                    status_code: Some(500),
                    ..Default::default()
                },
                true,
            )],
            ..Default::default()
        };
        assert_eq!(
            rules.route_request(&facts("a.com", "/")),
            RequestRoute::Origin
        );
    }

    #[test]
    fn body_regex_rules_never_route_requests() {
        let rules = RuleSet {
            map_local: vec![map_local(TrafficMatch {
                host_glob: Some("a.com".into()),
                body_regex: Some("x".into()),
                ..Default::default()
            })],
            ..Default::default()
        };
        assert_eq!(
            rules.route_request(&facts("a.com", "/")),
            RequestRoute::Origin
        );
    }

    #[test]
    fn disabled_tools_are_skipped_but_rules_kept() {
        let rules = RuleSet {
            blocks: vec![block(host_match("a.com"), true)],
            map_local: vec![map_local(host_match("a.com"))],
            map_remote: vec![map_remote(host_match("a.com"))],
            throttle: Some(ThrottleProfile {
                id: RuleId::new(),
                name: "3G".into(),
                down_bps: Some(1),
                up_bps: None,
                latency_ms: 0,
                loss_percent: 0,
                r#match: None,
            }),
            ..Default::default()
        };
        let no_block = RuleSet {
            blocks_enabled: false,
            ..rules.clone()
        };
        assert!(matches!(
            no_block.route_request(&facts("a.com", "/")),
            RequestRoute::MapLocal(_)
        ));
        assert!(no_block.connect_blocked("a.com").is_none());

        let only_remote = RuleSet {
            map_local_enabled: false,
            ..no_block.clone()
        };
        assert!(matches!(
            only_remote.route_request(&facts("a.com", "/")),
            RequestRoute::MapRemote(_)
        ));
        let none = RuleSet {
            map_remote_enabled: false,
            ..only_remote
        };
        assert_eq!(
            none.route_request(&facts("a.com", "/")),
            RequestRoute::Origin
        );

        let no_throttle = RuleSet {
            throttle_enabled: false,
            ..rules
        };
        assert!(no_throttle.throttle_for(&facts("a.com", "/")).is_none());
        assert!(
            no_throttle.throttle.is_some(),
            "profile kept while disabled"
        );
        assert!(no_throttle.effective_throttle().is_none());
    }

    #[test]
    fn legacy_rules_json_has_all_tools_enabled() {
        let rs: RuleSet = serde_json::from_str("{}").expect("deserialize");
        assert!(rs.blocks_enabled && rs.map_local_enabled && rs.map_remote_enabled);
        assert!(rs.throttle_enabled);
    }

    #[test]
    fn connect_blocked_only_for_host_only_block_rules() {
        let rules = RuleSet {
            blocks: vec![
                block(host_match("*.ads.com"), true),
                block(
                    TrafficMatch {
                        host_glob: Some("api.com".into()),
                        path_glob: Some("/tracking/*".into()),
                        ..Default::default()
                    },
                    true,
                ),
            ],
            ..Default::default()
        };
        assert!(rules.connect_blocked("x.ads.com").is_some());
        assert!(
            rules.connect_blocked("api.com").is_none(),
            "path-level block must not kill the whole tunnel"
        );
    }

    #[test]
    fn throttle_applies_globally_or_by_match() {
        let profile = |m: Option<TrafficMatch>| ThrottleProfile {
            id: RuleId::new(),
            name: "3G".into(),
            down_bps: Some(1000),
            up_bps: None,
            latency_ms: 100,
            loss_percent: 0,
            r#match: m,
        };
        let global = RuleSet {
            throttle: Some(profile(None)),
            ..Default::default()
        };
        assert!(global.throttle_for(&facts("any.com", "/")).is_some());

        let scoped = RuleSet {
            throttle: Some(profile(Some(host_match("slow.com")))),
            ..Default::default()
        };
        assert!(scoped.throttle_for(&facts("slow.com", "/")).is_some());
        assert!(scoped.throttle_for(&facts("fast.com", "/")).is_none());
    }
}
