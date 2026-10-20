//! RuleSet — все правила перехвата/маршрутизации. Персистентны в rules.json.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::script_rule::ScriptRule;
pub use super::ssl_proxy::SslProxyEntry;
use super::ssl_proxy::{SSL_DEFAULTS_VERSION, any_decrypting, missing_default_excludes};
use crate::value_objects::http_vo::HeaderEntry;
use crate::value_objects::{RuleId, TrafficMatch};

/// Снимок всех правил (версия растёт на каждом изменении).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleSet {
    /// Версия назначается RuleRepository при сохранении.
    pub version: u64,
    /// Глобальный тумблер SSL Proxying (галочка «Enable SSL Proxying» в
    /// Charles): выключение не трогает список хостов. Старые rules.json без
    /// поля читаются как `true` — поведение до появления флага.
    pub ssl_proxy_enabled: bool,
    pub ssl_proxy: Vec<SslProxyEntry>,
    /// Какие умолчания SSL Proxying уже добавлены (исключения Apple —
    /// [`RuleSet::with_ssl_defaults`]); старый rules.json — 0.
    pub ssl_defaults_version: u32,
    /// Галочки «Enable <инструмент>» (Tools): выключенный инструмент не
    /// применяется, но его правила сохраняются. Старый rules.json — `true`.
    pub blocks_enabled: bool,
    pub map_local_enabled: bool,
    pub map_remote_enabled: bool,
    pub throttle_enabled: bool,
    /// Галочка «Enable Rewrite» (Phase 7). Старый rules.json — `true`.
    pub rewrite_enabled: bool,
    /// Галочка «Enable Breakpoints» (Phase 7). Старый rules.json — `true`.
    pub breakpoints_enabled: bool,
    pub rewrite: Vec<RewriteRule>,
    pub map_local: Vec<MapLocalRule>,
    pub map_remote: Vec<MapRemoteRule>,
    pub blocks: Vec<BlockRule>,
    pub breakpoints: Vec<BreakpointRule>,
    pub throttle: Option<ThrottleProfile>,
    /// Галочка «Enable Scripting» (Track S). Старый rules.json — `true`.
    pub scripts_enabled: bool,
    /// Правила-скрипты (Track S).
    pub scripts: Vec<ScriptRule>,
}

impl Default for RuleSet {
    fn default() -> Self {
        Self {
            version: 0,
            ssl_proxy_enabled: true,
            ssl_proxy: Vec::new(),
            ssl_defaults_version: 0,
            blocks_enabled: true,
            map_local_enabled: true,
            map_remote_enabled: true,
            throttle_enabled: true,
            rewrite_enabled: true,
            breakpoints_enabled: true,
            rewrite: Vec::new(),
            map_local: Vec::new(),
            map_remote: Vec::new(),
            blocks: Vec::new(),
            breakpoints: Vec::new(),
            throttle: None,
            scripts_enabled: true,
            scripts: Vec::new(),
        }
    }
}

impl RuleSet {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Профиль throttle для движка: `None`, если throttle выключен галочкой.
    pub fn effective_throttle(&self) -> Option<&ThrottleProfile> {
        self.throttle.as_ref().filter(|_| self.throttle_enabled)
    }

    /// SSL Proxying реально расшифровывает хоть что-то.
    pub fn ssl_proxying_active(&self) -> bool {
        self.ssl_proxy_enabled && any_decrypting(&self.ssl_proxy)
    }

    /// Добавить недостающие умолчания SSL Proxying (исключения доменов с
    /// пиннингом Apple) — один раз на версию: удалённое пользователем
    /// исключение при следующей загрузке не вернётся.
    pub fn with_ssl_defaults(self) -> Self {
        if self.ssl_defaults_version >= SSL_DEFAULTS_VERSION {
            return self;
        }
        let missing = missing_default_excludes(&self.ssl_proxy);
        Self {
            ssl_proxy: self.ssl_proxy.into_iter().chain(missing).collect(),
            ssl_defaults_version: SSL_DEFAULTS_VERSION,
            ..self
        }
    }

    /// Список для движка: пусто, если глобальный тумблер выключен
    /// (движок сам пропускает выключенные записи).
    pub fn effective_ssl_entries(&self) -> Vec<SslProxyEntry> {
        if self.ssl_proxy_enabled {
            self.ssl_proxy.clone()
        } else {
            Vec::new()
        }
    }

    /// Тумблер SSL Proxying без потери списка. Включение при отсутствии
    /// активных include-записей добавляет `*` (тулбар-кнопка = «MITM всех
    /// хостов», исключения остаются в силе).
    pub fn with_ssl_proxying(self, enable: bool) -> Self {
        let needs_wildcard = enable && !any_decrypting(&self.ssl_proxy);
        let ssl_proxy = if needs_wildcard {
            self.ssl_proxy
                .into_iter()
                .chain(std::iter::once(SslProxyEntry::include("*")))
                .collect()
        } else {
            self.ssl_proxy
        };
        Self {
            ssl_proxy_enabled: enable,
            ssl_proxy,
            ..self
        }
    }
}

fn default_true() -> bool {
    true
}

/// Профиль throttle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThrottleProfile {
    pub id: RuleId,
    pub name: String,
    /// Пропускная способность вниз (байт/сек); None — не ограничивать.
    #[serde(default)]
    pub down_bps: Option<u64>,
    /// Пропускная способность вверх (байт/сек); None — не ограничивать.
    #[serde(default)]
    pub up_bps: Option<u64>,
    /// Добавочная задержка на обмен, мс.
    #[serde(default)]
    pub latency_ms: u64,
    #[serde(default, rename = "match")]
    pub r#match: Option<TrafficMatch>,
}

/// Куда применяется rewrite-правило.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewriteLocation {
    Request,
    Response,
}

/// Rewrite-правило.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RewriteRule {
    pub id: RuleId,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub location: RewriteLocation,
    #[serde(default, rename = "match")]
    pub r#match: Option<TrafficMatch>,
    #[serde(default)]
    pub ops: Vec<RewriteOp>,
}

/// Атомарная операция rewrite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RewriteOp {
    /// Добавить заголовок (дубликаты допустимы, как в Charles).
    AddHeader { name: String, value: String },
    /// Заменить значение заголовка (несуществующий — добавить).
    ReplaceHeader { name: String, value: String },
    /// Удалить заголовок.
    RemoveHeader { name: String },
    /// Поиск/замена в теле.
    BodyFindReplace {
        find: String,
        replace: String,
        #[serde(default)]
        regex: bool,
    },
    /// Перезапись URL (подстрока или regex).
    UrlRewrite {
        find: String,
        replace: String,
        #[serde(default)]
        regex: bool,
    },
    /// Подмена статуса ответа.
    SetStatus { status: u16 },
}

/// Map Local: ответ берётся из локального файла.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapLocalRule {
    pub id: RuleId,
    #[serde(rename = "match")]
    pub r#match: TrafficMatch,
    /// Файл (отдаётся как есть) или каталог (к нему добавляется path запроса).
    pub local_path: PathBuf,
    /// Код ответа, когда файл найден (нет файла — всегда 404).
    #[serde(default = "default_map_local_status")]
    pub status: u16,
    /// Content-Type ответа; `None` — по расширению файла.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Дополнительные заголовки ответа (CORS, Cache-Control, Set-Cookie…),
    /// по порядку, повторы разрешены. Только когда файл найден.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<HeaderEntry>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Код ответа Map Local по умолчанию (и для старых rules.json).
pub const DEFAULT_MAP_LOCAL_STATUS: u16 = 200;

fn default_map_local_status() -> u16 {
    DEFAULT_MAP_LOCAL_STATUS
}

/// Map Remote: запрос уходит на другой сервер.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapRemoteRule {
    pub id: RuleId,
    #[serde(rename = "match")]
    pub r#match: TrafficMatch,
    /// Целевой origin (`https://staging.example.com`).
    pub remote: String,
    #[serde(default)]
    pub preserve_host: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Действие при блокировке.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockAction {
    /// Ответить кодом (тело — пустое).
    Status(u16),
    /// Оборвать соединение.
    Close,
}

/// Правило блокировки.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockRule {
    pub id: RuleId,
    #[serde(rename = "match")]
    pub r#match: TrafficMatch,
    pub action: BlockAction,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Фаза брейкпоинта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakPhase {
    Request,
    Response,
    Both,
}

/// Правило брейкпоинта (парковка обмена до решения пользователя).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BreakpointRule {
    pub id: RuleId,
    #[serde(rename = "match")]
    pub r#match: TrafficMatch,
    pub phase: BreakPhase,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl BreakPhase {
    /// Брейкпоинт с этой фазой срабатывает на `phase` (Request/Response).
    pub fn covers(self, phase: BreakPhase) -> bool {
        self == BreakPhase::Both || self == phase
    }
}

impl RuleSet {
    /// Брейкпоинт для обмена на фазе: первое включённое совпавшее правило.
    /// Условие — по запросу (на фазе ответа — итоговому), как у скриптов.
    pub fn breakpoint_for(
        &self,
        facts: &super::rule_routing::RequestFacts<'_>,
        phase: BreakPhase,
    ) -> Option<&BreakpointRule> {
        if !self.breakpoints_enabled {
            return None;
        }
        self.breakpoints.iter().find(|rule| {
            rule.enabled
                && rule.phase.covers(phase)
                && super::rule_routing::matches_request(&rule.r#match, facts)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssl_defaults_are_added_once() {
        let legacy = RuleSet {
            ssl_proxy: vec![SslProxyEntry::include("*")],
            ..RuleSet::default()
        };
        let migrated = legacy.with_ssl_defaults();
        assert!(
            migrated
                .ssl_proxy
                .iter()
                .any(|e| e.exclude && e.host_pattern == "*.apple.com")
        );
        assert_eq!(
            migrated.ssl_defaults_version,
            super::super::ssl_proxy::SSL_DEFAULTS_VERSION
        );

        // Удалённое исключение не возвращается при следующей загрузке.
        let trimmed = RuleSet {
            ssl_proxy: migrated
                .ssl_proxy
                .iter()
                .filter(|e| !e.exclude)
                .cloned()
                .collect(),
            ..migrated
        };
        assert_eq!(trimmed.clone().with_ssl_defaults(), trimmed);
    }

    #[test]
    fn wildcard_is_added_when_only_excludes_exist() {
        let rules = RuleSet::default()
            .with_ssl_defaults()
            .with_ssl_proxying(true);
        assert!(
            rules
                .ssl_proxy
                .iter()
                .any(|e| e.host_pattern == "*" && !e.exclude)
        );
        assert!(rules.ssl_proxying_active());
    }

    #[test]
    fn rule_set_defaults_from_empty_json() {
        let rs: RuleSet = serde_json::from_str("{}").expect("deserialize");
        assert_eq!(rs.version, 0);
        assert!(rs.ssl_proxy.is_empty());
        assert!(rs.is_empty());
    }

    #[test]
    fn ssl_proxy_wildcard_covers_bare_domain() {
        let entry = SslProxyEntry {
            id: RuleId::new(),
            host_pattern: "*.example.com".into(),
            enabled: true,
            exclude: false,
        };
        assert!(entry.matches_host("example.com"));
        assert!(entry.matches_host("api.example.com"));
        assert!(!entry.matches_host("example.com.evil.org"));
        assert!(!entry.matches_host("other.org"));
    }

    #[test]
    fn disabled_ssl_proxy_entry_never_matches() {
        let entry = SslProxyEntry {
            id: RuleId::new(),
            host_pattern: "*".into(),
            enabled: false,
            exclude: false,
        };
        assert!(!entry.matches_host("anything.com"));
    }

    fn entry(pattern: &str, enabled: bool) -> SslProxyEntry {
        SslProxyEntry {
            id: RuleId::new(),
            host_pattern: pattern.into(),
            enabled,
            exclude: false,
        }
    }

    #[test]
    fn ssl_pattern_matching_is_case_insensitive() {
        assert!(entry("*.Example.COM", true).matches_host("api.example.com"));
        assert!(entry("API.example.com", true).matches_host("api.example.com"));
    }

    #[test]
    fn legacy_map_local_rule_responds_200_with_auto_content_type() {
        let rule: MapLocalRule = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000001","match":{},"local_path":"/m.json"}"#,
        )
        .expect("deserialize");
        assert_eq!(rule.status, DEFAULT_MAP_LOCAL_STATUS);
        assert_eq!(rule.content_type, None);
        assert!(rule.headers.is_empty());
        let json = serde_json::to_value(&rule).expect("serialize");
        assert!(
            json.get("content_type").is_none(),
            "auto type is not written: {json}"
        );
        assert!(
            json.get("headers").is_none(),
            "no extra headers are not written: {json}"
        );
    }

    #[test]
    fn map_local_headers_round_trip_in_order() {
        let rule = MapLocalRule {
            id: RuleId::new(),
            r#match: TrafficMatch::default(),
            local_path: "/m.json".into(),
            status: 200,
            content_type: None,
            headers: vec![
                HeaderEntry::new("Set-Cookie", "a=1"),
                HeaderEntry::new("Access-Control-Allow-Origin", "*"),
                HeaderEntry::new("Set-Cookie", "b=2"),
            ],
            enabled: true,
        };
        let json = serde_json::to_string(&rule).expect("serialize");
        let back: MapLocalRule = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, rule);
    }

    #[test]
    fn legacy_rules_json_without_flag_keeps_ssl_proxying_on() {
        let rs: RuleSet = serde_json::from_str(
            r#"{"ssl_proxy":[{"id":"00000000-0000-0000-0000-000000000001","host_pattern":"*"}]}"#,
        )
        .expect("deserialize");
        assert!(rs.ssl_proxy_enabled);
        assert!(rs.ssl_proxying_active());
    }

    #[test]
    fn toggling_ssl_proxying_off_keeps_the_host_list() {
        let rs = RuleSet {
            ssl_proxy: vec![entry("*.a.com", true), entry("b.com", false)],
            ..Default::default()
        };
        let off = rs.clone().with_ssl_proxying(false);
        assert_eq!(off.ssl_proxy, rs.ssl_proxy, "list preserved");
        assert!(!off.ssl_proxying_active());
        assert!(
            off.effective_ssl_entries().is_empty(),
            "engine gets nothing"
        );

        let on = off.with_ssl_proxying(true);
        assert_eq!(on.ssl_proxy, rs.ssl_proxy, "existing enabled entry reused");
        assert_eq!(on.effective_ssl_entries(), rs.ssl_proxy);
    }

    #[test]
    fn enabling_with_no_active_entries_adds_wildcard() {
        let rs = RuleSet {
            ssl_proxy: vec![entry("b.com", false)],
            ssl_proxy_enabled: false,
            ..Default::default()
        };
        let on = rs.with_ssl_proxying(true);
        assert!(on.ssl_proxying_active());
        assert_eq!(on.ssl_proxy.len(), 2);
        assert_eq!(on.ssl_proxy[1].host_pattern, "*");
        assert_eq!(on.ssl_proxy[0].host_pattern, "b.com", "user entry kept");
    }

    #[test]
    fn breakpoint_matches_phase_and_switches() {
        let rule = |phase, enabled| BreakpointRule {
            id: RuleId::new(),
            r#match: TrafficMatch {
                host_glob: Some("api.test".into()),
                ..Default::default()
            },
            phase,
            enabled,
        };
        let facts = crate::entities::RequestFacts {
            host: "api.test",
            path: "/",
            method: "GET",
            ..Default::default()
        };
        let rules = RuleSet {
            breakpoints: vec![
                rule(BreakPhase::Response, false),
                rule(BreakPhase::Both, true),
            ],
            ..Default::default()
        };
        assert_eq!(
            rules
                .breakpoint_for(&facts, BreakPhase::Request)
                .map(|r| r.phase),
            Some(BreakPhase::Both)
        );
        assert!(rules.breakpoint_for(&facts, BreakPhase::Response).is_some());
        let off = RuleSet {
            breakpoints_enabled: false,
            ..rules
        };
        assert!(off.breakpoint_for(&facts, BreakPhase::Request).is_none());
        let legacy: BreakpointRule = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000001","match":{},"phase":"request"}"#,
        )
        .expect("legacy");
        assert!(legacy.enabled);
    }

    #[test]
    fn rule_set_roundtrips_through_json() {
        let rs = RuleSet {
            version: 7,
            blocks: vec![BlockRule {
                id: RuleId::new(),
                r#match: TrafficMatch {
                    host_glob: Some("ads.example.com".into()),
                    ..Default::default()
                },
                action: BlockAction::Status(403),
                enabled: true,
            }],
            throttle: Some(ThrottleProfile {
                id: RuleId::new(),
                name: "3G".into(),
                down_bps: Some(64 * 1024),
                up_bps: None,
                latency_ms: 500,
                r#match: None,
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&rs).expect("serialize");
        // Поле r#match сериализуется как "match" (схема rules.json).
        assert!(json.contains("\"match\""));
        let back: RuleSet = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, rs);
    }
}
