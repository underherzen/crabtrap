//! Лента релизов GitHub (`/releases/latest`) через системный curl — как
//! networksetup/security в соседних адаптерах: без HTTP-клиента в
//! infrastructure. curl не смотрит в системный прокси macOS, поэтому
//! проверка не зависит от того, запущен ли crabtrap.

use application::dto::updates::ReleaseInfoDto;
use application::services::ReleaseFeed;
use async_trait::async_trait;
use domain::errors::DomainError;
use serde::Deserialize;
use tokio::process::Command;

/// Сколько ждать GitHub (проверка фоновая, но не должна висеть).
const REQUEST_TIMEOUT_SECS: &str = "10";

pub struct GithubReleaseFeed {
    /// `owner/repo`.
    repo: String,
}

impl GithubReleaseFeed {
    pub fn new(repo: impl Into<String>) -> Self {
        Self { repo: repo.into() }
    }

    fn url(&self) -> String {
        format!("https://api.github.com/repos/{}/releases/latest", self.repo)
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

/// Ответ `/releases/latest` → DTO.
pub fn parse_release(json: &[u8]) -> Result<ReleaseInfoDto, DomainError> {
    let release: GithubRelease = serde_json::from_slice(json).map_err(|e| {
        DomainError::InfrastructureError(format!("unexpected GitHub response: {e}"))
    })?;
    Ok(ReleaseInfoDto {
        tag: release.tag_name,
        url: release.html_url,
    })
}

#[async_trait]
impl ReleaseFeed for GithubReleaseFeed {
    async fn latest(&self) -> Result<ReleaseInfoDto, DomainError> {
        let output = Command::new("/usr/bin/curl")
            .args(["--fail", "--silent", "--show-error", "--location"])
            .args(["--max-time", REQUEST_TIMEOUT_SECS])
            .args(["--header", "Accept: application/vnd.github+json"])
            .args([
                "--user-agent",
                concat!("crabtrap/", env!("CARGO_PKG_VERSION")),
            ])
            .arg(self.url())
            .output()
            .await
            .map_err(|e| DomainError::InfrastructureError(format!("run curl: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(DomainError::InfrastructureError(format!(
                "update check failed: {stderr}"
            )));
        }
        parse_release(&output.stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tag_and_page_url() {
        let json = br#"{"tag_name":"v0.2.0","html_url":"https://github.com/o/crabtrap/releases/tag/v0.2.0","draft":false,"assets":[]}"#;
        let release = parse_release(json).unwrap();
        assert_eq!(release.tag, "v0.2.0");
        assert_eq!(
            release.url,
            "https://github.com/o/crabtrap/releases/tag/v0.2.0"
        );
    }

    #[test]
    fn rejects_error_payload() {
        let json = br#"{"message":"Not Found","documentation_url":"https://docs.github.com"}"#;
        assert!(matches!(
            parse_release(json),
            Err(DomainError::InfrastructureError(_))
        ));
    }

    #[test]
    fn builds_latest_release_url() {
        assert_eq!(
            GithubReleaseFeed::new("o/crabtrap").url(),
            "https://api.github.com/repos/o/crabtrap/releases/latest"
        );
    }
}
