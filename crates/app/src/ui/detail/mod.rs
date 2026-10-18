//! Инспектор обмена: строка обмена (метод, статус, URL) и две панели —
//! запрос и ответ — каждая со своими вкладками ([`tabs`]). Инспектор стоит
//! под таблицей или справа от неё ([`InspectorLayout`], View-меню и кнопка
//! тулбара; сохраняется в config.toml).
//!
//! - [`DetailStore`] — Entity-состояние: следит за `TrafficStore.selected`
//!   (через `cx.observe`), при смене выбора грузит детали через
//!   [`GetExchangeDetailUseCase`] по D4-мосту (tokio-хендл + async-channel —
//!   то же, что в `ui::certificates`; вложенный block_on в gpui-потоке
//!   не используем принципиально), держит снапшот `Arc<ExchangeDetailDto>`,
//!   выбранные вкладки панелей и раскладку.
//! - [`pane::DetailPane`] — RenderOnce: пустое состояние / загрузка /
//!   ошибка / строка обмена + панели. Провал обмена (ExchangeFailed)
//!   приходит как `note` в DTO — показывается строкой под строкой обмена.
//!
//! Содержимое вкладок — в [`crate::ui::inspector`].

pub mod pane;
pub mod tabs;

use std::sync::Arc;

use application::dto::inspection::ExchangeDetailDto;
use application::use_cases::GetExchangeDetailUseCase;
use config::InspectorLayout;
use domain::entities::websocket::{WS_MESSAGES_CAP, WebSocketMessage};
use domain::value_objects::ExchangeId;
use gpui_kit::{App, Context, Entity, Subscription};

use crate::app_state::AppState;
use crate::ui::async_bridge::spawn_ui_task;
use crate::ui::inspector::Side;
use crate::ui::prefs;
use crate::ui::traffic::store::TrafficStore;
use tabs::InspectorTab;

pub use pane::DetailPane;

/// Состояние инспектора.
pub struct DetailStore {
    state: AppState,
    runtime: tokio::runtime::Handle,
    use_case: Arc<GetExchangeDetailUseCase>,
    /// Обмен, чьи детали загружены/грузятся (None — ничего не выбрано).
    loaded_id: Option<ExchangeId>,
    pub detail: Option<Arc<ExchangeDetailDto>>,
    pub loading: bool,
    pub error: Option<String>,
    pub request_tab: InspectorTab,
    pub response_tab: InspectorTab,
    pub layout: InspectorLayout,
    /// Снапшот выбранного обмена уже перечитан после прихода ответа (не
    /// чаще раза за выбор: у провалившегося обмена ответа не будет никогда).
    refreshed: bool,
    /// Сообщения WebSocket выбранного обмена (последние, с потолком).
    pub websocket: Arc<Vec<WebSocketMessage>>,
    /// Курсор следующей подгрузки сообщений.
    ws_next: usize,
    /// Подгрузка сообщений в полёте.
    ws_fetching: bool,
    traffic: Entity<TrafficStore>,
    /// Держит подписку на TrafficStore живой.
    _subscription: Subscription,
}

impl DetailStore {
    pub fn new(
        state: AppState,
        runtime: tokio::runtime::Handle,
        traffic: &Entity<TrafficStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(traffic, |this, traffic, cx| {
            this.sync_selection(&traffic, cx)
        });
        let layout = state.config.ui.inspector_layout;
        Self {
            use_case: Arc::new(GetExchangeDetailUseCase::new(
                state.exchange_repository.clone(),
            )),
            state,
            runtime,
            loaded_id: None,
            detail: None,
            loading: false,
            error: None,
            websocket: Arc::new(Vec::new()),
            ws_next: 0,
            ws_fetching: false,
            traffic: traffic.clone(),
            request_tab: InspectorTab::default_for(Side::Request),
            response_tab: InspectorTab::default_for(Side::Response),
            layout,
            refreshed: false,
            _subscription: subscription,
        }
    }

    pub fn tab(&self, side: Side) -> InspectorTab {
        match side {
            Side::Request => self.request_tab,
            Side::Response => self.response_tab,
        }
    }

    pub fn set_tab(entity: &Entity<Self>, side: Side, tab: InspectorTab, cx: &mut App) {
        entity.update(cx, |this, cx| {
            let slot = match side {
                Side::Request => &mut this.request_tab,
                Side::Response => &mut this.response_tab,
            };
            if *slot != tab {
                *slot = tab;
                cx.notify();
            }
        });
    }

    /// Сменить раскладку (и сохранить её в config.toml).
    pub fn set_layout(entity: &Entity<Self>, layout: InspectorLayout, cx: &mut App) {
        let changed = entity.update(cx, |this, cx| {
            let changed = this.layout != layout;
            this.layout = layout;
            cx.notify();
            changed
        });
        if changed {
            prefs::save_ui(move |ui| config::UiConfig {
                inspector_layout: layout,
                ..ui
            });
        }
    }

    /// Реакция на изменение выбора в таблице (каждый notify TrafficStore;
    /// дёшево выходит при совпадении id). Выбранный обмен ещё ждал ответа,
    /// а теперь ответ пришёл — снапшот перечитывается.
    fn sync_selection(&mut self, traffic: &Entity<TrafficStore>, cx: &mut Context<Self>) {
        let traffic = traffic.read(cx);
        let selected = traffic.selected;
        if selected == self.loaded_id {
            let response_arrived = !self.loading
                && !self.refreshed
                && self.detail.as_ref().is_some_and(|d| d.response.is_none())
                && selected
                    .and_then(|id| traffic.row(&id))
                    .is_some_and(|row| row.responded || row.failed.is_some());
            if let (true, Some(id)) = (response_arrived, selected) {
                self.refreshed = true;
                self.load(id, cx);
                return;
            }
            if let Some(id) = selected {
                self.fetch_websocket(id, cx);
            }
            return;
        }
        self.loaded_id = selected;
        self.refreshed = false;
        self.websocket = Arc::new(Vec::new());
        self.ws_next = 0;
        if let Some(id) = selected {
            self.fetch_websocket(id, cx);
        }
        self.detail = None;
        self.error = None;
        let Some(id) = selected else {
            self.loading = false;
            // Ничего не выбрано (в т.ч. после Clear): отпустить тела вкладок.
            crate::ui::inspector::forget_tab_states(cx);
            cx.notify();
            return;
        };
        self.load(id, cx);
    }

    /// Подгрузить новые сообщения WebSocket (только дельту от курсора);
    /// одна подгрузка в полёте, по её завершении — проверка, не пришли ли
    /// ещё (иначе хвост пачки оставался бы невидимым).
    fn fetch_websocket(&mut self, id: ExchangeId, cx: &mut Context<Self>) {
        let arrived = self
            .traffic
            .read(cx)
            .row(&id)
            .map_or(0, |row| row.ws_messages);
        if self.ws_fetching || arrived <= self.ws_next {
            return;
        }
        self.ws_fetching = true;
        let (uc, from) = (self.use_case.clone(), self.ws_next);
        let entity = cx.entity();
        let runtime = self.runtime.clone();
        spawn_ui_task(
            cx,
            &entity,
            runtime,
            async move { uc.websocket_messages(&id, from).await },
            move |this, result, cx| {
                this.ws_fetching = false;
                if this.loaded_id != Some(id) {
                    return;
                }
                if let Ok(page) = result {
                    // Без копии, если вкладка сейчас не держит список.
                    let all = Arc::make_mut(&mut this.websocket);
                    all.extend(page.messages);
                    let excess = all.len().saturating_sub(WS_MESSAGES_CAP);
                    all.drain(..excess);
                    this.ws_next = page.next;
                    cx.notify();
                }
                this.fetch_websocket(id, cx);
            },
        );
    }

    fn load(&mut self, id: ExchangeId, cx: &mut Context<Self>) {
        tracing::debug!(exchange = %id, "detail load");
        self.loading = true;
        cx.notify();

        let uc = self.use_case.clone();
        let fut = async move { uc.execute(&id).await };
        let entity = cx.entity();
        let runtime = self.runtime.clone();
        spawn_ui_task(cx, &entity, runtime, fut, move |this, result, cx| {
            // Пока грузили, выбрали другой обмен — этот ответ устарел.
            if this.loaded_id != Some(id) {
                return;
            }
            this.loading = false;
            match result {
                Ok(dto) => this.detail = Some(Arc::new(dto)),
                Err(e) => this.error = Some(e.to_string()),
            }
            cx.notify();
        });
    }
}
