//! DTO правил: редактор RuleSet, throttle, брейкпоинты.

use serde::{Deserialize, Serialize};

use domain::entities::rule::BreakPhase;
use domain::entities::rule::{
    AllowEntry, BlockRule, BreakpointRule, MapLocalRule, MapRemoteRule, RewriteRule, RuleSet,
    SslProxyEntry, ThrottleProfile,
};
use domain::entities::script_rule::ScriptRule;
use domain::value_objects::{ExchangeId, InterceptId, RuleId, TrafficMatch};

/// Снимок правил для UI-редактора (зеркало RuleSet + версия).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleSetDto {
    pub version: u64,
    /// Глобальный тумблер SSL Proxying (отсутствует в старых данных → on).
    #[serde(default = "default_true")]
    pub ssl_proxy_enabled: bool,
    #[serde(default)]
    pub ssl_proxy: Vec<SslProxyEntry>,
    /// Добавленные умолчания SSL Proxying (иначе удалённые исключения
    /// вернулись бы после сохранения).
    #[serde(default)]
    pub ssl_defaults_version: u32,
    #[serde(default = "default_true")]
    pub blocks_enabled: bool,
    #[serde(default = "default_true")]
    pub map_local_enabled: bool,
    #[serde(default = "default_true")]
    pub map_remote_enabled: bool,
    #[serde(default = "default_true")]
    pub throttle_enabled: bool,
    #[serde(default = "default_true")]
    pub rewrite_enabled: bool,
    #[serde(default = "default_true")]
    pub breakpoints_enabled: bool,
    #[serde(default)]
    pub rewrite: Vec<RewriteRule>,
    #[serde(default)]
    pub map_local: Vec<MapLocalRule>,
    #[serde(default)]
    pub map_remote: Vec<MapRemoteRule>,
    #[serde(default)]
    pub blocks: Vec<BlockRule>,
    #[serde(default)]
    pub breakpoints: Vec<BreakpointRule>,
    #[serde(default)]
    pub throttle: Option<ThrottleProfile>,
    #[serde(default = "default_true")]
    pub scripts_enabled: bool,
    #[serde(default)]
    pub scripts: Vec<ScriptRule>,
    #[serde(default)]
    pub allow_enabled: bool,
    #[serde(default)]
    pub allow_list: Vec<AllowEntry>,
}

fn default_true() -> bool {
    true
}

impl Default for RuleSetDto {
    fn default() -> Self {
        Self::from_rule_set(&RuleSet::default())
    }
}

impl RuleSetDto {
    pub fn from_rule_set(rules: &RuleSet) -> Self {
        Self {
            version: rules.version,
            ssl_proxy_enabled: rules.ssl_proxy_enabled,
            ssl_proxy: rules.ssl_proxy.clone(),
            ssl_defaults_version: rules.ssl_defaults_version,
            blocks_enabled: rules.blocks_enabled,
            map_local_enabled: rules.map_local_enabled,
            map_remote_enabled: rules.map_remote_enabled,
            throttle_enabled: rules.throttle_enabled,
            rewrite_enabled: rules.rewrite_enabled,
            breakpoints_enabled: rules.breakpoints_enabled,
            rewrite: rules.rewrite.clone(),
            map_local: rules.map_local.clone(),
            map_remote: rules.map_remote.clone(),
            blocks: rules.blocks.clone(),
            breakpoints: rules.breakpoints.clone(),
            throttle: rules.throttle.clone(),
            scripts_enabled: rules.scripts_enabled,
            scripts: rules.scripts.clone(),
            allow_enabled: rules.allow_enabled,
            allow_list: rules.allow_list.clone(),
        }
    }

    pub fn into_rule_set(self) -> RuleSet {
        RuleSet {
            version: self.version,
            ssl_proxy_enabled: self.ssl_proxy_enabled,
            ssl_proxy: self.ssl_proxy,
            ssl_defaults_version: self.ssl_defaults_version,
            blocks_enabled: self.blocks_enabled,
            map_local_enabled: self.map_local_enabled,
            map_remote_enabled: self.map_remote_enabled,
            throttle_enabled: self.throttle_enabled,
            rewrite_enabled: self.rewrite_enabled,
            breakpoints_enabled: self.breakpoints_enabled,
            rewrite: self.rewrite,
            map_local: self.map_local,
            map_remote: self.map_remote,
            blocks: self.blocks,
            breakpoints: self.breakpoints,
            throttle: self.throttle,
            scripts_enabled: self.scripts_enabled,
            scripts: self.scripts,
            allow_enabled: self.allow_enabled,
            allow_list: self.allow_list,
        }
    }
}

/// Спека throttle для команды движку (без id — может быть анонимной).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThrottleSpecDto {
    pub name: String,
    #[serde(default)]
    pub down_bps: Option<u64>,
    #[serde(default)]
    pub up_bps: Option<u64>,
    #[serde(default)]
    pub latency_ms: u64,
    #[serde(default)]
    pub r#match: Option<TrafficMatch>,
}

impl ThrottleSpecDto {
    /// Команда движку из сохранённого профиля.
    pub fn from_profile(p: &ThrottleProfile) -> Self {
        Self {
            name: p.name.clone(),
            down_bps: p.down_bps,
            up_bps: p.up_bps,
            latency_ms: p.latency_ms,
            r#match: p.r#match.clone(),
        }
    }
}

/// Перехваченное сообщение (request ИЛИ response) для модалки брейкпоинта.
/// method/uri/status опциональны — фаза определяет, что заполнено.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterceptableMessageDto {
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub headers: Vec<domain::value_objects::HeaderEntry>,
    /// Тело без Content-Encoding; `None` — не загружено (больше капа,
    /// потоковое) — редактировать нельзя, уходит как было.
    #[serde(default)]
    pub body: Option<Vec<u8>>,
}

/// Что показывать в модалке брейкпоинта.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterceptPreviewDto {
    pub intercept_id: InterceptId,
    pub exchange_id: ExchangeId,
    pub phase: BreakPhase,
    /// URL запроса (на фазе ответа — заголовок окна).
    #[serde(default)]
    pub url: String,
    pub message: InterceptableMessageDto,
}

/// Решение пользователя по брейкпоинту.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InterceptResolutionDto {
    /// Продолжить (опционально с отредактированным сообщением).
    Resume {
        #[serde(default)]
        edited: Option<InterceptableMessageDto>,
    },
    /// Оборвать обмен.
    Abort,
}

/// Заготовка для UI-редактора: генерация id новых правил.
pub fn new_rule_id() -> RuleId {
    RuleId::new()
}
