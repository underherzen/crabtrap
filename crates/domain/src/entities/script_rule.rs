//! Правило-скрипт (Track S, как Scripting Tool в Proxyman): JS-код,
//! который получает запрос/ответ совпавшего обмена и может их изменить
//! или ответить сам (Mock API).
//!
//! Домен знает только «какой скрипт и на какой фазе»; исполнение кода —
//! порт `ScriptRuntime` (application) и его реализация (infrastructure).

use serde::{Deserialize, Serialize};

use super::rule::RuleSet;
use super::rule_routing::RequestFacts;
use crate::value_objects::{RuleId, TrafficMatch};

/// Фаза, на которой выполняется скрипт.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptPhase {
    Request,
    Response,
}

/// Правило-скрипт.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptRule {
    pub id: RuleId,
    pub name: String,
    #[serde(rename = "match")]
    pub r#match: TrafficMatch,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Вызывать `onRequest`.
    #[serde(default = "default_true")]
    pub run_on_request: bool,
    /// Вызывать `onResponse`.
    #[serde(default = "default_true")]
    pub run_on_response: bool,
    /// Mock API: ответ строит `onRequest`, запрос в origin не уходит.
    #[serde(default)]
    pub mock_api: bool,
    /// Исходный код (JavaScript).
    #[serde(default)]
    pub source: String,
}

fn default_true() -> bool {
    true
}

impl ScriptRule {
    /// Скрипт выполняется на этой фазе. Mock API отвечает на фазе
    /// запроса, поэтому на неё включён всегда.
    pub fn runs_on(&self, phase: ScriptPhase) -> bool {
        match phase {
            ScriptPhase::Request => self.run_on_request || self.mock_api,
            ScriptPhase::Response => self.run_on_response,
        }
    }
}

impl RuleSet {
    /// Скрипт для обмена на фазе: первое включённое совпавшее правило
    /// (как у остальных инструментов). Условие проверяется только по
    /// фактам запроса — на обеих фазах матчится один и тот же скрипт.
    pub fn script_for(&self, facts: &RequestFacts<'_>, phase: ScriptPhase) -> Option<&ScriptRule> {
        self.matching_script(facts)
            .filter(|rule| rule.runs_on(phase))
    }

    /// Первое включённое правило-скрипт, совпавшее с запросом (без учёта
    /// фазы): пайплайн запоминает его на обмен, чтобы фаза ответа шла тем
    /// же скриптом, даже если `onRequest` поменял URL.
    pub fn matching_script(&self, facts: &RequestFacts<'_>) -> Option<&ScriptRule> {
        if !self.scripts_enabled {
            return None;
        }
        self.scripts
            .iter()
            .filter(|rule| rule.enabled)
            .find(|rule| {
                rule.r#match.body_regex.is_none()
                    && rule
                        .r#match
                        .matches(facts.host, facts.path, facts.method, None, None)
            })
    }

    /// Включённый скрипт по id (правило могли выключить или удалить).
    pub fn active_script(&self, id: RuleId) -> Option<&ScriptRule> {
        self.scripts
            .iter()
            .find(|rule| rule.id == id)
            .filter(|rule| self.scripts_enabled && rule.enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(host: &str, on_request: bool, on_response: bool) -> ScriptRule {
        ScriptRule {
            id: RuleId::new(),
            name: host.into(),
            r#match: TrafficMatch {
                host_glob: Some(host.into()),
                ..Default::default()
            },
            enabled: true,
            run_on_request: on_request,
            run_on_response: on_response,
            mock_api: false,
            source: String::new(),
        }
    }

    fn facts(host: &str) -> RequestFacts<'_> {
        RequestFacts {
            host,
            path: "/",
            method: "GET",
        }
    }

    #[test]
    fn first_matching_enabled_script_wins() {
        let disabled = ScriptRule {
            enabled: false,
            ..script("api.com", true, true)
        };
        let first = script("*.com", true, true);
        let rules = RuleSet {
            scripts: vec![disabled, first.clone(), script("api.com", true, true)],
            ..Default::default()
        };
        assert_eq!(
            rules.script_for(&facts("api.com"), ScriptPhase::Request),
            Some(&first)
        );
        assert_eq!(
            rules.script_for(&facts("api.org"), ScriptPhase::Request),
            None
        );
    }

    #[test]
    fn phase_flags_select_the_matched_script_only() {
        let rules = RuleSet {
            scripts: vec![script("a.com", false, true), script("a.com", true, true)],
            ..Default::default()
        };
        // Совпавший первым скрипт не работает на запросе — второй не
        // подхватывает: одно правило на обмен.
        assert_eq!(
            rules.script_for(&facts("a.com"), ScriptPhase::Request),
            None
        );
        assert!(
            rules
                .script_for(&facts("a.com"), ScriptPhase::Response)
                .is_some()
        );
    }

    #[test]
    fn mock_api_always_runs_on_request() {
        let mock = ScriptRule {
            mock_api: true,
            ..script("m.com", false, false)
        };
        assert!(mock.runs_on(ScriptPhase::Request));
        assert!(!mock.runs_on(ScriptPhase::Response));
    }

    #[test]
    fn global_switch_disables_all_scripts() {
        let rules = RuleSet {
            scripts: vec![script("a.com", true, true)],
            scripts_enabled: false,
            ..Default::default()
        };
        assert_eq!(
            rules.script_for(&facts("a.com"), ScriptPhase::Request),
            None
        );
    }

    #[test]
    fn legacy_script_json_gets_defaults() {
        let rule: ScriptRule = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000001","name":"s","match":{}}"#,
        )
        .expect("deserialize");
        assert!(rule.enabled && rule.run_on_request && rule.run_on_response);
        assert!(!rule.mock_api);
        assert!(rule.source.is_empty());
    }
}
