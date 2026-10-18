//! DTO сертификатов (Phase 3).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Статус корневого CA.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CaStatusDto {
    pub cert_path: PathBuf,
    /// SHA-256 fingerprint (hex).
    pub fingerprint: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    /// Существует ли файл CA на диске.
    pub exists: bool,
}

/// Статус доверия системе (keychain).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TrustStatusDto {
    pub trusted: bool,
    /// В какой keychain установлен (например, `login`/`System`).
    pub keychain: Option<String>,
    pub details: Option<String>,
}

/// Выпущенный лист сертификата (PEM). Инфраструктура адаптирует в rustls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeafCertDto {
    pub cert_pem: String,
    pub key_pem: String,
}
