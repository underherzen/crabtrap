//! MITM-терминация TLS (Phase 3): выпуск/кеширование листьев и rustls
//! server-config для расшифрованной стороны CONNECT-туннеля.
//!
//! Криптографии здесь НЕТ (решение Phase 3): весь rcgen живёт в
//! infrastructure (`RcgenCertificateService`), движок получает `LeafCertDto`
//! (PEM) через порт `CertificateService`, парсит их в
//! `rustls::sign::CertifiedKey` (PEM — pki-types, ring-провайдер) и держит
//! небольшой LRU по ключу `(host, ca_fingerprint)`. Fingerprint в ключе
//! автоматически инвалидирует листья после регенерации CA.
//!
//! `ResolvesServerCert::resolve` синхронен, поэтому лист выпускается
//! ДО начала хендшейка (`prepare` в connect.rs), а resolve только ищет
//! в кэше (SNI хоста туннеля; несовпадающий SNI получает сертификат
//! хоста CONNECT — поведение Charles).

use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use application::dto::certificates::{CaStatusDto, LeafCertDto};
use application::services::CertificateService;
use domain::errors::DomainError;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

/// Емкость LRU готовых `CertifiedKey`.
const ENGINE_LEAF_CACHE: usize = 512;

/// Псевдоним ALPN, под который выпускаются листья (Phase 11 добавит h2).
/// ALPN MITM-сервера: h2 (Phase 11) и HTTP/1.1 — клиент выбирает сам.
const MITM_ALPN: &[&str] = &["h2", "http/1.1"];

/// Выпускатель листьев движка: PEM → CertifiedKey + LRU.
pub(crate) struct CertIssuer {
    service: Arc<dyn CertificateService>,
    cache: Mutex<lru::LruCache<(String, String), Arc<CertifiedKey>>>,
    /// Fingerprint CA последнего `prepare` (ключ кэша на момент выпуска).
    ca_fingerprint: Mutex<String>,
}

impl CertIssuer {
    pub(crate) fn new(service: Arc<dyn CertificateService>) -> Self {
        Self {
            service,
            cache: Mutex::new(lru::LruCache::new(
                NonZeroUsize::new(ENGINE_LEAF_CACHE).expect("nonzero"),
            )),
            ca_fingerprint: Mutex::new(String::new()),
        }
    }

    /// Выпустить (или взять из кэша) лист для хоста ДО хендшейка.
    /// Обновляет текущий fingerprint CA (регенерация → промах → перевыпуск).
    pub(crate) async fn prepare(&self, host: &str) -> Result<(), String> {
        let status = self.service.ensure_ca().await.map_err(err)?;
        let fp = status.fingerprint.unwrap_or_default();
        let key = (host.to_string(), fp.clone());
        if self.cache.lock().expect("cert cache").contains(&key) {
            *self.ca_fingerprint.lock().expect("fp lock") = fp;
            return Ok(());
        }
        let alpn = MITM_ALPN.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let leaf = self.service.issue_leaf(host, &alpn).await.map_err(err)?;
        let certified = parse_leaf(&leaf)?;
        self.cache
            .lock()
            .expect("cert cache")
            .put(key, Arc::new(certified));
        *self.ca_fingerprint.lock().expect("fp lock") = fp;
        Ok(())
    }

    /// Синхронный поиск готового листа (для `ResolvesServerCert`).
    pub(crate) fn lookup(&self, host: &str) -> Option<Arc<CertifiedKey>> {
        let fp = self.ca_fingerprint.lock().expect("fp lock").clone();
        self.cache
            .lock()
            .expect("cert cache")
            .get(&(host.to_string(), fp))
            .cloned()
    }

    /// ServerConfig MITM-стороны: только http/1.1 (D7; h2 — Phase 11),
    /// резолвер сертификатов с fallback на хост CONNECT.
    pub(crate) fn server_config(
        self: &Arc<Self>,
        fallback_host: &str,
    ) -> Arc<rustls::ServerConfig> {
        let resolver = MitmCertResolver {
            issuer: Arc::clone(self),
            fallback_host: fallback_host.to_string(),
        };
        let mut config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(resolver));
        // Клиент без пересечения ALPN получает alert NoApplicationProtocol
        // (честный отказ); клиент без ALPN вообще — соединяется (http/1.1).
        config.alpn_protocols = MITM_ALPN.iter().map(|p| p.as_bytes().to_vec()).collect();
        Arc::new(config)
    }

    /// Статус CA (для `/ssl` и `/ca.pem`).
    pub(crate) async fn ca_status(&self) -> Result<CaStatusDto, DomainError> {
        self.service.ensure_ca().await
    }
}

fn err(e: DomainError) -> String {
    e.to_string()
}

impl std::fmt::Debug for CertIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertIssuer").finish_non_exhaustive()
    }
}

/// Резолвер сертификатов MITM-соединения.
#[derive(Debug)]
struct MitmCertResolver {
    issuer: Arc<CertIssuer>,
    /// Хост CONNECT-запроса (когда SNI не прислан или не прогрет).
    fallback_host: String,
}

impl ResolvesServerCert for MitmCertResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let sni = client_hello.server_name().map(str::to_string);
        match sni {
            Some(name) => self
                .issuer
                .lookup(&name)
                .or_else(|| self.issuer.lookup(&self.fallback_host)),
            None => self.issuer.lookup(&self.fallback_host),
        }
    }
}

/// PEM-DTO → rustls CertifiedKey (cert-chain + подписывающий ключ ring).
pub(crate) fn parse_leaf(leaf: &LeafCertDto) -> Result<CertifiedKey, String> {
    let certs = CertificateDer::pem_slice_iter(leaf.cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("leaf cert PEM: {e}"))?;
    if certs.is_empty() {
        return Err("no certificates in leaf PEM".to_string());
    }
    let key = PrivateKeyDer::from_pem_slice(leaf.key_pem.as_bytes())
        .map_err(|e| format!("leaf key PEM: {e}"))?;
    let signing = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|e| format!("unsupported leaf key kind: {e}"))?;
    Ok(CertifiedKey::new(certs, signing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use application::dto::certificates::{CaStatusDto, TrustStatusDto};
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Реальный rcgen-мок: самоподписанный CA + подписанные листья
    /// (PEM), как их отдаёт RcgenCertificateService.
    struct RcgenLikeService {
        issues: AtomicU64,
    }

    #[async_trait]
    impl CertificateService for RcgenLikeService {
        async fn ensure_ca(&self) -> Result<CaStatusDto, DomainError> {
            Ok(CaStatusDto {
                cert_path: PathBuf::from("/tmp/test-ca.pem"),
                fingerprint: Some("test-ca-fp".to_string()),
                created_at: None,
                exists: true,
            })
        }

        async fn issue_leaf(
            &self,
            host: &str,
            _alpn: &[String],
        ) -> Result<LeafCertDto, DomainError> {
            self.issues.fetch_add(1, Ordering::SeqCst);
            let ca_key = rcgen::KeyPair::generate().unwrap();
            let mut ca_params = rcgen::CertificateParams::default();
            ca_params
                .distinguished_name
                .push(rcgen::DnType::CommonName, "test CA");
            ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            let ca = ca_params.self_signed(&ca_key).unwrap();
            let issuer = rcgen::Issuer::from_ca_cert_pem(&ca.pem(), ca_key).unwrap();

            let leaf_key = rcgen::KeyPair::generate().unwrap();
            let mut params = rcgen::CertificateParams::default();
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, host);
            params.subject_alt_names = vec![rcgen::SanType::DnsName(host.try_into().unwrap())];
            let cert = params.signed_by(&leaf_key, &issuer).unwrap();
            Ok(LeafCertDto {
                cert_pem: cert.pem(),
                key_pem: leaf_key.serialize_pem(),
            })
        }

        async fn trust_status(&self) -> Result<TrustStatusDto, DomainError> {
            Ok(TrustStatusDto::default())
        }
        async fn install_trust(&self) -> Result<TrustStatusDto, DomainError> {
            Ok(TrustStatusDto::default())
        }
        async fn regenerate_ca(&self) -> Result<CaStatusDto, DomainError> {
            self.ensure_ca().await
        }
        fn ca_cert_path(&self) -> PathBuf {
            PathBuf::from("/tmp/test-ca.pem")
        }
    }

    #[tokio::test]
    async fn parse_leaf_builds_signing_certified_key() {
        let svc = RcgenLikeService {
            issues: AtomicU64::new(0),
        };
        let leaf = svc.issue_leaf("example.org", &[]).await.expect("leaf");
        let certified = parse_leaf(&leaf).expect("parse");
        assert_eq!(certified.cert.len(), 1);
        assert!(format!("{:?}", certified.key.algorithm()).contains("ECDSA"));
        // Смоук подписи: SigningKey::sign через rustls-механику уже покрыт
        // сборкой ServerConfig ниже — здесь достаточно валидного ключа.
        let issuer = Arc::new(CertIssuer::new(Arc::new(svc)));
        let config = issuer.server_config("example.org");
        assert_eq!(
            config.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()],
            "MITM предлагает h2 и http/1.1"
        );
    }

    #[tokio::test]
    async fn prepare_populates_cache_and_lookup_hits() {
        let svc = Arc::new(RcgenLikeService {
            issues: AtomicU64::new(0),
        });
        let issuer = Arc::new(CertIssuer::new(svc.clone()));
        issuer.prepare("cached.test").await.expect("prepare");
        assert!(issuer.lookup("cached.test").is_some());
        assert!(issuer.lookup("missing.test").is_none());
        issuer.prepare("cached.test").await.expect("prepare #2");
        assert_eq!(
            svc.issues.load(Ordering::SeqCst),
            1,
            "повторный prepare берёт лист из LRU"
        );
    }

    #[tokio::test]
    async fn regenerate_changes_fingerprint_and_invalidates() {
        // Fingerprint меняется → новый ключ LRU → перевыпуск листа.
        struct FlippingFp {
            issues: AtomicU64,
            flip: AtomicU64,
        }
        #[async_trait]
        impl CertificateService for FlippingFp {
            async fn ensure_ca(&self) -> Result<CaStatusDto, DomainError> {
                let n = self.flip.fetch_add(1, Ordering::SeqCst);
                Ok(CaStatusDto {
                    cert_path: PathBuf::from("/tmp/x.pem"),
                    fingerprint: Some(format!("fp-{n}")),
                    created_at: None,
                    exists: true,
                })
            }
            async fn issue_leaf(
                &self,
                host: &str,
                _a: &[String],
            ) -> Result<LeafCertDto, DomainError> {
                self.issues.fetch_add(1, Ordering::SeqCst);
                let key = rcgen::KeyPair::generate().unwrap();
                let mut p = rcgen::CertificateParams::default();
                p.distinguished_name.push(rcgen::DnType::CommonName, host);
                let cert = p.self_signed(&key).unwrap();
                Ok(LeafCertDto {
                    cert_pem: cert.pem(),
                    key_pem: key.serialize_pem(),
                })
            }
            async fn trust_status(&self) -> Result<TrustStatusDto, DomainError> {
                Ok(TrustStatusDto::default())
            }
            async fn install_trust(&self) -> Result<TrustStatusDto, DomainError> {
                Ok(TrustStatusDto::default())
            }
            async fn regenerate_ca(&self) -> Result<CaStatusDto, DomainError> {
                self.ensure_ca().await
            }
            fn ca_cert_path(&self) -> PathBuf {
                PathBuf::from("/tmp/x.pem")
            }
        }
        let svc = Arc::new(FlippingFp {
            issues: AtomicU64::new(0),
            flip: AtomicU64::new(0),
        });
        let issuer = Arc::new(CertIssuer::new(svc.clone()));
        issuer.prepare("flip.test").await.expect("p1");
        issuer.prepare("flip.test").await.expect("p2");
        assert_eq!(
            svc.issues.load(Ordering::SeqCst),
            2,
            "fingerprint изменился между вызовами → лист перевыпущен"
        );
        assert!(
            issuer.lookup("flip.test").is_some(),
            "последний выпуск в кэше"
        );
    }
}
