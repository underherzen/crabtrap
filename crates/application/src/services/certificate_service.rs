//! Порт работы с сертификатами (реализация — infrastructure, Phase 3:
//! rcgen CA + osascript/security для доверия).

use std::path::PathBuf;

use domain::errors::DomainError;

use crate::dto::certificates::{CaStatusDto, LeafCertDto, TrustStatusDto};

#[async_trait::async_trait]
pub trait CertificateService: Send + Sync {
    /// Гарантировать наличие CA (создать при первом запуске, переиспользовать).
    async fn ensure_ca(&self) -> Result<CaStatusDto, DomainError>;

    /// Выпустить лист для хоста (движок кэширует их LRU по host+alpn).
    async fn issue_leaf(&self, host: &str, alpn: &[String]) -> Result<LeafCertDto, DomainError>;

    async fn trust_status(&self) -> Result<TrustStatusDto, DomainError>;

    /// Установить доверие (osascript с админ-правами; результат опрашивать
    /// затем через `trust_status`).
    async fn install_trust(&self) -> Result<TrustStatusDto, DomainError>;

    /// Перегенерировать CA (сбрасывает доверие!).
    async fn regenerate_ca(&self) -> Result<CaStatusDto, DomainError>;

    /// Путь к ca.pem (для раздачи устройствам по `http://<mac-ip>:<port>/ssl`).
    fn ca_cert_path(&self) -> PathBuf;
}
