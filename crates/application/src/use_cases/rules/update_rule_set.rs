//! Валидация + сохранение RuleSet: CRUD из редактора правил.
//!
//! После успеха UI шлёт `EngineCommand::UpdateRules` для hot-reload движка
//! (композиция — в app, не здесь).

use std::sync::Arc;

use domain::entities::rule::{BlockAction, RewriteOp, RuleSet};
use domain::entities::script_rule::ScriptRule;
use domain::errors::DomainError;
use domain::repositories::RuleRepository;
use domain::value_objects::TrafficMatch;
use domain::value_objects::http_vo::HeaderEntry;
use domain::value_objects::regex_cache::whole_match_check;

use super::response_form::{MAP_LOCAL_STATUS_RANGE, content_type_error, map_local_header_error};
use crate::dto::rules::RuleSetDto;

pub struct UpdateRuleSetUseCase {
    rule_repository: Arc<dyn RuleRepository>,
}

impl UpdateRuleSetUseCase {
    pub fn new(rule_repository: Arc<dyn RuleRepository>) -> Self {
        Self { rule_repository }
    }

    /// Проверить и сохранить. Возвращает новую версию набора.
    pub async fn execute(&self, dto: RuleSetDto) -> Result<u64, DomainError> {
        let rules = dto.into_rule_set();
        validate(&rules)?;
        self.rule_repository.update(rules).await
    }
}

/// Длина regex условия (URL, тело): правило — не программа, длинный шаблон —
/// ошибка ввода.
pub const MAX_URL_REGEX_CHARS: usize = 2048;

/// Шаблоны условия любого инструмента: regex компилируются, имя
/// GraphQL-операции не пустое.
fn validate_match_patterns(m: &TrafficMatch, what: &str) -> Result<(), DomainError> {
    let invalid = |message: String| Err(DomainError::ValidationError(format!("{what}: {message}")));
    if let Some(pattern) = &m.url_regex {
        if pattern.chars().count() > MAX_URL_REGEX_CHARS {
            return invalid(format!(
                "URL regex is longer than {MAX_URL_REGEX_CHARS} characters"
            ));
        }
        if let Err(e) = whole_match_check(pattern, m.case_sensitive) {
            return invalid(format!("invalid URL regex: {e}"));
        }
    }
    if let Some(pattern) = &m.body_regex {
        if pattern.chars().count() > MAX_URL_REGEX_CHARS {
            return invalid(format!(
                "body regex is longer than {MAX_URL_REGEX_CHARS} characters"
            ));
        }
        if let Err(e) = regex::Regex::new(pattern) {
            return invalid(format!("invalid body regex: {e}"));
        }
    }
    if m.graphql_operation
        .as_deref()
        .is_some_and(|op| op.trim().is_empty())
    {
        return invalid("GraphQL operation name is empty".into());
    }
    Ok(())
}

/// Условие правила маршрутизации (Block / Map Local / Map Remote):
/// обязательно (пустое ловило бы весь трафик) и без `body_regex` — на фазе
/// запроса тела ещё нет, такое условие молча не работало бы.
fn validate_routing_match(m: &TrafficMatch, what: &str) -> Result<(), DomainError> {
    // Флаги (регистр, игнор query) сами ничего не отбирают.
    let conditions = TrafficMatch {
        ignore_query: false,
        case_sensitive: false,
        ..m.clone()
    };
    if conditions == TrafficMatch::default() {
        return Err(DomainError::ValidationError(format!(
            "{what}: match is required"
        )));
    }
    if m.body_regex.is_some() {
        return Err(DomainError::ValidationError(format!(
            "{what}: body_regex is not supported for request routing"
        )));
    }
    validate_match_patterns(m, what)
}

/// Цель Map Remote: абсолютный http(s)-URL с хостом.
fn validate_remote_url(remote: &str, what: &str) -> Result<(), DomainError> {
    let valid = url::Url::parse(remote).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty())
    });
    if valid {
        Ok(())
    } else {
        Err(DomainError::ValidationError(format!(
            "{what}: remote must be an absolute http(s) URL with a host, got '{remote}'"
        )))
    }
}

/// Потолок исходника скрипта: rules.json пишется на каждое изменение.
pub const MAX_SCRIPT_SOURCE_BYTES: usize = 512 * 1024;

fn validate_script(rule: &ScriptRule) -> Result<(), DomainError> {
    let what = format!("script {}", rule.id);
    if rule.name.trim().is_empty() {
        return Err(DomainError::ValidationError(format!(
            "{what}: name is empty"
        )));
    }
    validate_routing_match(&rule.r#match, &what)?;
    if !(rule.run_on_request || rule.run_on_response || rule.mock_api) {
        return Err(DomainError::ValidationError(format!(
            "{what} ({}): enable Request, Response or Mock API",
            rule.name
        )));
    }
    if rule.source.len() > MAX_SCRIPT_SOURCE_BYTES {
        return Err(DomainError::ValidationError(format!(
            "{what} ({}): script is larger than {} KiB",
            rule.name,
            MAX_SCRIPT_SOURCE_BYTES / 1024
        )));
    }
    Ok(())
}

/// Домен оставляет структуру пермиссивной (serde-default), поэтому все
/// содержательные инварианты проверяются здесь, на входе.
fn validate(rules: &RuleSet) -> Result<(), DomainError> {
    let mut rule_ids = std::collections::HashSet::new();
    let mut check_id = |id: domain::value_objects::RuleId, what: &str| -> Result<(), DomainError> {
        if !rule_ids.insert(id) {
            return Err(DomainError::DuplicateEntity(format!("{what} rule id {id}")));
        }
        Ok(())
    };

    for entry in &rules.ssl_proxy {
        check_id(entry.id, "ssl_proxy")?;
        if entry.host_pattern.trim().is_empty() {
            return Err(DomainError::ValidationError(
                "ssl_proxy entry: host_pattern is empty".into(),
            ));
        }
    }

    for entry in &rules.allow_list {
        check_id(entry.id, "allow_list")?;
        let pattern = entry.host_pattern.trim();
        if pattern.is_empty() || pattern.contains(char::is_whitespace) {
            return Err(DomainError::ValidationError(format!(
                "allow_list entry {}: host pattern {:?} is empty or has spaces",
                entry.id, entry.host_pattern
            )));
        }
    }

    for entry in &rules.dns_spoof {
        check_id(entry.id, "dns_spoof")?;
        let pattern = entry.host_pattern.trim();
        if pattern.is_empty() || pattern.contains(char::is_whitespace) {
            return Err(DomainError::ValidationError(format!(
                "dns_spoof entry {}: host pattern {:?} is empty or has spaces",
                entry.id, entry.host_pattern
            )));
        }
    }

    for rule in &rules.rewrite {
        check_id(rule.id, "rewrite")?;
        if rule.name.trim().is_empty() {
            return Err(DomainError::ValidationError(format!(
                "rewrite rule {}: name is empty",
                rule.id
            )));
        }
        if rule.ops.is_empty() {
            return Err(DomainError::ValidationError(format!(
                "rewrite rule {} ({}): no ops",
                rule.id, rule.name
            )));
        }
        if let Some(m) = &rule.r#match {
            validate_match_patterns(m, &format!("rewrite rule {}", rule.id))?;
        }
        for op in &rule.ops {
            validate_rewrite_op(op)?;
        }
    }

    for rule in &rules.map_local {
        check_id(rule.id, "map_local")?;
        let what = format!("map_local rule {}", rule.id);
        validate_routing_match(&rule.r#match, &what)?;
        validate_map_local_response(rule.status, rule.content_type.as_deref(), &what)?;
        validate_map_local_headers(&rule.headers, &what)?;
    }

    for rule in &rules.map_remote {
        check_id(rule.id, "map_remote")?;
        let what = format!("map_remote rule {}", rule.id);
        validate_routing_match(&rule.r#match, &what)?;
        validate_remote_url(&rule.remote, &what)?;
    }

    for rule in &rules.blocks {
        check_id(rule.id, "block")?;
        validate_routing_match(&rule.r#match, &format!("block rule {}", rule.id))?;
        if let BlockAction::Status(status) = rule.action {
            validate_status(status, &format!("block rule {}", rule.id))?;
        }
    }

    for rule in &rules.breakpoints {
        check_id(rule.id, "breakpoint")?;
        validate_routing_match(&rule.r#match, &format!("breakpoint {}", rule.id))?;
    }

    for rule in &rules.scripts {
        check_id(rule.id, "script")?;
        validate_script(rule)?;
    }

    if let Some(throttle) = &rules.throttle {
        check_id(throttle.id, "throttle")?;
        match (throttle.down_bps, throttle.up_bps) {
            (Some(0), _) | (_, Some(0)) => {
                return Err(DomainError::ValidationError(
                    "throttle: bandwidth must be > 0".into(),
                ));
            }
            _ => {}
        }
        if let Some(m) = &throttle.r#match {
            validate_match_patterns(m, "throttle")?;
            if m.graphql_operation.is_some() {
                return Err(DomainError::ValidationError(
                    "throttle: GraphQL operation is not supported (the request body is not read)"
                        .into(),
                ));
            }
        }
    }

    Ok(())
}

fn validate_rewrite_op(op: &RewriteOp) -> Result<(), DomainError> {
    match op {
        RewriteOp::AddHeader { name, .. }
        | RewriteOp::ReplaceHeader { name, .. }
        | RewriteOp::RemoveHeader { name } => {
            if name.trim().is_empty() {
                return Err(DomainError::ValidationError(
                    "rewrite op: header name is empty".into(),
                ));
            }
        }
        RewriteOp::BodyFindReplace { find, regex, .. } => {
            if find.is_empty() {
                return Err(DomainError::ValidationError(
                    "rewrite op BodyFindReplace: find is empty".into(),
                ));
            }
            if *regex && regex::Regex::new(find).is_err() {
                return Err(DomainError::ValidationError(format!(
                    "rewrite op BodyFindReplace: invalid regex {find}"
                )));
            }
        }
        RewriteOp::UrlRewrite { find, regex, .. } => {
            if find.is_empty() {
                return Err(DomainError::ValidationError(
                    "rewrite op UrlRewrite: find is empty".into(),
                ));
            }
            if *regex && regex::Regex::new(find).is_err() {
                return Err(DomainError::ValidationError(format!(
                    "rewrite op UrlRewrite: invalid regex {find}"
                )));
            }
        }
        RewriteOp::SetStatus { status } => {
            validate_status(*status, "rewrite op SetStatus")?;
        }
    }
    Ok(())
}

fn validate_status(status: u16, what: &str) -> Result<(), DomainError> {
    if !(100..=599).contains(&status) {
        return Err(DomainError::ValidationError(format!(
            "{what}: status {status} out of range 100..=599"
        )));
    }
    Ok(())
}

/// Ответ Map Local: финальный код (не 1xx) и корректный Content-Type.
fn validate_map_local_response(
    status: u16,
    content_type: Option<&str>,
    what: &str,
) -> Result<(), DomainError> {
    if !MAP_LOCAL_STATUS_RANGE.contains(&status) {
        return Err(DomainError::ValidationError(format!(
            "{what}: status {status} out of range 200..=599"
        )));
    }
    match content_type.and_then(content_type_error) {
        Some(e) => Err(DomainError::ValidationError(format!("{what}: {e}"))),
        None => Ok(()),
    }
}

/// Доп. заголовки Map Local: имя — токен, значение — одна строка, без
/// фрейминга/hop-by-hop и Content-Type (у него своё поле).
fn validate_map_local_headers(headers: &[HeaderEntry], what: &str) -> Result<(), DomainError> {
    match headers
        .iter()
        .find_map(|h| map_local_header_error(&h.name, &h.value))
    {
        Some(e) => Err(DomainError::ValidationError(format!("{what}: {e}"))),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use domain::entities::rule::{MapRemoteRule, RewriteOp, RewriteRule, RuleSet, SslProxyEntry};
    use domain::value_objects::http_vo::HeaderEntry;
    use domain::value_objects::{RuleId, TrafficMatch};

    struct MemRuleRepo {
        current: std::sync::Mutex<RuleSet>,
    }

    #[async_trait]
    impl RuleRepository for MemRuleRepo {
        async fn get(&self) -> Result<RuleSet, DomainError> {
            Ok(self.current.lock().unwrap().clone())
        }

        async fn update(&self, mut rules: RuleSet) -> Result<u64, DomainError> {
            let mut guard = self.current.lock().unwrap();
            guard.version += 1;
            rules.version = guard.version;
            *guard = rules;
            Ok(guard.version)
        }

        async fn version(&self) -> Result<u64, DomainError> {
            Ok(self.current.lock().unwrap().version)
        }
    }

    fn host(glob: &str) -> TrafficMatch {
        TrafficMatch {
            host_glob: Some(glob.into()),
            ..Default::default()
        }
    }

    #[test]
    fn block_rule_requires_a_match() {
        let rules = RuleSet {
            blocks: vec![domain::entities::rule::BlockRule {
                id: RuleId::new(),
                r#match: TrafficMatch::default(),
                action: BlockAction::Status(403),
                enabled: true,
            }],
            ..Default::default()
        };
        assert!(matches!(
            validate(&rules),
            Err(DomainError::ValidationError(_))
        ));
    }

    #[test]
    fn routing_rules_reject_body_regex() {
        let rules = RuleSet {
            map_local: vec![domain::entities::rule::MapLocalRule {
                id: RuleId::new(),
                r#match: TrafficMatch {
                    body_regex: Some("secret".into()),
                    ..Default::default()
                },
                local_path: "/tmp/x".into(),
                status: 200,
                content_type: None,
                headers: Vec::new(),
                enabled: true,
            }],
            ..Default::default()
        };
        assert!(matches!(
            validate(&rules),
            Err(DomainError::ValidationError(_))
        ));
    }

    #[test]
    fn map_local_response_status_and_content_type_are_validated() {
        let rule = |status: u16, content_type: Option<&str>| RuleSet {
            map_local: vec![domain::entities::rule::MapLocalRule {
                id: RuleId::new(),
                r#match: host("a.com"),
                local_path: "/tmp/x".into(),
                status,
                content_type: content_type.map(Into::into),
                headers: Vec::new(),
                enabled: true,
            }],
            ..Default::default()
        };
        assert!(validate(&rule(404, Some("text/plain"))).is_ok());
        assert!(
            validate(&rule(101, None)).is_err(),
            "1xx is not a final response"
        );
        assert!(validate(&rule(600, None)).is_err());
        assert!(validate(&rule(200, Some("text/plain\r\nX-Evil: 1"))).is_err());
    }

    #[test]
    fn match_patterns_are_validated_for_every_tool() {
        let with = |m: TrafficMatch| RuleSet {
            blocks: vec![domain::entities::rule::BlockRule {
                id: RuleId::new(),
                r#match: m,
                action: BlockAction::Status(403),
                enabled: true,
            }],
            ..Default::default()
        };
        let regex = |r: &str| TrafficMatch {
            url_regex: Some(r.into()),
            ..Default::default()
        };
        assert!(validate(&with(regex(r"https://api\.test/.*"))).is_ok());
        assert!(validate(&with(regex("(unclosed"))).is_err());
        assert!(validate(&with(regex(&"a".repeat(MAX_URL_REGEX_CHARS + 1)))).is_err());

        let graphql = |op: &str| TrafficMatch {
            graphql_operation: Some(op.into()),
            ..Default::default()
        };
        assert!(
            validate(&with(graphql("GetUser"))).is_ok(),
            "alone is a valid match"
        );
        assert!(validate(&with(graphql("  "))).is_err());
        let flags_only = TrafficMatch {
            ignore_query: true,
            case_sensitive: true,
            ..Default::default()
        };
        assert!(
            validate(&with(flags_only)).is_err(),
            "flags alone would catch every request"
        );

        let throttle = RuleSet {
            throttle: Some(domain::entities::rule::ThrottleProfile {
                id: RuleId::new(),
                name: "3G".into(),
                down_bps: None,
                up_bps: None,
                latency_ms: 0,
                r#match: Some(graphql("GetUser")),
            }),
            ..Default::default()
        };
        assert!(
            validate(&throttle).is_err(),
            "throttle has no request body — a GraphQL condition would never match"
        );

        let rewrite = RuleSet {
            rewrite: vec![RewriteRule {
                id: RuleId::new(),
                name: "r".into(),
                enabled: true,
                location: domain::entities::rule::RewriteLocation::Request,
                r#match: Some(regex("(bad")),
                ops: vec![RewriteOp::RemoveHeader { name: "x".into() }],
            }],
            ..Default::default()
        };
        assert!(
            validate(&rewrite).is_err(),
            "rewrite matches are checked too"
        );
    }

    #[test]
    fn dns_spoof_hosts_are_validated() {
        let with = |pattern: &str| RuleSet {
            dns_spoof: vec![domain::entities::rule::DnsSpoofEntry::new(
                pattern,
                std::net::Ipv4Addr::LOCALHOST.into(),
            )],
            ..Default::default()
        };
        assert!(validate(&with("*.example.com")).is_ok());
        assert!(validate(&with("")).is_err());
        assert!(validate(&with("a b.com")).is_err());
    }

    #[test]
    fn allow_list_hosts_are_validated() {
        let with = |pattern: &str| RuleSet {
            allow_list: vec![domain::entities::rule::AllowEntry::new(pattern)],
            ..Default::default()
        };
        assert!(validate(&with("*.example.com")).is_ok());
        assert!(validate(&with(" ")).is_err());
        assert!(validate(&with("a b.com")).is_err());
    }

    #[test]
    fn map_local_headers_are_validated() {
        let rule = |name: &str, value: &str| RuleSet {
            map_local: vec![domain::entities::rule::MapLocalRule {
                id: RuleId::new(),
                r#match: host("a.com"),
                local_path: "/tmp/x".into(),
                status: 200,
                content_type: None,
                headers: vec![HeaderEntry::new(name, value)],
                enabled: true,
            }],
            ..Default::default()
        };
        assert!(validate(&rule("Access-Control-Allow-Origin", "*")).is_ok());
        assert!(validate(&rule("Content-Length", "3")).is_err(), "framing");
        assert!(validate(&rule("Bad Name", "x")).is_err());
        assert!(validate(&rule("X-Evil", "a\r\nX-Injected: 1")).is_err());
    }

    #[test]
    fn map_remote_target_must_be_http_url_with_host() {
        let rule = |remote: &str| RuleSet {
            map_remote: vec![MapRemoteRule {
                id: RuleId::new(),
                r#match: host("a.com"),
                remote: remote.into(),
                preserve_host: false,
                enabled: true,
            }],
            ..Default::default()
        };
        assert!(validate(&rule("http://localhost:3000")).is_ok());
        assert!(validate(&rule("https://staging.example.com/v2")).is_ok());
        for bad in ["foo://x", "https://", "staging.internal", "ftp://h/"] {
            assert!(validate(&rule(bad)).is_err(), "{bad} must be rejected");
        }
    }

    fn ssl_entry(pattern: &str) -> SslProxyEntry {
        SslProxyEntry {
            id: RuleId::new(),
            host_pattern: pattern.into(),
            enabled: true,
            exclude: false,
        }
    }

    #[test]
    fn valid_set_bumps_version() {
        let repo = Arc::new(MemRuleRepo {
            current: std::sync::Mutex::new(RuleSet::default()),
        });
        let uc = UpdateRuleSetUseCase::new(repo.clone());
        let dto = RuleSetDto::from_rule_set(&RuleSet {
            ssl_proxy: vec![ssl_entry("*.example.com")],
            ..Default::default()
        });
        let version = futures::executor::block_on(uc.execute(dto)).expect("valid");
        assert_eq!(version, 1);
        assert_eq!(futures::executor::block_on(repo.version()).unwrap(), 1);
    }

    #[test]
    fn empty_host_pattern_is_rejected() {
        let repo = Arc::new(MemRuleRepo {
            current: std::sync::Mutex::new(RuleSet::default()),
        });
        let uc = UpdateRuleSetUseCase::new(repo);
        let dto = RuleSetDto::from_rule_set(&RuleSet {
            ssl_proxy: vec![ssl_entry("   ")],
            ..Default::default()
        });
        let err = futures::executor::block_on(uc.execute(dto)).unwrap_err();
        assert!(matches!(err, DomainError::ValidationError(_)));
    }

    #[test]
    fn duplicate_rule_ids_are_rejected() {
        let id = RuleId::new();
        let repo = Arc::new(MemRuleRepo {
            current: std::sync::Mutex::new(RuleSet::default()),
        });
        let uc = UpdateRuleSetUseCase::new(repo);
        let dto = RuleSetDto::from_rule_set(&RuleSet {
            ssl_proxy: vec![
                SslProxyEntry {
                    id,
                    host_pattern: "a.com".into(),
                    enabled: true,
                    exclude: false,
                },
                SslProxyEntry {
                    id,
                    host_pattern: "b.com".into(),
                    enabled: true,
                    exclude: false,
                },
            ],
            ..Default::default()
        });
        let err = futures::executor::block_on(uc.execute(dto)).unwrap_err();
        assert!(matches!(err, DomainError::DuplicateEntity(_)));
    }

    #[test]
    fn invalid_regex_in_rewrite_is_rejected() {
        let repo = Arc::new(MemRuleRepo {
            current: std::sync::Mutex::new(RuleSet::default()),
        });
        let uc = UpdateRuleSetUseCase::new(repo);
        let dto = RuleSetDto::from_rule_set(&RuleSet {
            rewrite: vec![RewriteRule {
                id: RuleId::new(),
                name: "bad".into(),
                enabled: true,
                location: domain::entities::rule::RewriteLocation::Request,
                r#match: None,
                ops: vec![RewriteOp::BodyFindReplace {
                    find: "(unclosed".into(),
                    replace: "x".into(),
                    regex: true,
                }],
            }],
            ..Default::default()
        });
        let err = futures::executor::block_on(uc.execute(dto)).unwrap_err();
        assert!(matches!(err, DomainError::ValidationError(_)));
    }

    #[test]
    fn map_remote_requires_absolute_url() {
        let repo = Arc::new(MemRuleRepo {
            current: std::sync::Mutex::new(RuleSet::default()),
        });
        let uc = UpdateRuleSetUseCase::new(repo);
        let dto = RuleSetDto::from_rule_set(&RuleSet {
            map_remote: vec![MapRemoteRule {
                id: RuleId::new(),
                r#match: TrafficMatch {
                    host_glob: Some("a.com".into()),
                    ..Default::default()
                },
                remote: "staging.internal".into(),
                preserve_host: false,
                enabled: true,
            }],
            ..Default::default()
        });
        let err = futures::executor::block_on(uc.execute(dto)).unwrap_err();
        assert!(matches!(err, DomainError::ValidationError(_)));
    }

    fn script(name: &str, source: &str) -> domain::entities::script_rule::ScriptRule {
        domain::entities::script_rule::ScriptRule {
            id: RuleId::new(),
            name: name.into(),
            r#match: host("api.com"),
            enabled: true,
            run_on_request: true,
            run_on_response: true,
            mock_api: false,
            source: source.into(),
        }
    }

    #[test]
    fn script_rules_are_validated() {
        let ok = RuleSet {
            scripts: vec![script("ok", "function onRequest(){}")],
            ..Default::default()
        };
        assert!(validate(&ok).is_ok());

        let invalid = [
            script(" ", "x"),
            domain::entities::script_rule::ScriptRule {
                r#match: TrafficMatch::default(),
                ..script("no match", "x")
            },
            domain::entities::script_rule::ScriptRule {
                run_on_request: false,
                run_on_response: false,
                ..script("never runs", "x")
            },
            script("huge", &"x".repeat(MAX_SCRIPT_SOURCE_BYTES + 1)),
        ];
        for rule in invalid {
            let rules = RuleSet {
                scripts: vec![rule.clone()],
                ..Default::default()
            };
            assert!(
                matches!(validate(&rules), Err(DomainError::ValidationError(_))),
                "{}",
                rule.name
            );
        }
    }

    #[test]
    fn mock_api_alone_is_enough_to_run() {
        let rules = RuleSet {
            scripts: vec![domain::entities::script_rule::ScriptRule {
                run_on_request: false,
                run_on_response: false,
                mock_api: true,
                ..script("mock", "x")
            }],
            ..Default::default()
        };
        assert!(validate(&rules).is_ok());
    }
}
