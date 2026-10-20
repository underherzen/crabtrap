//! SSL Proxying: каким хостам MITM разрешён (include) и каким запрещён
//! (exclude, «Don't decrypt» — как Exclude в Charles). Исключение сильнее
//! включения: `*` + exclude `*.apple.com` расшифровывает всё, кроме Apple.
//!
//! Исключения по умолчанию — домены с системным пиннингом Apple
//! (CertificatePinning.plist в macOS/iOS): их поддомены принимают только
//! сертификаты от CA Apple, пользовательский корень не помогает — MITM
//! ломает сайт и системные сервисы, поэтому такие хосты идут туннелем.

use serde::{Deserialize, Serialize};

use crate::value_objects::RuleId;
use crate::value_objects::match_pattern::host_pattern_matches;

/// Домены с системным пиннингом Apple (исключения по умолчанию).
pub const APPLE_PINNED_HOSTS: [&str; 7] = [
    "*.apple.com",
    "*.icloud.com",
    "*.icloud.com.cn",
    "*.cdn-apple.com",
    "*.mzstatic.com",
    "*.itunes.com",
    "*.apple-cloudkit.com",
];

/// Версия набора умолчаний SSL Proxying: rules.json с меньшей версией
/// получает недостающие исключения один раз (удалённые потом не вернутся).
pub const SSL_DEFAULTS_VERSION: u32 = 1;

/// Запись SSL-Proxying: хост, которому MITM разрешён или запрещён.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SslProxyEntry {
    pub id: RuleId,
    /// Glob хоста (`*.example.com`).
    pub host_pattern: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Не расшифровывать (туннель) — перекрывает совпавшие include-записи.
    #[serde(default)]
    pub exclude: bool,
}

fn default_true() -> bool {
    true
}

impl SslProxyEntry {
    /// Включённая запись «расшифровывать» для шаблона.
    pub fn include(host_pattern: impl Into<String>) -> Self {
        Self {
            id: RuleId::new(),
            host_pattern: host_pattern.into(),
            enabled: true,
            exclude: false,
        }
    }

    /// Включённая запись «не расшифровывать» для шаблона.
    pub fn exclude(host_pattern: impl Into<String>) -> Self {
        Self {
            exclude: true,
            ..Self::include(host_pattern)
        }
    }

    /// `*.example.com` покрывает и голый `example.com` (семантика Charles),
    /// прочие шаблоны — обычный glob.
    pub fn matches_host(&self, host: &str) -> bool {
        self.enabled && host_pattern_matches(&self.host_pattern, host)
    }
}

/// Расшифровывать ли хост: есть совпавшая include-запись и нет совпавшей
/// exclude-записи (выключенные записи не участвуют).
pub fn should_decrypt(entries: &[SslProxyEntry], host: &str) -> bool {
    let matched = |exclude: bool| {
        entries
            .iter()
            .any(|e| e.exclude == exclude && e.matches_host(host))
    };
    matched(false) && !matched(true)
}

/// Хоть одна включённая include-запись (SSL Proxying что-то расшифровывает).
pub fn any_decrypting(entries: &[SslProxyEntry]) -> bool {
    entries.iter().any(|e| e.enabled && !e.exclude)
}

/// Исключения по умолчанию, которых ещё нет в списке (по шаблону).
pub fn missing_default_excludes(entries: &[SslProxyEntry]) -> Vec<SslProxyEntry> {
    APPLE_PINNED_HOSTS
        .iter()
        .filter(|pattern| {
            !entries
                .iter()
                .any(|e| e.exclude && e.host_pattern.eq_ignore_ascii_case(pattern))
        })
        .map(|pattern| SslProxyEntry::exclude(*pattern))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclude_wins_over_wildcard_include() {
        let entries = vec![
            SslProxyEntry::include("*"),
            SslProxyEntry::exclude("*.apple.com"),
        ];
        assert!(should_decrypt(&entries, "example.com"));
        assert!(!should_decrypt(&entries, "www.apple.com"));
        assert!(!should_decrypt(&entries, "apple.com"));
    }

    #[test]
    fn disabled_exclude_does_not_apply() {
        let off = SslProxyEntry {
            enabled: false,
            ..SslProxyEntry::exclude("*.apple.com")
        };
        let entries = vec![SslProxyEntry::include("*"), off];
        assert!(should_decrypt(&entries, "www.apple.com"));
    }

    #[test]
    fn only_excludes_decrypt_nothing() {
        let entries = vec![SslProxyEntry::exclude("*.apple.com")];
        assert!(!should_decrypt(&entries, "example.com"));
        assert!(!any_decrypting(&entries));
        assert!(!should_decrypt(&[], "example.com"));
    }

    #[test]
    fn missing_defaults_skip_present_patterns() {
        let entries = vec![SslProxyEntry::exclude("*.APPLE.com")];
        let missing = missing_default_excludes(&entries);
        assert_eq!(missing.len(), APPLE_PINNED_HOSTS.len() - 1);
        assert!(missing.iter().all(|e| e.exclude && e.enabled));
        assert!(!missing.iter().any(|e| e.host_pattern == "*.apple.com"));
    }

    #[test]
    fn legacy_entry_without_exclude_field_decrypts() {
        let json = format!(r#"{{"id":"{}","host_pattern":"*"}}"#, RuleId::new());
        let entry: SslProxyEntry = serde_json::from_str(&json).expect("legacy entry");
        assert!(entry.enabled && !entry.exclude);
    }
}
