//! Файловая реализация `CertificateRepository`: метаданные CA рядом с
//! сертификатами (`certs/ca-meta.json`), атомарно (tmp + rename).

use std::path::PathBuf;

use async_trait::async_trait;
use domain::entities::certificate::CaInfo;
use domain::errors::DomainError;
use domain::repositories::CertificateRepository;

pub struct FileCertificateRepository {
    meta_path: PathBuf,
}

impl FileCertificateRepository {
    /// Репозиторий метаданных CA в каталоге сертификатов.
    pub fn new(certs_dir: PathBuf) -> Self {
        Self {
            meta_path: certs_dir.join("ca-meta.json"),
        }
    }

    pub fn from_config(config: &config::AppConfig) -> Self {
        Self::new(config.storage.certs_dir.clone())
    }
}

#[async_trait]
impl CertificateRepository for FileCertificateRepository {
    async fn load_ca(&self) -> Result<Option<CaInfo>, DomainError> {
        let path = self.meta_path.clone();
        let raw = tokio::task::spawn_blocking(move || std::fs::read_to_string(path))
            .await
            .map_err(|e| DomainError::RepositoryError(e.to_string()))?
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    DomainError::NotFound("ca-meta.json".to_string())
                } else {
                    DomainError::RepositoryError(e.to_string())
                }
            })?;
        let info = serde_json::from_str(&raw)
            .map_err(|e| DomainError::RepositoryError(format!("corrupt ca-meta.json: {e}")))?;
        Ok(Some(info))
    }

    async fn persist_ca(&self, info: &CaInfo) -> Result<(), DomainError> {
        let path = self.meta_path.clone();
        let raw = serde_json::to_string_pretty(info)
            .map_err(|e| DomainError::RepositoryError(e.to_string()))?;
        let res = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, raw)?;
            std::fs::rename(&tmp, &path)
        })
        .await
        .map_err(|e| DomainError::RepositoryError(e.to_string()))?;
        res.map_err(|e| DomainError::RepositoryError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn ca_info_roundtrips_through_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = FileCertificateRepository::new(dir.path().to_path_buf());
        assert!(repo.load_ca().await.is_err()); // ещё нет файла

        let info = CaInfo {
            subject: "crabtrap CA".into(),
            fingerprint_sha256: "ab12".repeat(32),
            not_before: Utc::now() - chrono::Duration::days(1),
            not_after: Utc::now() + chrono::Duration::days(3650),
            cert_path: dir.path().join("ca.pem"),
        };
        repo.persist_ca(&info).await.expect("persist");
        let loaded = repo.load_ca().await.expect("load");
        assert_eq!(loaded, Some(info));
    }
}
