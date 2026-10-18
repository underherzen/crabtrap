//! Вкладка «Headers»: заголовки одной стороны обмена, фильтр по подстроке
//! (case-insensitive); значение — целиком, копирование — см. [`super::kv`].
//!
//! Вкладка — `RenderOnce`, но фильтр — UI-состояние, поэтому оно живёт в
//! отдельной сущности [`HeadersPane`], по одной на (обмен, сторону) —
//! [`super::tab_state`], при смене обмена старые выбрасываются.
//! `HeadersTab::render` достаёт/создаёт сущность и делегирует рендер ей.

use std::sync::Arc;

use application::dto::capture::HeaderEntryDto;
use application::dto::inspection::ExchangeDetailDto;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement,
    RenderOnce, StatefulInteractiveElement as _, Styled, Subscription, Window, div, px,
};

use super::kv::KvRow;
use super::{ExchangeContext, Side, tab_state};
use crate::ui::theme::glass;
use crate::ui::theme::metrics::{TEXT_SM, TEXT_XS};

/// Фильтр заголовков: подстрока (case-insensitive) по имени ИЛИ значению.
/// Пустой/пробельный запрос — без фильтра.
pub fn filter_headers<'a>(headers: &'a [HeaderEntryDto], query: &str) -> Vec<&'a HeaderEntryDto> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return headers.iter().collect();
    }
    headers
        .iter()
        .filter(|h| {
            h.name.to_lowercase().contains(&query) || h.value.to_lowercase().contains(&query)
        })
        .collect()
}

/// Состояние вкладки для одной стороны обмена (фильтр).
pub struct HeadersPane {
    /// Свежий снапшот деталей: DetailPane перезагружает его, вкладка
    /// подтягивает при каждом собственном рендере.
    detail: Arc<ExchangeDetailDto>,
    side: Side,
    search: Entity<InputState>,
    /// Держит подписку на InputEvent фильтра живой.
    _search_sub: Subscription,
}

impl HeadersPane {
    /// Создание состояния (нужен `&mut Window` для InputState).
    fn new(
        detail: Arc<ExchangeDetailDto>,
        side: Side,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter headers"));
        cx.new(|cx| {
            // Каждый ввод → перерисовка вкладки (строки фильтруются в render).
            let search_sub = cx.subscribe(
                &search,
                |_,
                 _,
                 _: &gpui_kit::component::input::InputEvent,
                 cx: &mut gpui_kit::Context<Self>| { cx.notify() },
            );
            Self {
                detail,
                side,
                search,
                _search_sub: search_sub,
            }
        })
    }

    fn headers(&self) -> Option<&[HeaderEntryDto]> {
        match self.side {
            Side::Request => Some(&self.detail.request.headers),
            Side::Response => self.detail.response.as_ref().map(|r| r.headers.as_slice()),
        }
    }
}

impl gpui_kit::Render for HeadersPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = glass(cx);
        let mono = cx.theme().mono_font_family.clone();
        let Some(all) = self.headers() else {
            return super::placeholder("Waiting for the response…", &p).into_any_element();
        };
        let query = self.search.read(cx).value().to_string();
        let filtered = filter_headers(all, &query);
        let count = if filtered.len() == all.len() {
            format!("{} headers", all.len())
        } else {
            format!("{} of {} headers", filtered.len(), all.len())
        };
        let toolbar = h_flex()
            .flex_none()
            .items_center()
            .gap_2()
            .child(
                Input::new(&self.search)
                    .h(px(24.))
                    .w(px(220.))
                    .small()
                    .prefix(
                        Icon::new(IconName::Search)
                            .xsmall()
                            .text_color(p.text_tertiary),
                    )
                    .cleanable(true),
            )
            .child(
                div()
                    .text_size(px(TEXT_XS))
                    .text_color(p.text_tertiary)
                    .child(count),
            );

        let rows = if filtered.is_empty() {
            let message = if query.trim().is_empty() {
                "No headers".to_string()
            } else {
                format!("No headers match ‘{}’", query.trim())
            };
            vec![
                div()
                    .px_2()
                    .py(px(6.))
                    .text_size(px(TEXT_SM))
                    .text_color(p.text_secondary)
                    .child(message)
                    .into_any_element(),
            ]
        } else {
            filtered
                .iter()
                .enumerate()
                .map(|(ix, header)| {
                    KvRow::new(
                        ("headers-row", ix),
                        header.name.clone(),
                        header.value.clone(),
                        mono.clone(),
                    )
                    .striped(ix % 2 == 1)
                    .into_any_element()
                })
                .collect()
        };

        v_flex()
            .id(("headers-tab", self.side as usize))
            .size_full()
            .overflow_y_scroll()
            .p(px(10.))
            .gap(px(8.))
            // Дети прокручиваемой колонки не сжимаются (flex_none): иначе
            // flex сжимает строки вместо переполнения — прокрутка не работает.
            .child(toolbar)
            .child(v_flex().flex_none().children(rows))
            .into_any_element()
    }
}

/// Headers (RenderOnce-обёртка над [`HeadersPane`]).
#[derive(gpui_kit::IntoElement)]
pub struct HeadersTab {
    ctx: ExchangeContext,
}

impl HeadersTab {
    pub fn new(ctx: ExchangeContext) -> Self {
        Self { ctx }
    }
}

impl RenderOnce for HeadersTab {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (detail, side) = (self.ctx.detail.clone(), self.ctx.side);
        let pane = tab_state(detail.summary.id, side, cx, |cx| {
            HeadersPane::new(detail, side, window, cx)
        });
        // Подтягиваем свежий снапшот (DetailPane мог перезагрузить детали).
        pane.update(cx, |state, _| state.detail = self.ctx.detail.clone());
        pane
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, value: &str) -> HeaderEntryDto {
        HeaderEntryDto {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn empty_query_returns_all() {
        let headers = vec![header("Host", "example.com"), header("Accept", "*/*")];
        assert_eq!(filter_headers(&headers, "").len(), 2);
        assert_eq!(filter_headers(&headers, "   ").len(), 2);
    }

    #[test]
    fn filters_by_name_or_value_case_insensitive() {
        let headers = vec![
            header("Content-Type", "application/json"),
            header("X-Trace", "abc-JSON-def"),
            header("Accept", "text/html"),
        ];
        let hits = filter_headers(&headers, "json");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].name, "Content-Type");
        assert_eq!(hits[1].name, "X-Trace");

        assert_eq!(filter_headers(&headers, "CONTENT-TYPE").len(), 1);
    }

    #[test]
    fn no_match_returns_empty() {
        let headers = vec![header("Host", "example.com")];
        assert!(filter_headers(&headers, "nope").is_empty());
    }

    #[test]
    fn query_is_trimmed() {
        let headers = vec![header("Host", "example.com")];
        assert_eq!(filter_headers(&headers, "  host  ").len(), 1);
    }
}
