//! DNS Spoofing (как в Proxyman): хост разрешается в заданный IP вместо
//! системного DNS. Запрос, Host, SNI и сертификат — по исходному имени;
//! меняется только адрес, куда crabtrap подключается.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use super::network::DnsOverride;
use super::rule::RuleSet;
use crate::value_objects::RuleId;

/// Хост → IP в списке DNS Spoofing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsSpoofEntry {
    pub id: RuleId,
    /// Шаблон хоста (`api.example.com`, `*.example.com` — и сам домен).
    pub host_pattern: String,
    pub ip: IpAddr,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl DnsSpoofEntry {
    pub fn new(host_pattern: impl Into<String>, ip: IpAddr) -> Self {
        Self {
            id: RuleId::new(),
            host_pattern: host_pattern.into(),
            ip,
            enabled: true,
        }
    }
}

impl RuleSet {
    /// Таблица подмены для движка: включённые записи, пока инструмент
    /// включён (иначе — пусто, обычный DNS).
    pub fn effective_dns_overrides(&self) -> Vec<DnsOverride> {
        self.dns_spoof
            .iter()
            .filter(|e| self.dns_spoof_enabled && e.enabled)
            .map(|e| DnsOverride::new(e.host_pattern.clone(), e.ip))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("ip")
    }

    #[test]
    fn only_enabled_entries_of_an_enabled_tool_reach_the_engine() {
        let disabled = DnsSpoofEntry {
            enabled: false,
            ..DnsSpoofEntry::new("off.test", ip("10.0.0.2"))
        };
        let rules = RuleSet {
            dns_spoof: vec![
                DnsSpoofEntry::new("*.example.com", ip("10.0.0.1")),
                disabled,
            ],
            ..RuleSet::default()
        };
        assert_eq!(
            rules.effective_dns_overrides(),
            vec![DnsOverride::new("*.example.com", ip("10.0.0.1"))]
        );
        let tool_off = RuleSet {
            dns_spoof_enabled: false,
            ..rules
        };
        assert!(tool_off.effective_dns_overrides().is_empty());
    }

    #[test]
    fn old_rules_json_has_the_tool_on_and_no_entries() {
        let rules: RuleSet = serde_json::from_str("{}").expect("json");
        assert!(rules.dns_spoof_enabled);
        assert!(rules.dns_spoof.is_empty());
    }
}
