//! DTO управления движком прокси: настройки, статус, команды UI → engine.

use serde::{Deserialize, Serialize};

use domain::entities::network::{DnsOverride, ExternalProxyConfig};
use domain::entities::rule::{RuleSet, SslProxyEntry};

use crate::dto::rules::{InterceptResolutionDto, ThrottleSpecDto};

/// Настройки запуска листенера.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySettingsDto {
    /// Например, `127.0.0.1:8080`.
    pub listen_addr: String,
    pub max_concurrent_conns: usize,
}

/// Состояние движка (опрос через `ProxyEngineService::status`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EngineStatusDto {
    pub running: bool,
    pub listen_addr: Option<String>,
    pub active_connections: usize,
    pub last_error: Option<String>,
}

/// Состояние системного прокси macOS (networksetup).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SystemProxyStatusDto {
    /// ЭФФЕКТИВНЫЙ прокси (то, что реально применяют приложения в текущей
    /// сети — `scutil --proxy`): включены и HTTP, и HTTPS.
    pub enabled: bool,
    /// Эффективный HTTP-прокси `host:port` (None — не включён).
    pub server: Option<String>,
    /// Эффективный HTTPS-прокси `host:port` (None — не включён).
    #[serde(default)]
    pub secure_server: Option<String>,
    /// `host:port` прокси, прописанные в настройках сетевых сервисов
    /// (могут не действовать: другой активный сервис, VPN пересоздал свой).
    #[serde(default)]
    pub configured_servers: Vec<String>,
    #[serde(default)]
    pub bypass: Vec<String>,
    pub details: Option<String>,
}

/// Команда UI → движку. Отправляется через `Sender<EngineCommand>`
/// (async-channel), обработка — в цикле команд движка.
// Команды редкие и идут поштучно через канал: Box ради размера enum
// ничего не даёт, а усложняет каждый конструктор.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EngineCommand {
    Start {
        settings: ProxySettingsDto,
    },
    Stop,
    /// Hot-reload снимка правил без разрыва соединений.
    UpdateRules {
        snapshot: RuleSet,
    },
    UpdateSslProxying {
        entries: Vec<SslProxyEntry>,
    },
    /// None — выключить throttle.
    UpdateThrottle {
        spec: Option<ThrottleSpecDto>,
    },
    /// Решение по припаркованному брейкпоинту (Phase 7).
    ResolveIntercept {
        id: domain::value_objects::InterceptId,
        resolution: InterceptResolutionDto,
    },
    SetExternalProxy {
        cfg: Option<ExternalProxyConfig>,
    },
    SetDnsOverrides {
        entries: Vec<DnsOverride>,
    },
    /// Вкл/выкл захвата тел на лету (Phase 4, тумблер «Recording» в
    /// тулбаре): tee MonitoredBody начинает/перестаёт складывать тела.
    SetBodyCapture {
        enabled: bool,
    },
}
