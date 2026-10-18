//! Порт системного прокси macOS (networksetup; реализация — infrastructure).

use domain::errors::DomainError;
use domain::value_objects::host::SocketAddrVo;

use crate::dto::proxy_config::SystemProxyStatusDto;

#[async_trait::async_trait]
pub trait SystemProxyService: Send + Sync {
    /// Включить системный HTTP/HTTPS-прокси на `listen`.
    async fn enable(&self, listen: SocketAddrVo, bypass: Vec<String>) -> Result<(), DomainError>;

    async fn disable(&self) -> Result<(), DomainError>;

    async fn status(&self) -> Result<SystemProxyStatusDto, DomainError>;
}
