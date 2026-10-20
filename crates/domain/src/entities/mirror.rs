//! Mirror (как в Charles): ответы выбранных хостов сохраняются на диск
//! деревом `<папка>/<host>/<path>` — копия сайта или API, какой её видел
//! клиент. Последний ответ перезаписывает файл.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::rule::RuleSet;
use crate::value_objects::RuleId;
use crate::value_objects::match_pattern::host_pattern_matches;

/// Хост, ответы которого зеркалируются, и папка для них.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorEntry {
    pub id: RuleId,
    /// Шаблон хоста (`api.example.com`, `*.example.com` — и сам домен).
    pub host_pattern: String,
    /// Корневая папка; файлы — в `<folder>/<host>/…`.
    pub folder: PathBuf,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl MirrorEntry {
    pub fn new(host_pattern: impl Into<String>, folder: impl Into<PathBuf>) -> Self {
        Self {
            id: RuleId::new(),
            host_pattern: host_pattern.into(),
            folder: folder.into(),
            enabled: true,
        }
    }
}

impl RuleSet {
    /// Папка для ответов хоста: первая включённая запись, пока инструмент
    /// включён.
    pub fn mirror_folder_for(&self, host: &str) -> Option<&Path> {
        if !self.mirror_enabled {
            return None;
        }
        self.mirror
            .iter()
            .find(|e| e.enabled && host_pattern_matches(&e.host_pattern, host))
            .map(|e| e.folder.as_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_enabled_matching_entry_of_an_enabled_tool_wins() {
        let off = MirrorEntry {
            enabled: false,
            ..MirrorEntry::new("api.test", "/off")
        };
        let rules = RuleSet {
            mirror: vec![
                off,
                MirrorEntry::new("*.test", "/all"),
                MirrorEntry::new("api.test", "/api"),
            ],
            ..RuleSet::default()
        };
        assert_eq!(rules.mirror_folder_for("api.test"), Some(Path::new("/all")));
        assert_eq!(rules.mirror_folder_for("other.org"), None);
        let tool_off = RuleSet {
            mirror_enabled: false,
            ..rules
        };
        assert_eq!(tool_off.mirror_folder_for("api.test"), None);
    }

    #[test]
    fn old_rules_json_has_the_tool_on_and_no_entries() {
        let rules: RuleSet = serde_json::from_str("{}").expect("json");
        assert!(rules.mirror_enabled);
        assert!(rules.mirror.is_empty());
    }
}
