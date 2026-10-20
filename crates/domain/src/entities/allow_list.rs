//! Allow List (как в Proxyman): пока список включён и в нём есть активные
//! хосты, crabtrap записывает и обрабатывает правилами только их; остальной
//! трафик проксируется мимо — без записи, правил, расшифровки и throttle.

use serde::{Deserialize, Serialize};

use super::rule::RuleSet;
use crate::value_objects::RuleId;
use crate::value_objects::match_pattern::host_pattern_matches;

/// Хост в Allow List.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowEntry {
    pub id: RuleId,
    /// Шаблон хоста (`api.example.com`, `*.example.com` — и сам домен).
    pub host_pattern: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl AllowEntry {
    pub fn new(host_pattern: impl Into<String>) -> Self {
        Self {
            id: RuleId::new(),
            host_pattern: host_pattern.into(),
            enabled: true,
        }
    }

    pub fn matches_host(&self, host: &str) -> bool {
        self.enabled && host_pattern_matches(&self.host_pattern, host)
    }
}

impl RuleSet {
    /// Allow List действует: включён и есть хоть один активный хост
    /// (включённый, но пустой список не должен молча прятать весь трафик).
    pub fn allow_list_active(&self) -> bool {
        self.allow_enabled && self.allow_list.iter().any(|e| e.enabled)
    }

    /// Записывать ли трафик хоста (и применять к нему правила).
    pub fn records_host(&self, host: &str) -> bool {
        !self.allow_list_active() || self.allow_list.iter().any(|e| e.matches_host(host))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(enabled: bool, entries: Vec<AllowEntry>) -> RuleSet {
        RuleSet {
            allow_enabled: enabled,
            allow_list: entries,
            ..RuleSet::default()
        }
    }

    #[test]
    fn everything_is_recorded_while_the_list_is_off_or_empty() {
        assert!(RuleSet::default().records_host("any.test"));
        assert!(rules(false, vec![AllowEntry::new("api.test")]).records_host("other.test"));
        assert!(
            rules(true, Vec::new()).records_host("other.test"),
            "enabled but empty"
        );
        let disabled_entry = AllowEntry {
            enabled: false,
            ..AllowEntry::new("api.test")
        };
        assert!(rules(true, vec![disabled_entry]).records_host("other.test"));
    }

    #[test]
    fn active_list_records_only_matching_hosts() {
        let set = rules(
            true,
            vec![
                AllowEntry::new("*.example.com"),
                AllowEntry::new("API.test"),
            ],
        );
        assert!(set.allow_list_active());
        assert!(
            set.records_host("example.com"),
            "*.domain covers the bare domain"
        );
        assert!(set.records_host("cdn.example.com"));
        assert!(set.records_host("api.test"));
        assert!(!set.records_host("ads.tracker.test"));
    }

    #[test]
    fn legacy_rules_json_has_the_list_off() {
        let set: RuleSet = serde_json::from_str("{}").expect("json");
        assert!(!set.allow_enabled);
        assert!(set.allow_list.is_empty());
    }
}
