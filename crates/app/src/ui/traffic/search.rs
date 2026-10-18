//! Поиск по заголовкам/телам (Phase 5): поле в тулбаре + полоса результатов.
//!
//! - [`SearchStore`] — Entity: Enter в поле запускает
//!   [`SearchExchangesUseCase`] по D4-мосту (`spawn_ui_task`: tokio-хендл
//!   движка + async-channel, UI-поток не блокируется). Пришедшие хиты
//!   сужают таблицу транзиентным id-фильтром
//!   ([`TrafficStore::set_search_ids`], пересечение с фильтром тулбара),
//!   клик по строке хита выбирает обмен (детали перегрузятся через
//!   наблюдение `TrafficStore.selected`).
//! - [`SearchStrip`] — RenderOnce-полоса над таблицей: «N hits in M
//!   exchanges», первые [`MAX_ROWS`] строк (field + сниппет моноширинно),
//!   кнопка ✕ гасит поиск.
//! - Esc в поле чистит его (`clean_on_escape`) → Change с пустым значением
//!   → сброс поиска; ⌘F фокусирует поле через action [`FocusSearch`].
//!
//! Синтаксис поля: подстрока (case-insensitive) или `re:<regex>` — regex-режим.

use std::collections::HashSet;
use std::sync::Arc;

use application::use_cases::inspection::{SearchExchangesUseCase, SearchQueryDto, SearchScope};
use domain::value_objects::ExchangeId;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _,
    StatefulInteractiveElement as _, Styled,
};

use crate::app_state::AppState;
use crate::ui::async_bridge::spawn_ui_task;
use crate::ui::traffic::store::TrafficStore;

/// Лимит use case (выдача ограничена на стороне домена).
const SEARCH_LIMIT: usize = 200;
/// Сколько строк хитов рисуем в полосе (без виртуализации — больше не надо).
const MAX_ROWS: usize = 50;

// ⌘F — фокусировать поле поиска (биндинг регистрируется в `main`).
gpui_kit::actions!(crabtrap, [FocusSearch]);

/// Состояние поиска (Entity).
pub struct SearchStore {
    use_case: Arc<SearchExchangesUseCase>,
    runtime: tokio::runtime::Handle,
    traffic: Entity<TrafficStore>,
    /// Последний выполненный запрос (None — поиск неактивен, полоса скрыта).
    pub query: Option<String>,
    pub hits: Vec<application::use_cases::inspection::SearchHitDto>,
    pub loading: bool,
    pub error: Option<String>,
}

impl SearchStore {
    pub fn new(
        state: &AppState,
        runtime: tokio::runtime::Handle,
        traffic: Entity<TrafficStore>,
    ) -> Self {
        Self {
            use_case: Arc::new(
                SearchExchangesUseCase::new(
                    state.exchange_repository.clone(),
                    state.body_repository.clone(),
                )
                .with_decoder(state.body_decoder.clone()),
            ),
            runtime,
            traffic,
            query: None,
            hits: Vec::new(),
            loading: false,
            error: None,
        }
    }

    /// Запустить поиск (Enter). Пустое поле / пустой `re:` — сброс.
    pub fn run(&mut self, raw: &str, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let trimmed = raw.trim();
        let (regex, text) = match trimmed.strip_prefix("re:") {
            Some(rest) => (true, rest.trim()),
            None => (false, trimmed),
        };
        if text.is_empty() {
            self.reset(cx);
            return;
        }
        self.query = Some(trimmed.to_string());
        self.loading = true;
        self.error = None;
        self.hits.clear();
        cx.notify();

        let query = SearchQueryDto {
            text: text.to_string(),
            case_sensitive: false,
            regex,
            scope: SearchScope::All,
            limit: SEARCH_LIMIT,
        };
        let uc = self.use_case.clone();
        let fut = async move { uc.execute(query).await };
        let entity = cx.entity();
        let runtime = self.runtime.clone();
        spawn_ui_task(cx, &entity, runtime, fut, |this, result, cx| {
            this.loading = false;
            this.hits.clear();
            match result {
                Ok(hits) => {
                    let ids: HashSet<ExchangeId> = hits.iter().map(|h| h.exchange_id).collect();
                    this.traffic.update(cx, |s, cx| {
                        s.set_search_ids(Some(ids));
                        cx.notify();
                    });
                    this.hits = hits;
                }
                Err(e) => {
                    this.error = Some(e.to_string());
                    this.traffic.update(cx, |s, cx| {
                        s.set_search_ids(None);
                        cx.notify();
                    });
                }
            }
            cx.notify();
        });
    }

    /// Сбросить поиск (✕ / пустое поле): полоса, id-фильтр таблицы.
    /// Значение поля ввода чистит вызывающий — `set_value` не эмитит Change.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.query = None;
        self.hits.clear();
        self.error = None;
        self.loading = false;
        self.traffic.update(cx, |s, cx| {
            s.set_search_ids(None);
            cx.notify();
        });
        cx.notify();
    }

    /// Клик по хиту: выбрать обмен (панель деталей следит за selected).
    pub fn select_hit(&mut self, id: ExchangeId, cx: &mut Context<Self>) {
        self.traffic.update(cx, |s, cx| {
            s.selected = Some(id);
            cx.notify();
        });
    }

    /// Число уникальных обменов в выдаче («N hits in M exchanges»).
    pub fn exchanges_count(&self) -> usize {
        self.hits
            .iter()
            .map(|h| h.exchange_id)
            .collect::<HashSet<_>>()
            .len()
    }
}

/// Сборка поля поиска (нужен `&mut Window`).
pub fn build_search_input(window: &mut gpui_kit::Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| {
        let mut state = InputState::new(window, cx).placeholder("search headers & bodies (⌘F)");
        state.set_clean_on_escape(true);
        state
    })
}

/// Подписка поля: Enter — запуск, пустое значение (Esc/✕) — сброс.
/// Вызывается один раз при сборке окна (как `bind_filter_input`).
pub fn bind_search_input(input: &Entity<InputState>, search: &Entity<SearchStore>, cx: &mut App) {
    let search = search.clone();
    cx.subscribe(input, move |input, event: &InputEvent, cx| match event {
        InputEvent::PressEnter { .. } => {
            let value = input.read(cx).value().to_string();
            search.update(cx, |s, cx| s.run(&value, cx));
        }
        InputEvent::Change if input.read(cx).value().is_empty() => {
            search.update(cx, |s, cx| s.reset(cx));
        }
        _ => {}
    })
    .detach();
}

/// Полоса результатов: рендерится под тулбаром, пока поиск активен.
#[derive(gpui_kit::IntoElement)]
pub struct SearchStrip {
    store: Entity<SearchStore>,
    input: Entity<InputState>,
}

impl SearchStrip {
    pub fn new(store: Entity<SearchStore>, input: Entity<InputState>) -> Self {
        Self { store, input }
    }
}

impl gpui_kit::RenderOnce for SearchStrip {
    fn render(self, _: &mut gpui_kit::Window, cx: &mut App) -> impl gpui_kit::IntoElement {
        use gpui_kit::component::ActiveTheme as _;
        use gpui_kit::component::Sizable as _;
        use gpui_kit::component::button::{Button, ButtonVariants as _};
        use gpui_kit::{ElementId, IntoElement, ParentElement, div, px};

        let theme = cx.theme();
        let p = crate::ui::theme::glass(cx);
        let (query, loading, error, hits) = {
            let s = self.store.read(cx);
            (s.query.clone(), s.loading, s.error.clone(), s.hits.clone())
        };

        // Поиск неактивен — полоса не занимается местом.
        if query.is_none() && !loading && error.is_none() {
            return div().into_any_element();
        }

        let muted = p.text_secondary;

        // Статус: «searching…» | ошибка | «N hits in M exchanges».
        let status = if loading {
            "searching…".to_string()
        } else if let Some(err) = &error {
            format!("Search failed: {err}")
        } else {
            let mut line = format!(
                "{} hit{} in {} exchange{}",
                hits.len(),
                if hits.len() == 1 { "" } else { "s" },
                self.store.read(cx).exchanges_count(),
                // exchanges_count не может быть 1 при hits.len() != 1
                if hits.len() == 1 { "" } else { "s" },
            );
            if let Some(q) = &query {
                line.push_str(&format!(" for “{q}”"));
            }
            if hits.len() > MAX_ROWS {
                line.push_str(&format!(" — showing first {MAX_ROWS}"));
            } else if hits.len() >= SEARCH_LIMIT {
                line.push_str(" — stopped at limit");
            }
            line
        };
        let status_color = if error.is_some() && !loading {
            theme.danger
        } else {
            p.text_secondary
        };

        // ✕ — сбросить поиск и поле (set_value сам Change не эмитит).
        let store_for_clear = self.store.clone();
        let input_for_clear = self.input.clone();
        let clear = Button::new("search-clear")
            .icon(gpui_kit::component::IconName::Close)
            .ghost()
            .xsmall()
            .tooltip("Clear search")
            .on_click(move |_, window, cx| {
                input_for_clear.update(cx, |input, cx| input.set_value("", window, cx));
                store_for_clear.update(cx, |s, cx| s.reset(cx));
            });

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_1()
            .text_size(px(12.))
            .child(div().flex_1().text_color(status_color).child(status))
            .child(clear);

        // Строки хитов: field (muted) + сниппет (mono), клик — выбрать обмен.
        let mut rows = div().flex().flex_col().pb_1();
        for (i, hit) in hits.iter().take(MAX_ROWS).enumerate() {
            let store_for_row = self.store.clone();
            let id = hit.exchange_id;
            rows = rows.child(
                div()
                    .id(ElementId::NamedInteger("search-hit".into(), i as u64))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py(px(1.))
                    .cursor_pointer()
                    .hover(|style| style.bg(p.row_hover))
                    .on_click(move |_, _, cx| {
                        store_for_row.update(cx, |s, cx| s.select_hit(id, cx));
                    })
                    .child(
                        div()
                            .w(px(230.))
                            .flex_shrink_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(11.))
                            .text_color(muted)
                            .child(hit.field.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(11.))
                            .font_family(".AppleSystemUIFontMonospaced")
                            .child(hit.snippet.clone()),
                    ),
            );
        }
        if hits.is_empty() && !loading && error.is_none() {
            rows = rows.child(
                div()
                    .px_4()
                    .py_1()
                    .text_size(px(11.))
                    .text_color(muted)
                    .child("no hits"),
            );
        }

        div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(p.hairline)
            .bg(p.header)
            .max_h(px(180.))
            .id("search-strip")
            .overflow_y_scroll()
            .child(header)
            .child(rows)
            .into_any_element()
    }
}
