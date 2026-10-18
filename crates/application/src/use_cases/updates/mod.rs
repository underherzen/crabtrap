//! Проверка обновлений: сравнение версии приложения с последним релизом.

mod check_for_updates;

pub use check_for_updates::{CheckForUpdatesUseCase, is_newer};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use domain::errors::DomainError;
    use futures::executor::block_on;

    use super::*;
    use crate::dto::updates::{ReleaseInfoDto, UpdateStatus};
    use crate::services::ReleaseFeed;

    /// None — лента недоступна.
    struct StubFeed(Option<ReleaseInfoDto>);

    #[async_trait]
    impl ReleaseFeed for StubFeed {
        async fn latest(&self) -> Result<ReleaseInfoDto, DomainError> {
            self.0
                .clone()
                .ok_or_else(|| DomainError::InfrastructureError("offline".into()))
        }
    }

    fn release(tag: &str) -> ReleaseInfoDto {
        ReleaseInfoDto {
            tag: tag.into(),
            url: format!("https://github.com/o/r/releases/tag/{tag}"),
        }
    }

    fn check(current: &str, feed: Option<ReleaseInfoDto>) -> Result<UpdateStatus, DomainError> {
        block_on(CheckForUpdatesUseCase::new(Arc::new(StubFeed(feed)), current).execute())
    }

    #[test]
    fn newer_release_is_available() {
        assert_eq!(
            check("0.1.0", Some(release("v0.2.0"))).unwrap(),
            UpdateStatus::Available(release("v0.2.0"))
        );
    }

    #[test]
    fn same_or_older_release_is_up_to_date() {
        assert_eq!(
            check("0.2.0", Some(release("v0.2.0"))).unwrap(),
            UpdateStatus::UpToDate
        );
        assert_eq!(
            check("0.3.0", Some(release("v0.2.9"))).unwrap(),
            UpdateStatus::UpToDate
        );
    }

    #[test]
    fn feed_error_is_propagated() {
        assert!(matches!(
            check("0.1.0", None),
            Err(DomainError::InfrastructureError(_))
        ));
    }

    #[test]
    fn is_newer_compares_numerically() {
        assert!(is_newer("0.9.0", "0.10.0"));
        assert!(is_newer("1.2.3", "v1.2.4"));
        assert!(is_newer("1.9.9", "2.0.0"));
        assert!(!is_newer("1.2.3", "1.2.3"));
        assert!(!is_newer("1.2.3", "v1.2.2"));
    }

    #[test]
    fn is_newer_ignores_unparsable_candidate() {
        assert!(!is_newer("0.1.0", "nightly"));
        assert!(!is_newer("0.1.0", "v1.2"));
        assert!(!is_newer("0.1.0", "v1.2.3-beta.1"));
    }
}
