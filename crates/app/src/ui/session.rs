//! File → Open Session… / Save Session…: весь трафик — в файл `.crabtrap`
//! и обратно (сценарий — `application::sessions`). Открытые обмены
//! добавляются к текущему трафику с тегом `imported`.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use application::use_cases::sessions::{ImportOutcome, SessionFileUseCase};
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, AsyncApp, Entity, Global, PathPromptOptions};

use crate::ui::export::default_folder;
use crate::ui::notify::{notify, notify_error};
use crate::ui::traffic::store::TrafficStore;

/// Расширение файла сессии.
pub const SESSION_EXTENSION: &str = "crabtrap";

#[derive(Clone)]
pub struct SessionDeps {
    pub use_case: Arc<SessionFileUseCase>,
    pub traffic: Entity<TrafficStore>,
}

/// Выполнить работу с файлом на своём потоке: сжатие, разбор JSON и диск
/// не должны занимать рантайм, на котором работает прокси. Репозитории
/// синхронны внутри (как в pump'е событий) — `block_on` им достаточно.
fn off_proxy_runtime<T: Send + 'static>(
    job: impl Future<Output = T> + Send + 'static,
) -> async_channel::Receiver<T> {
    let (tx, rx) = async_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name("crabtrap-session-file".into())
        .spawn(move || {
            let _ = tx.send_blocking(futures::executor::block_on(job));
        });
    if let Err(error) = spawned {
        tracing::error!(%error, "cannot start the session file thread");
    }
    rx
}

impl Global for SessionDeps {}

pub fn install(deps: SessionDeps, cx: &mut App) {
    cx.set_global(deps);
}

fn deps(cx: &App) -> Option<SessionDeps> {
    let deps = cx.try_global::<SessionDeps>().cloned();
    if deps.is_none() {
        tracing::warn!("session action requested before install");
    }
    deps
}

/// Имя файла по умолчанию: `crabtrap-2026-10-20-1530.crabtrap`.
pub fn session_file_name() -> String {
    format!(
        "crabtrap-{}.{SESSION_EXTENSION}",
        chrono::Local::now().format("%Y-%m-%d-%H%M")
    )
}

/// Спросить путь и сохранить весь трафик.
pub fn save_session(cx: &mut App) {
    let Some(deps) = deps(cx) else {
        return;
    };
    let chosen = cx.prompt_for_new_path(&default_folder(), Some(&session_file_name()));
    cx.spawn(async move |cx: &mut AsyncApp| {
        let path = match chosen.await {
            Ok(Ok(Some(path))) => path,
            Ok(Ok(None)) | Err(_) => return,
            Ok(Err(error)) => {
                cx.update(|cx| notify_error(format!("Can't open the save dialog: {error}"), cx));
                return;
            }
        };
        let target = path.clone();
        let use_case = deps.use_case.clone();
        let rx = off_proxy_runtime(async move { use_case.save(&target).await });
        let Ok(result) = rx.recv().await else {
            cx.update(|cx| notify_error("Can't save the session".into(), cx));
            return;
        };
        cx.update(|cx| match result {
            Ok(count) => notify(
                Notification::success(format!(
                    "Saved {count} request{} to {}",
                    plural(count),
                    file_name(&path)
                )),
                cx,
            ),
            Err(error) => notify_error(format!("Can't save the session: {error}"), cx),
        });
    })
    .detach();
}

/// Выбрать файл сессии и добавить её обмены в трафик.
pub fn open_session(cx: &mut App) {
    import_from_file("Open", cx, |use_case, path| async move {
        use_case.open(&path).await
    });
}

/// Выбрать HAR и добавить его записи в трафик.
pub fn import_har(cx: &mut App) {
    import_from_file("Import", cx, |use_case, path| async move {
        use_case.import_har(&path).await
    });
}

/// Диалог выбора файла → `run` в рантайме → строки в таблицу и итог.
fn import_from_file<F, Fut>(prompt: &'static str, cx: &mut App, run: F)
where
    F: FnOnce(Arc<SessionFileUseCase>, PathBuf) -> Fut + Send + 'static,
    Fut: Future<Output = ImportOutcome> + Send + 'static,
{
    let Some(deps) = deps(cx) else {
        return;
    };
    let chosen = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(prompt.into()),
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let path = match chosen.await {
            Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                Some(path) => path,
                None => return,
            },
            Ok(Ok(None)) | Err(_) => return,
            Ok(Err(error)) => {
                cx.update(|cx| notify_error(format!("Can't open the file dialog: {error}"), cx));
                return;
            }
        };
        let rx = off_proxy_runtime(run(deps.use_case.clone(), path.clone()));
        let Ok(outcome) = rx.recv().await else {
            cx.update(|cx| notify_error(format!("Can't open {}", file_name(&path)), cx));
            return;
        };
        cx.update(|cx| show_import(outcome, &deps.traffic, &file_name(&path), cx));
    })
    .detach();
}

/// Добавить открытое в таблицу и сказать, что вышло (часть до ошибки
/// остаётся — она уже в трафике).
pub fn show_import(
    outcome: ImportOutcome,
    traffic: &Entity<TrafficStore>,
    name: &str,
    cx: &mut App,
) {
    let count = outcome.imported.len();
    if count > 0 {
        traffic.update(cx, |store, cx| {
            if store.import(outcome.imported) {
                cx.notify();
            }
        });
    }
    let skipped = match outcome.skipped {
        0 => String::new(),
        n => format!(" ({n} entr{} skipped)", if n == 1 { "y" } else { "ies" }),
    };
    match outcome.error {
        None => notify(
            Notification::success(format!(
                "Opened {count} request{} from {name}{skipped}",
                plural(count)
            )),
            cx,
        ),
        Some(error) if count > 0 => notify_error(
            format!(
                "Opened {count} request{} from {name}, then stopped: {error}",
                plural(count)
            ),
            cx,
        ),
        Some(error) => notify_error(format!("Can't open {name}: {error}"), cx),
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_name_has_the_extension() {
        assert!(session_file_name().starts_with("crabtrap-"));
        assert!(session_file_name().ends_with(".crabtrap"));
    }
}
