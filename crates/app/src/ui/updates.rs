//! Check for Updates: последний релиз GitHub против запущенной версии.
//! Ручная проверка (меню) сообщает и «актуально», и ошибку; фоновая при
//! старте — только о найденном обновлении.

use std::sync::Arc;

use application::dto::updates::UpdateStatus;
use application::use_cases::updates::CheckForUpdatesUseCase;
use domain::errors::DomainError;
use gpui_kit::component::Root;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, SharedString};

use crate::app_state::AppState;

/// Кто запросил проверку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Startup,
    Manual,
}

/// `owner/repo` из URL репозитория (`https://github.com/owner/repo`).
pub fn repo_slug(repository_url: &str) -> Option<&str> {
    let slug = repository_url
        .strip_prefix("https://github.com/")?
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let (owner, repo) = slug.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/')).then_some(slug)
}

/// Что показать пользователю (None — молчать).
fn notice(result: &Result<UpdateStatus, DomainError>, trigger: Trigger) -> Option<Notice> {
    match (result, trigger) {
        (Ok(UpdateStatus::Available(release)), _) => Some(Notice {
            title: format!("crabtrap {} is available", release.tag),
            message: format!(
                "You have v{}. Click to open the release page.",
                env!("CARGO_PKG_VERSION")
            ),
            url: Some(release.url.clone()),
        }),
        (Ok(UpdateStatus::UpToDate), Trigger::Manual) => Some(Notice {
            title: "crabtrap is up to date".into(),
            message: format!("v{} is the latest version.", env!("CARGO_PKG_VERSION")),
            url: None,
        }),
        (Err(error), Trigger::Manual) => Some(Notice {
            title: "Update check failed".into(),
            message: error.to_string(),
            url: None,
        }),
        (_, Trigger::Startup) => None,
    }
}

#[derive(Debug, PartialEq)]
struct Notice {
    title: String,
    message: String,
    url: Option<String>,
}

/// Проверить на движковом рантайме и показать итог в активном окне.
pub fn check(state: &AppState, runtime: &tokio::runtime::Handle, trigger: Trigger, cx: &mut App) {
    let use_case =
        CheckForUpdatesUseCase::new(Arc::clone(&state.release_feed), env!("CARGO_PKG_VERSION"));
    let (tx, rx) = async_channel::bounded(1);
    runtime.spawn(async move {
        let _ = tx.send(use_case.execute().await).await;
    });
    cx.spawn(async move |cx| {
        let Ok(result) = rx.recv().await else {
            return;
        };
        if let Err(error) = &result {
            tracing::warn!(%error, "update check failed");
        }
        if let Some(notice) = notice(&result, trigger) {
            cx.update(|cx| show(notice, cx));
        }
    })
    .detach();
}

fn show(notice: Notice, cx: &mut App) {
    let Some(handle) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
        return;
    };
    let mut notification = Notification::new()
        .title(SharedString::from(notice.title))
        .message(notice.message);
    if let Some(url) = notice.url {
        notification = notification
            .autohide(false)
            .on_click(move |_, _, cx| cx.open_url(&url));
    }
    let _ = handle.update(cx, |_, window, cx| {
        Root::update(window, cx, |root, window, cx| {
            root.push_notification(notification, window, cx);
        });
    });
}

#[cfg(test)]
mod tests {
    use application::dto::updates::ReleaseInfoDto;

    use super::*;

    #[test]
    fn repo_slug_accepts_github_urls() {
        assert_eq!(
            repo_slug("https://github.com/o/crabtrap"),
            Some("o/crabtrap")
        );
        assert_eq!(
            repo_slug("https://github.com/o/crabtrap/"),
            Some("o/crabtrap")
        );
        assert_eq!(
            repo_slug("https://github.com/o/crabtrap.git"),
            Some("o/crabtrap")
        );
    }

    #[test]
    fn repo_slug_rejects_other_urls() {
        assert_eq!(repo_slug(""), None);
        assert_eq!(repo_slug("https://gitlab.com/o/crabtrap"), None);
        assert_eq!(repo_slug("https://github.com/o"), None);
        assert_eq!(repo_slug("https://github.com/o/r/tree/main"), None);
    }

    fn available() -> Result<UpdateStatus, DomainError> {
        Ok(UpdateStatus::Available(ReleaseInfoDto {
            tag: "v9.0.0".into(),
            url: "https://github.com/o/r/releases/tag/v9.0.0".into(),
        }))
    }

    #[test]
    fn available_update_is_announced_with_link_on_any_trigger() {
        for trigger in [Trigger::Startup, Trigger::Manual] {
            let notice = notice(&available(), trigger).expect("notice");
            assert!(notice.title.contains("v9.0.0"));
            assert_eq!(
                notice.url.as_deref(),
                Some("https://github.com/o/r/releases/tag/v9.0.0")
            );
        }
    }

    #[test]
    fn startup_check_is_silent_unless_update_found() {
        assert_eq!(notice(&Ok(UpdateStatus::UpToDate), Trigger::Startup), None);
        let offline = Err(DomainError::InfrastructureError("offline".into()));
        assert_eq!(notice(&offline, Trigger::Startup), None);
    }

    #[test]
    fn manual_check_reports_up_to_date_and_errors() {
        assert!(notice(&Ok(UpdateStatus::UpToDate), Trigger::Manual).is_some());
        let offline = Err(DomainError::InfrastructureError("offline".into()));
        let notice = notice(&offline, Trigger::Manual).expect("notice");
        assert!(notice.message.contains("offline"));
    }
}
