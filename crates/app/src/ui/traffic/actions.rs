//! Действия над трафиком, общие для тулбара, меню Proxy и иконки в строке
//! меню macOS: очистка, тумблер записи тел и пауза.

use application::dto::proxy_config::EngineCommand;
use application::use_cases::ClearTrafficUseCase;
use gpui_kit::{App, Entity};

use crate::app_state::AppState;
use crate::ui::traffic::store::TrafficStore;

/// Clear: строки таблицы — сразу, обмены и тела в репозиториях — на
/// движковом рантайме.
pub fn clear_traffic(
    state: &AppState,
    traffic: &Entity<TrafficStore>,
    runtime: &tokio::runtime::Handle,
    cx: &mut App,
) {
    traffic.update(cx, |store, cx| {
        store.clear();
        cx.notify();
    });
    let uc = ClearTrafficUseCase::new(
        state.exchange_repository.clone(),
        state.body_repository.clone(),
    );
    let (tx, rx) = async_channel::bounded(1);
    runtime.spawn(async move {
        let _ = tx.send(uc.execute().await).await;
    });
    cx.spawn(async move |_| {
        if let Ok(Err(e)) = rx.recv().await {
            tracing::warn!(error = %e, "clear traffic failed");
        }
    })
    .detach();
}

/// Тумблер записи тел (EngineCommand::SetBodyCapture на лету).
pub fn toggle_recording(state: &AppState, traffic: &Entity<TrafficStore>, cx: &mut App) {
    let engine = state.engine_command.clone();
    traffic.update(cx, |store, cx| {
        store.recording = !store.recording;
        let enabled = store.recording;
        cx.notify();
        engine.send(EngineCommand::SetBodyCapture { enabled });
    });
}

/// Пауза обновлений таблицы: новые обмены в таблицу не попадают (и после
/// снятия паузы не появятся), но пишутся в репозиторий и в статистику.
pub fn toggle_pause(traffic: &Entity<TrafficStore>, cx: &mut App) {
    traffic.update(cx, |store, cx| {
        store.paused = !store.paused;
        cx.notify();
    });
}
