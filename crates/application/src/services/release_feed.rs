//! Порт ленты релизов (реализация — infrastructure, GitHub API через curl).

use domain::errors::DomainError;

use crate::dto::updates::ReleaseInfoDto;

#[async_trait::async_trait]
pub trait ReleaseFeed: Send + Sync {
    /// Последний опубликованный (не draft, не pre-release) релиз.
    async fn latest(&self) -> Result<ReleaseInfoDto, DomainError>;
}
