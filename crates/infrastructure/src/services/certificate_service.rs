//! Реализация [`CertificateService`]: rcgen CA + листья для MITM (Phase 3).
//!
//! Весь rcgen-криптограф живёт ЗДЕСЬ (решение Phase 3): движок получает
//! PEM-DTO через порт и сам превращает их в rustls `CertifiedKey`. CA
//! персистентен в `certs_dir` (`ca.pem` + `ca.key.pem`, каталог 0700, ключ
//! 0600) и переиспользуется между запусками; битые/несогласованные файлы
//! переименовываются в `*.corrupt-<ts>` и CA генерируется заново.
//!
//! Доверие на macOS: `security add-trusted-cert` через `osascript ...
//! with administrator privileges` (GUI-промпт пароля администратора).

use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use domain::entities::certificate::CaInfo;
use domain::errors::DomainError;
use domain::repositories::CertificateRepository as _;
use lru::LruCache;
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, SanType};
use time::OffsetDateTime;

use application::dto::certificates::{CaStatusDto, LeafCertDto, TrustStatusDto};
use application::services::CertificateService;

use rcgen::string::Ia5String;

use crate::repositories::FileCertificateRepository;

/// Емкость LRU выпущенных листьев (PEM-кэш, чтобы не подписывать заново).
const LEAF_CACHE_CAPACITY: usize = 512;
/// Валидность CA.
const CA_VALIDITY_DAYS: i64 = 3650; // ~10 лет
/// Валидность листа (короткоживущие, перевыпускаются LRU-промахом).
const LEAF_VALIDITY_DAYS: i64 = 730; // ~2 года
/// Запас назад от «сейчас» на рассинхрон часов.
const BACKDATING: i64 = 1; // сутки

/// PEM + DER загруженного/сгенерированного CA (in-memory кэш сервиса).
struct CaMaterial {
    cert_pem: String,
    key_pem: String,
    /// SHA-256 от DER-сертификата (hex).
    fingerprint: String,
    created_at: DateTime<Utc>,
}

/// rcgen-реализация порта сертификатов.
pub struct RcgenCertificateService {
    certs_dir: PathBuf,
    meta: FileCertificateRepository,
    /// Кэш CA; None — ещё не загружен (лениво в `ensure_ca`).
    ca: Mutex<Option<CaMaterial>>,
    /// Сериализует загрузку/генерацию/регенерацию CA: при первом запуске
    /// ensure_ca зовут одновременно движок, main и экран сертификатов —
    /// без лока получалось два CA (память ≠ диск).
    init_lock: tokio::sync::Mutex<()>,
    /// LRU листьев: (host, ca_fingerprint) → PEM-DTO. Ключ с fingerprint
    /// автоматически инвалидируется после regenerate_ca.
    leaf_cache: Mutex<LruCache<(String, String), LeafCertDto>>,
    /// Счётчик подписаний листьев (для тестов кэша).
    leaf_signs: AtomicU64,
}

impl RcgenCertificateService {
    /// Сервис с инъектируемым каталогом сертификатов (тесты — tempdir).
    pub fn new(certs_dir: PathBuf) -> Self {
        let meta = FileCertificateRepository::new(certs_dir.clone());
        Self {
            certs_dir,
            meta,
            ca: Mutex::new(None),
            init_lock: tokio::sync::Mutex::new(()),
            leaf_cache: Mutex::new(LruCache::new(
                NonZeroUsize::new(LEAF_CACHE_CAPACITY).expect("nonzero"),
            )),
            leaf_signs: AtomicU64::new(0),
        }
    }

    /// Каталог сертификатов из конфига (`storage.certs_dir`).
    pub fn from_config(config: &config::AppConfig) -> Self {
        Self::new(config.storage.certs_dir.clone())
    }

    /// Стандартное расположение macOS: `~/Library/Application Support/crabtrap/certs`
    /// (для композиций без крейта config — например, crabtrapd).
    pub fn default_location() -> Result<Self, DomainError> {
        let dir = directories::ProjectDirs::from("", "", "crabtrap")
            .map(|d| d.config_dir().join("certs"))
            .ok_or_else(|| DomainError::InfrastructureError("app dirs unavailable".into()))?;
        Ok(Self::new(dir))
    }

    fn cert_path(&self) -> PathBuf {
        self.certs_dir.join("ca.pem")
    }

    fn key_path(&self) -> PathBuf {
        self.certs_dir.join("ca.key.pem")
    }

    /// Сколько листьев реально подписано (без кэша) — для тестов.
    pub fn issued_leaf_count(&self) -> u64 {
        self.leaf_signs.load(Ordering::Relaxed)
    }
}

// ==== CA: генерация, загрузка, деградация ====

/// Префикс имени CA (CN).
const CA_NAME_PREFIX: &str = "crabtrap CA";

/// CN нового CA: префикс + минута создания (UTC). Выпуски различимы на
/// устройствах: «Доверие сертификатам» iOS показывает только имя.
fn ca_common_name(created: DateTime<Utc>) -> String {
    format!("{CA_NAME_PREFIX} {}", created.format("%Y-%m-%d %H:%M"))
}

/// CN сертификата CA (старые выпуски — просто «crabtrap CA»).
fn ca_subject(cert_pem: &str) -> String {
    let common_name = |der: &[u8]| -> Option<String> {
        let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
        let name = cert.subject().iter_common_name().next()?.as_str().ok()?;
        Some(name.to_string())
    };
    pem_certificate_der(cert_pem)
        .ok()
        .and_then(|der| common_name(&der))
        .unwrap_or_else(|| CA_NAME_PREFIX.to_string())
}

impl RcgenCertificateService {
    /// Параметры корневого CA (CN, BasicConstraints, KeyUsage, ~10 лет).
    fn ca_params() -> CertificateParams {
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, ca_common_name(Utc::now()));
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        let now = OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(BACKDATING);
        params.not_after = now + time::Duration::days(CA_VALIDITY_DAYS);
        params
    }

    /// Сгенерировать новый CA (ECDSA P-256, ключ `KeyPair::generate`
    /// по умолчанию) и записать на диск с правами 0700/0600.
    fn generate_ca(&self) -> Result<CaMaterial, DomainError> {
        let (cert_pem, key_pem) = Self::build_ca()?;
        self.persist_new_ca(cert_pem, key_pem)
    }

    /// Ключ + самоподписанный сертификат в памяти (без IO).
    fn build_ca() -> Result<(String, String), DomainError> {
        let key = KeyPair::generate()
            .map_err(|e| DomainError::InfrastructureError(format!("CA keygen: {e}")))?;
        let cert = Self::ca_params()
            .self_signed(&key)
            .map_err(|e| DomainError::InfrastructureError(format!("CA self-signed: {e}")))?;
        Ok((cert.pem(), key.serialize_pem()))
    }

    /// Записать пару PEM на диск (0700/0600) и собрать материал.
    fn persist_new_ca(&self, cert_pem: String, key_pem: String) -> Result<CaMaterial, DomainError> {
        std::fs::create_dir_all(&self.certs_dir)
            .map_err(|e| DomainError::RepositoryError(format!("mkdir certs: {e}")))?;
        restrict_dir_permissions(&self.certs_dir);
        write_secret(&self.key_path(), key_pem.as_bytes())?;
        write_secret(&self.cert_path(), cert_pem.as_bytes())?;

        let der = pem_certificate_der(&cert_pem)?;
        // created_at — mtime файла (единая семантика с load-путём).
        let created_at = std::fs::metadata(self.cert_path())
            .and_then(|m| m.modified())
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now());
        let material = CaMaterial {
            cert_pem,
            key_pem,
            fingerprint: fingerprint_hex(&der),
            created_at,
        };
        Ok(material)
    }

    /// CaInfo-проекция материала (для ca-meta.json).
    fn ca_info(&self, material: &CaMaterial) -> CaInfo {
        CaInfo {
            subject: ca_subject(&material.cert_pem),
            fingerprint_sha256: material.fingerprint.clone(),
            not_before: material.created_at - chrono::Duration::days(BACKDATING),
            not_after: material.created_at + chrono::Duration::days(CA_VALIDITY_DAYS),
            cert_path: self.cert_path(),
        }
    }

    /// Загрузить CA с диска; `Ok(None)` — файлов нет. Ошибка парсинга или
    /// несогласованность cert/key → вызывающий решает (corrupt-политика).
    fn load_ca_from_disk(&self) -> Result<Option<CaMaterial>, CaLoadError> {
        let cert_pem = match std::fs::read_to_string(self.cert_path()) {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Err(CaLoadError::Corrupt(format!("ca.pem is not UTF-8: {e}")));
            }
            Err(e) => return Err(CaLoadError::Io(format!("read ca.pem: {e}"))),
        };
        let key_pem = match std::fs::read_to_string(self.key_path()) {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(CaLoadError::Corrupt("ca.key.pem missing".into()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Err(CaLoadError::Corrupt(format!(
                    "ca.key.pem is not UTF-8: {e}"
                )));
            }
            Err(e) => return Err(CaLoadError::Io(format!("read ca.key.pem: {e}"))),
        };
        self.parse_ca(cert_pem, key_pem)
            .map(Some)
            .map_err(|e| CaLoadError::Corrupt(e.to_string()))
    }

    /// Разбор и проверка согласованности пары PEM (ошибка = файлы битые).
    fn parse_ca(&self, cert_pem: String, key_pem: String) -> Result<CaMaterial, DomainError> {
        // Валидация: ключ парсится, сертификат парсится, ключом МОЖНО
        // подписать лист (ловит рассинхрон ca.pem/ca.key.pem после ручных
        // правок файлов).
        let der = pem_certificate_der(&cert_pem)?;
        let key = KeyPair::from_pem(&key_pem)
            .map_err(|e| DomainError::RepositoryError(format!("bad ca.key.pem: {e}")))?;
        let issuer = Issuer::from_ca_cert_pem(&cert_pem, key).map_err(|e| {
            DomainError::RepositoryError(format!("ca.pem is not a usable issuer: {e}"))
        })?;
        self.sanity_sign(&issuer)?;

        let created_at = std::fs::metadata(self.cert_path())
            .and_then(|m| m.modified())
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now());
        Ok(CaMaterial {
            cert_pem,
            key_pem,
            fingerprint: fingerprint_hex(&der),
            created_at,
        })
    }

    /// Пробное подписание throwaway-имени — проверяет согласованность
    /// сертификата и ключа CA (подпись проверяется клиентами листьев).
    fn sanity_sign(&self, issuer: &Issuer<'_, KeyPair>) -> Result<(), DomainError> {
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, "crabtrap-ca-sanity-check.invalid");
        let leaf_key = KeyPair::generate()
            .map_err(|e| DomainError::InfrastructureError(format!("sanity keygen: {e}")))?;
        params
            .signed_by(&leaf_key, issuer)
            .map_err(|e| DomainError::RepositoryError(format!("CA sanity sign failed: {e}")))?;
        Ok(())
    }

    /// Переименовать текущие файлы CA в `*.corrupt-<ts>` (или `*.old-<ts>`
    /// при плановой регенерации).
    /// Отложить ca.pem/ca.key.pem в `<name>.<suffix>-<ts>`; возвращает
    /// выполненные переименования (from, to) — для отката.
    fn rename_ca_files(&self, suffix: &str) -> Vec<(PathBuf, PathBuf)> {
        let ts = Utc::now().format("%Y%m%dT%H%M%S");
        let mut moved = Vec::new();
        for path in [self.cert_path(), self.key_path()] {
            if path.exists() {
                let aside = path.with_file_name(format!(
                    "{}.{suffix}-{ts}",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("ca")
                ));
                match std::fs::rename(&path, &aside) {
                    Ok(()) => moved.push((path, aside)),
                    Err(e) => {
                        tracing::warn!(from = %path.display(), error = %e, "cannot rename CA file aside")
                    }
                }
            }
        }
        moved
    }

    /// Вернуть отложенные файлы на место (откат неудачного regenerate).
    fn restore_ca_files(moved: &[(PathBuf, PathBuf)]) {
        for (original, aside) in moved {
            if let Err(e) = std::fs::rename(aside, original) {
                tracing::error!(from = %aside.display(), error = %e, "cannot restore CA file");
            }
        }
    }

    /// Статус уже загруженного в память CA.
    fn cached_status(&self) -> Option<CaStatusDto> {
        self.ca
            .lock()
            .expect("ca lock")
            .as_ref()
            .map(|material| Self::status_of(material, self.cert_path()))
    }

    fn status_of(material: &CaMaterial, cert_path: PathBuf) -> CaStatusDto {
        CaStatusDto {
            cert_path,
            fingerprint: Some(material.fingerprint.clone()),
            created_at: Some(material.created_at),
            exists: true,
        }
    }
}

// ==== файловые помощники ====

/// Первый сертификат из PEM-блоба → DER.
fn pem_certificate_der(pem: &str) -> Result<Vec<u8>, DomainError> {
    CertificateDer::pem_slice_iter(pem.as_bytes())
        .next()
        .transpose()
        .map_err(|e| DomainError::RepositoryError(format!("bad PEM: {e}")))?
        .map(|cert| cert.as_ref().to_vec())
        .ok_or_else(|| DomainError::RepositoryError("no certificate in PEM".into()))
}

/// SHA-256 fingerprint (hex) от DER.
fn fingerprint_hex(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(der);
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Почему CA не загрузился: IO-сбой (не трогаем файлы) или порча.
#[derive(Debug)]
enum CaLoadError {
    Io(String),
    Corrupt(String),
}

/// Запись секрета: tmp + rename. tmp создаётся сразу с 0600 (unix) —
/// без окна, когда ключ читаем всем по umask.
fn write_secret(path: &Path, bytes: &[u8]) -> Result<(), DomainError> {
    use std::io::Write as _;
    let tmp = path.with_extension("tmp");
    let write_err =
        |e: std::io::Error| DomainError::RepositoryError(format!("write {}: {e}", path.display()));
    // Остаток прошлого падения мог быть создан с другими правами.
    let _ = std::fs::remove_file(&tmp);
    let mut file = secret_open_options().open(&tmp).map_err(write_err)?;
    file.write_all(bytes).map_err(write_err)?;
    file.sync_all().map_err(write_err)?;
    drop(file);
    std::fs::rename(&tmp, path)
        .map_err(|e| DomainError::RepositoryError(format!("rename {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(unix)]
fn secret_open_options() -> std::fs::OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    options
}

#[cfg(not(unix))]
fn secret_open_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    options
}

#[cfg(unix)]
fn restrict_dir_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_dir_permissions(_path: &Path) {}

/// Нормализация хоста: нижний регистр, без хвостовой точки, IDN → punycode.
/// Punycode — через `url` (разбор `https://{host}/` отдаёт ASCII-хост).
pub(crate) fn normalize_host(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if trimmed.parse::<std::net::IpAddr>().is_ok() {
        return trimmed; // IP не конвертируется
    }
    match url::Url::parse(&format!("https://{trimmed}/")) {
        Ok(parsed) => parsed
            .host_str()
            .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
            .unwrap_or(trimmed),
        Err(_) => trimmed, // непарсимый хост — прокси уже валидировал его иначе
    }
}

/// SAN для листа: IP-литерал → IpAddress-SAN, иначе — точное DNS-имя.
/// Wildcard не добавляется: лист выпускается на каждый хост (LRU по точному
/// имени), а `*.co.uk` для `bbc.co.uk` был бы wildcard'ом публичного суффикса.
pub(crate) fn sans_for_host(host: &str) -> Vec<SanType> {
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![SanType::IpAddress(ip)],
        Err(_) => vec![SanType::DnsName(ia5(host))],
    }
}

/// `Ia5String` из уже-ASCII строки (IDN уже в punycode — см. normalize_host).
fn ia5(s: &str) -> Ia5String {
    s.try_into()
        .unwrap_or_else(|_| "invalid.ia5.invalid".try_into().expect("static ascii"))
}

// ==== порт CertificateService ====

#[async_trait]
impl CertificateService for RcgenCertificateService {
    async fn ensure_ca(&self) -> Result<CaStatusDto, DomainError> {
        // Быстрый путь: уже в памяти.
        if let Some(status) = self.cached_status() {
            return Ok(status);
        }
        let _init = self.init_lock.lock().await;
        // Пока ждали лок, CA мог загрузить/создать соседний вызов.
        if let Some(status) = self.cached_status() {
            return Ok(status);
        }
        // Загрузка с диска; битое — в сторону и регенерация. Ошибка IO
        // (нет прав, диск) — наверх: перевыпуск CA сломал бы доверие.
        let loaded = match self.load_ca_from_disk() {
            Ok(Some(material)) => Some(material),
            Ok(None) => None,
            Err(CaLoadError::Io(e)) => {
                tracing::warn!(error = %e, "CA files unreadable");
                return Err(DomainError::InfrastructureError(e));
            }
            Err(CaLoadError::Corrupt(e)) => {
                tracing::warn!(error = %e, "CA corrupt/inconsistent, regenerating");
                let _ = self.rename_ca_files("corrupt");
                None
            }
        };
        let material = match loaded {
            Some(m) => m,
            None => {
                let m = self.generate_ca()?;
                tracing::info!(
                    fingerprint = %m.fingerprint,
                    path = %self.cert_path().display(),
                    "crabtrap CA generated"
                );
                m
            }
        };
        // Метаданные (ca-meta.json) — best-effort.
        let info = self.ca_info(&material);
        if let Err(e) = self.meta.persist_ca(&info).await {
            tracing::warn!(error = %e, "failed to persist ca-meta.json");
        }
        let status = Self::status_of(&material, self.cert_path());
        *self.ca.lock().expect("ca lock") = Some(material);
        Ok(status)
    }

    async fn issue_leaf(&self, host: &str, _alpn: &[String]) -> Result<LeafCertDto, DomainError> {
        let host = normalize_host(host);
        // CA должен существовать (движок зовёт ensure_ca до issue_leaf, но
        // порт не запрещает прямой вызов).
        let (ca_pem, ca_key_pem, ca_fp) = {
            // Guard не переживает await: клонируем материал сразу.
            let cached = self
                .ca
                .lock()
                .expect("ca lock")
                .as_ref()
                .map(|m| (m.cert_pem.clone(), m.key_pem.clone(), m.fingerprint.clone()));
            match cached {
                Some(t) => t,
                None => {
                    self.ensure_ca().await?;
                    let guard = self.ca.lock().expect("ca lock");
                    let m = guard.as_ref().expect("CA just ensured");
                    (m.cert_pem.clone(), m.key_pem.clone(), m.fingerprint.clone())
                }
            }
        };
        let cache_key = (host.clone(), ca_fp);
        if let Some(hit) = self.leaf_cache.lock().expect("leaf lock").get(&cache_key) {
            return Ok(hit.clone());
        }

        // Подписание (CPU) — в blocking-потоке; rcgen-типы не переживают
        // границу await, поэтому всё подписание в одной closure.
        let sans = sans_for_host(&host);
        let sign = tokio::task::spawn_blocking(move || -> Result<LeafCertDto, DomainError> {
            let ca_key = KeyPair::from_pem(&ca_key_pem)
                .map_err(|e| DomainError::InfrastructureError(format!("CA key parse: {e}")))?;
            let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key)
                .map_err(|e| DomainError::InfrastructureError(format!("issuer parse: {e}")))?;
            let leaf_key = KeyPair::generate()
                .map_err(|e| DomainError::InfrastructureError(format!("leaf keygen: {e}")))?;

            let mut params = CertificateParams::default();
            params
                .distinguished_name
                .push(DnType::CommonName, host.as_str());
            params.subject_alt_names = sans;
            params.extended_key_usages = vec![
                rcgen::ExtendedKeyUsagePurpose::ServerAuth,
                rcgen::ExtendedKeyUsagePurpose::ClientAuth,
            ];
            params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
            let now = OffsetDateTime::now_utc();
            params.not_before = now - time::Duration::days(BACKDATING);
            params.not_after = now + time::Duration::days(LEAF_VALIDITY_DAYS);

            let cert = params
                .signed_by(&leaf_key, &issuer)
                .map_err(|e| DomainError::InfrastructureError(format!("leaf sign: {e}")))?;
            Ok(LeafCertDto {
                cert_pem: cert.pem(),
                key_pem: leaf_key.serialize_pem(),
            })
        })
        .await
        .map_err(|e| DomainError::InfrastructureError(format!("leaf task: {e}")))??;
        self.leaf_signs.fetch_add(1, Ordering::Relaxed);
        self.leaf_cache
            .lock()
            .expect("leaf lock")
            .put(cache_key, sign.clone());
        Ok(sign)
    }

    async fn trust_status(&self) -> Result<TrustStatusDto, DomainError> {
        // ensure_ca: даже несозданный CA должен быть создан до проверки.
        self.ensure_ca().await?;
        let path = self.cert_path();
        let output = tokio::process::Command::new("/usr/bin/security")
            .args(["verify-cert", "-p", "ssl", "-c"])
            .arg(&path)
            .output()
            .await
            .map_err(|e| DomainError::InfrastructureError(format!("security verify-cert: {e}")))?;
        let details = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let trusted = output.status.success();
        // Какой домен держит trust: user-домен дампится без прав, System
        // считаем в остальных случаях (admin-путь install_trust).
        let keychain = if trusted {
            let user_dump = tokio::process::Command::new("/usr/bin/security")
                .args(["dump-trust-settings"])
                .output()
                .await
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            if user_dump.contains("crabtrap") {
                "login (user)".to_string()
            } else {
                "System".to_string()
            }
        } else {
            String::new()
        };
        Ok(TrustStatusDto {
            trusted,
            keychain: trusted.then_some(keychain).filter(|s| !s.is_empty()),
            details: Some(if trusted {
                if details.is_empty() {
                    "verified for ssl policy".to_string()
                } else {
                    details
                }
            } else {
                stderr
            }),
        })
    }

    async fn install_trust(&self) -> Result<TrustStatusDto, DomainError> {
        self.ensure_ca().await?;
        let argv = build_install_trust_command(&self.cert_path());
        let script = osascript_line(&argv);
        if std::env::var("CRABTRAP_TRUST_DRY_RUN").ok().as_deref() == Some("1") {
            tracing::info!(%script, "trust install dry-run");
            return Ok(TrustStatusDto {
                trusted: false,
                keychain: None,
                details: Some(format!("[dry-run] would run: {script}")),
            });
        }

        // 1) Admin-домен (System keychain, GUI-промпт пароля). Отмена
        //    пользователем — НЕ фатально: ниже проверяем фактическое
        //    доверие и при нужде фолбэчимся на user-домен.
        //    (Наблюдение macOS 14: сертификат в keychain попадает, а
        //    trust-политика не записывается — verify-cert единственная
        //    честная проверка.)
        let admin_err: Option<String> = match tokio::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .await
        {
            Ok(out) if out.status.success() => None,
            Ok(out) => Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            Err(e) => Some(e.to_string()),
        };

        // 2) Фактическое доверие?
        let mut status = self.trust_status().await?;
        if status.trusted {
            return Ok(status);
        }

        // 3) Фолбэк: user-домен (`add-trusted-cert` без `-d`) — не требует
        //    админа; macOS может показать GUI-подтверждение. Для Safari/
        //    Chrome пользовательского домена достаточно.
        match tokio::process::Command::new("/usr/bin/security")
            .args(["add-trusted-cert", "-r", "trustRoot", "-p", "ssl"])
            .arg(self.cert_path())
            .output()
            .await
        {
            Ok(out) if out.status.success() => {
                status = self.trust_status().await?;
                if !status.trusted {
                    status.details = Some(format!(
                        "still untrusted after user-domain fallback; admin step: {:?}; verify: {}",
                        admin_err,
                        status.details.clone().unwrap_or_default()
                    ));
                }
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                status.details = Some(format!(
                    "admin step: {:?}; user-domain fallback failed: {stderr}",
                    admin_err
                ));
            }
            Err(e) => {
                status.details = Some(format!(
                    "admin step: {:?}; user-domain run error: {e}",
                    admin_err
                ));
            }
        }
        Ok(status)
    }

    async fn regenerate_ca(&self) -> Result<CaStatusDto, DomainError> {
        let _init = self.init_lock.lock().await;
        // Старый CA (память и диск) остаётся рабочим, пока новый не записан:
        // сбой посреди regenerate не должен молча перевыпустить CA позже.
        let (cert_pem, key_pem) = Self::build_ca()?;
        let moved = self.rename_ca_files("old");
        let material = match self.persist_new_ca(cert_pem, key_pem) {
            Ok(material) => material,
            Err(e) => {
                Self::restore_ca_files(&moved);
                return Err(e);
            }
        };
        self.leaf_cache.lock().expect("leaf lock").clear();
        tracing::info!(fingerprint = %material.fingerprint, "crabtrap CA regenerated");
        let info = self.ca_info(&material);
        if let Err(e) = self.meta.persist_ca(&info).await {
            tracing::warn!(error = %e, "failed to persist ca-meta.json");
        }
        let status = Self::status_of(&material, self.cert_path());
        *self.ca.lock().expect("ca lock") = Some(material);
        Ok(status)
    }

    fn ca_cert_path(&self) -> PathBuf {
        self.cert_path()
    }
}

// ==== команда доверия (конструирование тестируется юнит-тестами) ====

/// `security add-trusted-cert` в System keychain от имени root домена
/// доверия. Выбор `-p ssl`: доверие ограничено SSL-политикой (наш случай —
/// TLS-перехват), не даём сертификату веса для прочих политик (кодовой
/// подписи и т.п.); `security verify-cert -p ssl` это подтверждает.
pub(crate) fn build_install_trust_command(cert_path: &Path) -> Vec<String> {
    [
        "/usr/bin/security",
        "add-trusted-cert",
        "-d", // admin domain (System keychain)
        "-r",
        "trustRoot", // доверять как корневому
        "-p",
        "ssl", // только SSL-политика
        "-k",
        "/Library/Keychains/System.keychain",
    ]
    .into_iter()
    .map(str::to_string)
    .chain([cert_path.to_string_lossy().into_owned()])
    .collect()
}

/// Однострочник AppleScript: запуск команды с правами администратора
/// (GUI-промпт пароля). Экранируются `\` и `"` для AppleScript-строки;
/// аргументы shell-команды квотятся по sh-правилам.
pub(crate) fn osascript_line(argv: &[String]) -> String {
    let shell = argv
        .iter()
        .map(|a| sh_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    let escaped = shell.replace('\\', "\\\\").replace('"', "\\\"");
    format!("do shell script \"{escaped}\" with administrator privileges")
}

fn sh_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/:_-.".contains(&b));
    if safe {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests;
