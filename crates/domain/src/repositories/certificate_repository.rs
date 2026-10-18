//! Метаданные корневого CA (сами ключи — на диске, chmod 600).

use async_trait::async_trait;

use crate::entities::certificate::CaInfo;
use crate::errors::DomainError;

#[async_trait]
pub trait CertificateRepository: Send + Sync {
    /// Метаданные CA, если он уже сгенерирован.
    async fn load_ca(&self) -> Result<Option<CaInfo>, DomainError>;

    /// Записать/обновить метаданные CA.
    async fn persist_ca(&self, info: &CaInfo) -> Result<(), DomainError>;
}
