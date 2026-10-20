//! Экспорт из трафика: «Copy as cURL» (в буфер обмена) и HAR-файл для
//! обмена или всей сессии (путь — системный диалог сохранения).

use std::path::PathBuf;
use std::sync::Arc;

use application::use_cases::export::{ExportTrafficUseCase, HarScope};
use domain::value_objects::ExchangeId;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, AsyncApp, ClipboardItem, Global};

use crate::ui::notify::{notify, notify_error};

/// Сценарий и рантайм (tokio) экспорта.
#[derive(Clone)]
pub struct ExportDeps {
    pub use_case: Arc<ExportTrafficUseCase>,
    pub runtime: tokio::runtime::Handle,
}

impl Global for ExportDeps {}

pub fn install(deps: ExportDeps, cx: &mut App) {
    cx.set_global(deps);
}

fn deps(cx: &App) -> Option<ExportDeps> {
    let deps = cx.try_global::<ExportDeps>().cloned();
    if deps.is_none() {
        tracing::warn!("export requested before install");
    }
    deps
}

/// Команда curl запроса — в буфер обмена.
pub fn copy_curl(id: ExchangeId, cx: &mut App) {
    let Some(deps) = deps(cx) else {
        return;
    };
    let (tx, rx) = async_channel::bounded(1);
    deps.runtime.spawn(async move {
        let _ = tx.send(deps.use_case.curl(&id).await).await;
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let Ok(result) = rx.recv().await else {
            return;
        };
        cx.update(|cx| match result {
            Ok(command) => {
                cx.write_to_clipboard(ClipboardItem::new_string(command));
                notify(Notification::success("cURL command copied"), cx);
            }
            Err(error) => notify_error(format!("Can't copy as cURL: {error}"), cx),
        });
    })
    .detach();
}

/// Спросить путь и записать HAR (`with_bodies = false` — только заголовки).
pub fn export_har(scope: HarScope, with_bodies: bool, suggested_name: String, cx: &mut App) {
    let Some(deps) = deps(cx) else {
        return;
    };
    let chosen = cx.prompt_for_new_path(&default_folder(), Some(&suggested_name));
    cx.spawn(async move |cx: &mut AsyncApp| {
        let path = match chosen.await {
            Ok(Ok(Some(path))) => path,
            Ok(Ok(None)) | Err(_) => return,
            Ok(Err(error)) => {
                cx.update(|cx| notify_error(format!("Can't open the save dialog: {error}"), cx));
                return;
            }
        };
        let (tx, rx) = async_channel::bounded(1);
        let target = path.clone();
        deps.runtime.spawn(async move {
            let _ = tx
                .send(deps.use_case.har(scope, with_bodies, &target).await)
                .await;
        });
        let Ok(result) = rx.recv().await else {
            return;
        };
        cx.update(|cx| match result {
            Ok(count) => notify(
                Notification::success(format!(
                    "Exported {count} request{} to {}",
                    if count == 1 { "" } else { "s" },
                    path.file_name().unwrap_or_default().to_string_lossy()
                )),
                cx,
            ),
            Err(error) => notify_error(format!("Can't export HAR: {error}"), cx),
        });
    })
    .detach();
}

/// Имя HAR-файла сессии: `crabtrap-2026-10-20-1530.har`.
pub fn session_file_name() -> String {
    format!(
        "crabtrap-{}.har",
        chrono::Local::now().format("%Y-%m-%d-%H%M")
    )
}

/// Имя HAR-файла одного обмена по хосту: `api.example.com.har`.
pub fn exchange_file_name(host: &str) -> String {
    let safe: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.har")
}

/// Папка по умолчанию — Загрузки (нет HOME — текущая).
fn default_folder() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Downloads"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_safe() {
        assert_eq!(
            exchange_file_name("api.example.com:8443"),
            "api.example.com_8443.har"
        );
        assert!(session_file_name().starts_with("crabtrap-"));
        assert!(session_file_name().ends_with(".har"));
    }
}
