//! Регенерация CA: новый ключ+сертификат, старые файлы переименовываются
//! в `*.old-<ts>`, доверие сбрасывается (нужно переустановить).

use std::sync::Arc;

use domain::errors::DomainError;

use crate::dto::certificates::CaStatusDto;
use crate::services::CertificateService;

pub struct RegenerateCaUseCase {
    certificate_service: Arc<dyn CertificateService>,
}

impl RegenerateCaUseCase {
    pub fn new(certificate_service: Arc<dyn CertificateService>) -> Self {
        Self {
            certificate_service,
        }
    }

    pub async fn execute(&self) -> Result<CaStatusDto, DomainError> {
        self.certificate_service.regenerate_ca().await
    }
}
