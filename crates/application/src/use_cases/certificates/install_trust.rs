//! Установка доверия CA в системный keychain (macOS: GUI-промпт пароля
//! администратора через osascript — предупреждение показывает UI).

use std::sync::Arc;

use domain::errors::DomainError;

use crate::dto::certificates::TrustStatusDto;
use crate::services::CertificateService;

pub struct InstallTrustUseCase {
    certificate_service: Arc<dyn CertificateService>,
}

impl InstallTrustUseCase {
    pub fn new(certificate_service: Arc<dyn CertificateService>) -> Self {
        Self {
            certificate_service,
        }
    }

    pub async fn execute(&self) -> Result<TrustStatusDto, DomainError> {
        self.certificate_service.install_trust().await
    }
}
