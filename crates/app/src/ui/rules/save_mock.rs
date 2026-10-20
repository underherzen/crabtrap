//! «Save Response & Map Local…»: сохранить тело ответа в каталог моков и
//! открыть диалог Map Local с файлом, статусом и заголовками ответа.
//! Ошибка (ответ не дозагружен, тело не записано) — уведомлением в окне.

use std::sync::Arc;

use application::use_cases::rules::location_form::LocationForm;
use application::use_cases::rules::save_response_mock::SaveResponseMockUseCase;
use domain::value_objects::ExchangeId;
use gpui_kit::{App, Global};

use super::Prefill;
use super::RulesTab;
use super::window as rules_window;
use crate::ui::notify::notify_error;

/// Сценарий и рантайм (tokio) для сохранения мока.
#[derive(Clone)]
pub struct SaveMockDeps {
    pub use_case: Arc<SaveResponseMockUseCase>,
    pub runtime: tokio::runtime::Handle,
}

impl Global for SaveMockDeps {}

pub fn install(deps: SaveMockDeps, cx: &mut App) {
    cx.set_global(deps);
}

/// Сохранить ответ обмена `id` и открыть Map Local с `location`.
pub fn save_and_open(id: ExchangeId, location: LocationForm, cx: &mut App) {
    let Some(deps) = cx.try_global::<SaveMockDeps>().cloned() else {
        tracing::warn!("save mock requested before install");
        return;
    };
    let (tx, rx) = async_channel::bounded(1);
    deps.runtime.spawn(async move {
        let _ = tx.send(deps.use_case.execute(&id).await).await;
    });
    cx.spawn(async move |cx| {
        let Ok(result) = rx.recv().await else {
            return;
        };
        cx.update(|cx| match result {
            Ok(mock) => {
                let prefill = Prefill {
                    location,
                    mock: Some(mock),
                };
                rules_window::open_prefilled(RulesTab::MapLocal, Some(prefill), cx);
            }
            Err(error) => notify_error(format!("Can't save the response: {error}"), cx),
        });
    })
    .detach();
}
