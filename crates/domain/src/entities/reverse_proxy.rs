//! Reverse Proxy (как в Proxyman): локальный порт → целевой сервер. Клиент
//! обращается к `http://localhost:9090`, crabtrap передаёт запросы цели и
//! записывает их — для клиентов, которые не умеют HTTP-прокси.

use serde::{Deserialize, Serialize};

use super::network::ReverseProxyConfig;
use super::rule::RuleSet;
use crate::value_objects::RuleId;

/// Запись Reverse Proxy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseProxyEntry {
    pub id: RuleId,
    /// Локальный порт (листенер на интерфейсе основного прокси).
    pub listen_port: u16,
    /// Цель — origin без пути: `https://api.example.com`, `http://localhost:3000`.
    pub target: String,
    /// Отправлять Host клиента (`localhost:9090`), а не хоста цели.
    #[serde(default)]
    pub preserve_host: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl ReverseProxyEntry {
    pub fn new(listen_port: u16, target: impl Into<String>) -> Self {
        Self {
            id: RuleId::new(),
            listen_port,
            target: target.into(),
            preserve_host: false,
            enabled: true,
        }
    }
}

impl RuleSet {
    /// Листенеры для движка: включённые записи, пока инструмент включён.
    pub fn effective_reverse_proxies(&self) -> Vec<ReverseProxyConfig> {
        self.reverse_proxies
            .iter()
            .filter(|e| self.reverse_proxy_enabled && e.enabled)
            .map(|e| ReverseProxyConfig {
                listen_port: e.listen_port,
                target: e.target.clone(),
                preserve_host: e.preserve_host,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_enabled_entries_of_an_enabled_tool_reach_the_engine() {
        let off = ReverseProxyEntry {
            enabled: false,
            ..ReverseProxyEntry::new(9091, "http://localhost:3001")
        };
        let rules = RuleSet {
            reverse_proxies: vec![ReverseProxyEntry::new(9090, "https://api.example.com"), off],
            ..RuleSet::default()
        };
        let configs = rules.effective_reverse_proxies();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].listen_port, 9090);
        assert_eq!(configs[0].target, "https://api.example.com");
        let tool_off = RuleSet {
            reverse_proxy_enabled: false,
            ..rules
        };
        assert!(tool_off.effective_reverse_proxies().is_empty());
    }

    #[test]
    fn old_rules_json_has_the_tool_on_and_no_entries() {
        let rules: RuleSet = serde_json::from_str("{}").expect("json");
        assert!(rules.reverse_proxy_enabled);
        assert!(rules.reverse_proxies.is_empty());
    }
}
