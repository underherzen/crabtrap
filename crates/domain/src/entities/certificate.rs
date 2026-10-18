//! Сущности сертификатов (CA и клиентские).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Метаданные корневого CA (путь — сами ключи никогда не покидают диск).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaInfo {
    pub subject: String,
    pub fingerprint_sha256: String,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    pub cert_path: PathBuf,
}

/// Клиентский сертификат (per-host mTLS). Пароль pkcs12 — в macOS Keychain,
/// здесь только путь.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientCertificate {
    pub id: String,
    pub name: String,
    pub pkcs12_path: PathBuf,
    /// None — сертификат по умолчанию для всех хостов.
    #[serde(default)]
    pub for_host: Option<String>,
}
