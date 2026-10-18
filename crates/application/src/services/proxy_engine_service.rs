//! Порт управления прокси-движком (реализация — крейт `engine`, Phase 2).

use domain::errors::DomainError;

use crate::dto::proxy_config::{EngineCommand, EngineStatusDto, ProxySettingsDto};

#[async_trait::async_trait]
pub trait ProxyEngineService: Send + Sync {
    /// Запустить листенеры. Повторный старт — `DomainError::Conflict`.
    async fn start(&self, settings: ProxySettingsDto) -> Result<EngineStatusDto, DomainError>;

    async fn stop(&self) -> Result<(), DomainError>;

    async fn status(&self) -> Result<EngineStatusDto, DomainError>;

    /// Послать команду в движок (sync, неблокирующе — клади в канал).
    /// Для fire-and-forget обновлений из UI (правила, throttle, брейкпоинты).
    fn send_command(&self, command: EngineCommand) -> Result<(), DomainError>;
}
