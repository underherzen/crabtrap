//! Статус корневого CA для экрана сертификатов.

use std::sync::Arc;

use domain::errors::DomainError;

use crate::dto::certificates::CaStatusDto;
use crate::services::CertificateService;

pub struct GetCaStatusUseCase {
    certificate_service: Arc<dyn CertificateService>,
}

impl GetCaStatusUseCase {
    pub fn new(certificate_service: Arc<dyn CertificateService>) -> Self {
        Self {
            certificate_service,
        }
    }

    /// Создаёт CA при первом запуске, переиспользует существующий.
    pub async fn execute(&self) -> Result<CaStatusDto, DomainError> {
        self.certificate_service.ensure_ca().await
    }
}
