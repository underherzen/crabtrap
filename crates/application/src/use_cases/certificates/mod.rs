//! Сценарии сертификатов: статус CA, установка доверия, регенерация.
//! Раздача устройствам (`/ssl`, `/ca.pem`) живёт в движке (Phase 3);
//! UI-экран использует эти use cases.

mod get_ca_status;
mod install_trust;
mod regenerate_ca;

pub use get_ca_status::GetCaStatusUseCase;
pub use install_trust::InstallTrustUseCase;
pub use regenerate_ca::RegenerateCaUseCase;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::dto::certificates::{CaStatusDto, LeafCertDto, TrustStatusDto};
    use crate::services::CertificateService;
    use async_trait::async_trait;
    use domain::errors::DomainError;
    use futures::executor::block_on;
    use std::path::PathBuf;

    /// Мок с подсчётом вызовов.
    struct MockCertService {
        ensure: AtomicUsize,
        trust: AtomicUsize,
        install: AtomicUsize,
        regen: AtomicUsize,
    }

    impl MockCertService {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                ensure: AtomicUsize::new(0),
                trust: AtomicUsize::new(0),
                install: AtomicUsize::new(0),
                regen: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl CertificateService for MockCertService {
        async fn ensure_ca(&self) -> Result<CaStatusDto, DomainError> {
            self.ensure.fetch_add(1, Ordering::SeqCst);
            Ok(CaStatusDto {
                cert_path: PathBuf::from("/tmp/ca.pem"),
                fingerprint: Some("ab".repeat(32)),
                created_at: None,
                exists: true,
            })
        }

        async fn issue_leaf(
            &self,
            _host: &str,
            _alpn: &[String],
        ) -> Result<LeafCertDto, DomainError> {
            Ok(LeafCertDto {
                cert_pem: String::new(),
                key_pem: String::new(),
            })
        }

        async fn trust_status(&self) -> Result<TrustStatusDto, DomainError> {
            self.trust.fetch_add(1, Ordering::SeqCst);
            Ok(TrustStatusDto {
                trusted: false,
                keychain: None,
                details: None,
            })
        }

        async fn install_trust(&self) -> Result<TrustStatusDto, DomainError> {
            self.install.fetch_add(1, Ordering::SeqCst);
            Ok(TrustStatusDto {
                trusted: true,
                keychain: Some("System".into()),
                details: None,
            })
        }

        async fn regenerate_ca(&self) -> Result<CaStatusDto, DomainError> {
            self.regen.fetch_add(1, Ordering::SeqCst);
            Ok(CaStatusDto {
                cert_path: PathBuf::from("/tmp/ca.pem"),
                fingerprint: Some("cd".repeat(32)),
                created_at: None,
                exists: true,
            })
        }

        fn ca_cert_path(&self) -> PathBuf {
            PathBuf::from("/tmp/ca.pem")
        }
    }

    #[test]
    fn get_ca_status_delegates_to_service() {
        let svc = MockCertService::new();
        let uc = GetCaStatusUseCase::new(svc.clone());
        let status = block_on(uc.execute()).expect("status");
        assert!(status.exists);
        assert_eq!(status.fingerprint.as_deref(), Some(&"ab".repeat(32)[..]));
        assert_eq!(svc.ensure.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn install_trust_returns_resulting_status() {
        let svc = MockCertService::new();
        let uc = InstallTrustUseCase::new(svc.clone());
        let status = block_on(uc.execute()).expect("install");
        assert!(status.trusted);
        assert_eq!(status.keychain.as_deref(), Some("System"));
        assert_eq!(svc.install.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn regenerate_ca_returns_new_status() {
        let svc = MockCertService::new();
        let uc = RegenerateCaUseCase::new(svc.clone());
        let status = block_on(uc.execute()).expect("regen");
        assert_eq!(status.fingerprint.as_deref(), Some(&"cd".repeat(32)[..]));
        assert_eq!(svc.regen.load(Ordering::SeqCst), 1);
    }
}
