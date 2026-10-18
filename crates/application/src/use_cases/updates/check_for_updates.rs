//! Есть ли релиз новее запущенной версии.

use std::sync::Arc;

use domain::errors::DomainError;

use crate::dto::updates::UpdateStatus;
use crate::services::ReleaseFeed;

pub struct CheckForUpdatesUseCase {
    feed: Arc<dyn ReleaseFeed>,
    current_version: String,
}

impl CheckForUpdatesUseCase {
    pub fn new(feed: Arc<dyn ReleaseFeed>, current_version: impl Into<String>) -> Self {
        Self {
            feed,
            current_version: current_version.into(),
        }
    }

    pub async fn execute(&self) -> Result<UpdateStatus, DomainError> {
        let release = self.feed.latest().await?;
        Ok(if is_newer(&self.current_version, &release.tag) {
            UpdateStatus::Available(release)
        } else {
            UpdateStatus::UpToDate
        })
    }
}

/// `candidate` строго новее `current` (оба — `X.Y.Z`, допускается
/// префикс `v`). Нераспознанная версия кандидата — не обновление.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (parse_version(current), parse_version(candidate)) {
        (Some(current), Some(candidate)) => candidate > current,
        (None, Some(_)) => true,
        (_, None) => false,
    }
}

/// `v1.2.3` / `1.2.3` → (1, 2, 3); pre-release и прочее — None.
fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let mut parts = raw.strip_prefix('v').unwrap_or(raw).split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}
