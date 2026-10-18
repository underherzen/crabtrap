//! Настройки внешнего прокси (Phase 9, интерфейс — уже сейчас).

use async_trait::async_trait;

use crate::entities::network::ExternalProxyConfig;
use crate::errors::DomainError;

#[async_trait]
pub trait ExternalProxyRepository: Send + Sync {
    /// None — прямой выход в интернет.
    async fn get(&self) -> Result<Option<ExternalProxyConfig>, DomainError>;

    async fn set(&self, config: Option<ExternalProxyConfig>) -> Result<(), DomainError>;
}
