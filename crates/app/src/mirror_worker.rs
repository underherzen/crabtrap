//! Поток Mirror: pump событий передаёт сюда id завершённых обменов, поток
//! пишет ответы в файлы (`MirrorResponseUseCase`) — диск не занимает ни UI,
//! ни рантайм прокси. Ошибки возвращаются в UI уведомлением — не чаще раза
//! в 10 секунд (остальные — в лог).

use std::sync::Arc;

use application::use_cases::mirror::MirrorResponseUseCase;
use domain::value_objects::ExchangeId;
use gpui_kit::{App, AsyncApp};

use crate::ui::notify::notify_error;

/// Сколько обменов ждут записи; дальше новые пропускаются (Mirror — копия,
/// а не журнал: лучше пропустить ответ, чем копить память).
const QUEUE: usize = 1024;

/// Очередь потока Mirror.
#[derive(Clone)]
pub struct MirrorQueue(async_channel::Sender<ExchangeId>);

impl MirrorQueue {
    /// Обмен завершён — проверить и записать (без ожидания).
    pub fn completed(&self, id: ExchangeId) {
        if self.0.try_send(id).is_err() {
            tracing::debug!(%id, "mirror queue is full, response not mirrored");
        }
    }
}

/// Сколько ошибок ждут показа; дальше — только в лог.
const ERRORS: usize = 64;
/// Не чаще одного уведомления за этот промежуток: ошибки по разным путям
/// (занятый диск, права) иначе шли бы потоком.
const NOTIFY_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// Запустить поток и приёмник ошибок в UI. `None` — поток не стартовал
/// (Mirror не работает, причина — в логе).
pub fn spawn(use_case: Arc<MirrorResponseUseCase>, cx: &mut App) -> Option<MirrorQueue> {
    let (tx, rx) = async_channel::bounded::<ExchangeId>(QUEUE);
    let (errors_tx, errors_rx) = async_channel::bounded::<String>(ERRORS);
    let spawned = std::thread::Builder::new()
        .name("crabtrap-mirror".into())
        .spawn(move || {
            while let Ok(id) = rx.recv_blocking() {
                match futures::executor::block_on(use_case.mirror(&id)) {
                    Ok(Some(path)) => tracing::debug!(%id, path = %path.display(), "mirrored"),
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%id, %error, "mirror failed");
                        let _ = errors_tx.try_send(error.to_string());
                    }
                }
            }
        });
    if let Err(error) = spawned {
        tracing::error!(%error, "cannot start the mirror thread, Mirror is off");
        return None;
    }
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut last_shown: Option<std::time::Instant> = None;
        while let Ok(error) = errors_rx.recv().await {
            if last_shown.is_some_and(|at| at.elapsed() < NOTIFY_EVERY) {
                continue;
            }
            last_shown = Some(std::time::Instant::now());
            cx.update(|cx| notify_error(format!("Mirror: {error}"), cx));
        }
    })
    .detach();
    Some(MirrorQueue(tx))
}
