//! Рендер инспектора: строка обмена + панели запроса и ответа.
//!
//! Под таблицей панели стоят рядом (запрос | ответ), справа от таблицы —
//! друг под другом; разделитель между ними перетаскивается.

use std::sync::Arc;
use std::time::Duration;

use application::dto::inspection::ExchangeDetailDto;
use config::InspectorLayout;
use gpui_kit::base::{h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement, RenderOnce, StatefulInteractiveElement as _, Styled, Window, div,
    ease_out_quint, px,
};

use super::DetailStore;
use super::tabs::InspectorTab;
use crate::ui::inspector::body::BodyTab;
use crate::ui::inspector::cookies::CookiesTab;
use crate::ui::inspector::headers::HeadersTab;
use crate::ui::inspector::hex::HexTab;
use crate::ui::inspector::kv::copy_text;
use crate::ui::inspector::overview::{OverviewTab, full_url};
use crate::ui::inspector::query::QueryTab;
use crate::ui::inspector::raw::RawTab;
use crate::ui::inspector::{ExchangeContext, Side};
use crate::ui::theme::components::{SegmentedControl, ToolbarButton, tone_pill};
use crate::ui::theme::metrics::{TEXT, TEXT_SM};
use crate::ui::theme::semantic::{ExchangeState, method_tone};
use crate::ui::theme::{Palette, ToneName, glass};
use crate::ui::traffic::table::{duration_label, human_bytes};

/// Высота строки обмена и шапок панелей.
const HEADER_HEIGHT: f32 = 38.;
/// Проявление содержимого при смене обмена.
const FADE_MS: u64 = 140;

/// Инспектор (RenderOnce над [`DetailStore`]).
#[derive(gpui_kit::IntoElement)]
pub struct DetailPane {
    store: Entity<DetailStore>,
}

impl DetailPane {
    pub fn new(store: Entity<DetailStore>) -> Self {
        Self { store }
    }
}

impl RenderOnce for DetailPane {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let store = self.store.read(cx);
        let pane = div()
            .flex()
            .flex_col()
            .size_full()
            .min_h(px(0.))
            .text_color(p.text);

        let Some(detail) = store.detail.clone() else {
            let message = if store.loading {
                "Loading…".to_string()
            } else if let Some(err) = &store.error {
                format!("Failed to load exchange: {err}")
            } else {
                "Select a request to inspect it".to_string()
            };
            return pane.child(empty_state(message, &p));
        };

        // Провал обмена: reason из ExchangeFailed живёт в note DTO.
        let failure_banner = detail
            .note
            .as_deref()
            .and_then(|note| note.strip_prefix("failed:"))
            .map(|reason| {
                div()
                    .flex_none()
                    .px(px(12.))
                    .py(px(6.))
                    .bg(p.tone(ToneName::Red).bg)
                    .text_size(px(TEXT_SM))
                    .text_color(p.tone(ToneName::Red).fg)
                    .child(format!("Exchange failed — {}", reason.trim()))
            });

        let ctx = ExchangeContext::new(detail.clone(), &store.state, store.runtime.clone())
            .with_websocket(store.websocket.clone());
        let request = side_pane(
            &self.store,
            Side::Request,
            store.tab(Side::Request),
            ctx.clone(),
            &p,
        );
        let response = side_pane(
            &self.store,
            Side::Response,
            store.tab(Side::Response),
            ctx,
            &p,
        );
        let sides = match store.layout {
            InspectorLayout::Bottom => h_resizable("inspector-sides-h")
                .child(resizable_panel().child(request))
                .child(resizable_panel().child(response))
                .into_any_element(),
            InspectorLayout::Right => v_resizable("inspector-sides-v")
                .child(resizable_panel().child(request))
                .child(resizable_panel().child(response))
                .into_any_element(),
        };

        // Смена обмена — короткое проявление содержимого (one-shot: id
        // анимации свой у каждого обмена, перерисовок в покое нет).
        let fade_id =
            gpui_kit::ElementId::Name(format!("inspector-fade-{}", detail.summary.id).into());
        pane.child(exchange_line(&detail, &p))
            .when_some(failure_banner, |this, banner| this.child(banner))
            .child(div().flex_1().min_h(px(0.)).child(sides).with_animation(
                fade_id,
                Animation::new(Duration::from_millis(FADE_MS)).with_easing(ease_out_quint()),
                |this, delta| this.opacity(delta),
            ))
    }
}

fn empty_state(message: String, p: &Palette) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .child(
            Icon::new(IconName::Inspector)
                .large()
                .text_color(p.text_tertiary),
        )
        .child(
            div()
                .text_size(px(TEXT))
                .text_color(p.text_secondary)
                .child(message),
        )
        .into_any_element()
}

/// Строка обмена: метод, статус, URL, длительность и размер, «копировать URL».
fn exchange_line(detail: &Arc<ExchangeDetailDto>, p: &Palette) -> AnyElement {
    let s = &detail.summary;
    let failed = detail
        .note
        .as_deref()
        .is_some_and(|n| n.starts_with("failed:"));
    let state = ExchangeState::of(s.status, failed);
    let status = if failed {
        "ERR".to_string()
    } else {
        s.status
            .map(|c| c.to_string())
            .unwrap_or_else(|| "…".into())
    };
    let url = full_url(&s.scheme, &s.host, &s.path);
    let meta = format!(
        "{} · {}",
        duration_label(s.duration_ms),
        if s.resp_size > 0 {
            human_bytes(s.resp_size)
        } else {
            "…".into()
        }
    );
    let copy_url = url.clone();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .h(px(HEADER_HEIGHT))
        .px(px(12.))
        .border_b_1()
        .border_color(p.hairline)
        .child(tone_pill(s.method.clone(), p.tone(method_tone(&s.method))))
        .child(tone_pill(status, p.tone(state.tone())))
        .child({
            // URL не влезает — целиком в подсказке; клик копирует.
            let (full, click_url) = (url.clone(), url.clone());
            div()
                .id("inspector-url")
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .cursor_pointer()
                .text_size(px(TEXT))
                .font_weight(FontWeight::MEDIUM)
                .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                .on_click(move |_, window, cx| copy_text(&click_url, "URL", window, cx))
                .child(url)
        })
        .child(
            div()
                .flex_none()
                .text_size(px(TEXT_SM))
                .text_color(p.text_tertiary)
                .child(meta),
        )
        .child(
            ToolbarButton::new("inspector-copy-url")
                .icon(Icon::new(IconName::Copy))
                .tooltip("Copy URL")
                .on_click(move |_, window, cx| copy_text(&copy_url, "URL", window, cx)),
        )
        .into_any_element()
}

/// Панель одной стороны: шапка (название + вкладки) и содержимое вкладки.
fn side_pane(
    store: &Entity<DetailStore>,
    side: Side,
    tab: InspectorTab,
    ctx: ExchangeContext,
    p: &Palette,
) -> AnyElement {
    // Вкладки WebSocket — только у состоявшегося апгрейда (101); отказ
    // origin показывается как обычный ответ (с телом).
    let websocket = ctx.detail.summary.protocol
        == domain::value_objects::http_vo::Protocol::WebSocket
        && ctx
            .detail
            .response
            .as_ref()
            .is_some_and(|r| r.status == 101);
    let tabs = InspectorTab::for_side(side, websocket);
    // Выбранной вкладки у этого обмена нет (Messages у HTTP, Body у
    // WebSocket) — вкладка по умолчанию стороны, иначе первая.
    let tab = if tabs.contains(&tab) {
        tab
    } else if tabs.contains(&InspectorTab::default_for(side)) {
        InspectorTab::default_for(side)
    } else {
        tabs[0]
    };
    let selected = tabs.iter().position(|t| *t == tab).unwrap_or(0);
    let store = store.clone();
    let switcher = SegmentedControl::new(
        match side {
            Side::Request => "inspector-request-tabs",
            Side::Response => "inspector-response-tabs",
        },
        tabs.iter().map(|t| t.label()),
    )
    .selected(selected)
    .on_select(move |ix, _, cx| {
        if let Some(tab) = tabs.get(ix).copied() {
            DetailStore::set_tab(&store, side, tab, cx);
        }
    });
    let ctx = ctx.on(side);
    let content = match tab {
        InspectorTab::Overview => OverviewTab::new(ctx).into_any_element(),
        InspectorTab::Headers => HeadersTab::new(ctx).into_any_element(),
        InspectorTab::Query => QueryTab::new(ctx).into_any_element(),
        InspectorTab::Body => BodyTab::new(ctx).into_any_element(),
        InspectorTab::Cookies => CookiesTab::new(ctx).into_any_element(),
        InspectorTab::Raw => RawTab::new(ctx).into_any_element(),
        InspectorTab::Hex => HexTab::new(ctx).into_any_element(),
        InspectorTab::Messages => {
            crate::ui::inspector::messages::MessagesTab::new(ctx).into_any_element()
        }
    };
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_w(px(0.))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(10.))
                .h(px(HEADER_HEIGHT))
                .px(px(12.))
                .border_b_1()
                .border_color(p.hairline)
                .child(
                    div()
                        .flex_none()
                        .text_size(px(TEXT_SM))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.text_secondary)
                        .child(side.label()),
                )
                .child(div().min_w(px(0.)).overflow_hidden().child(switcher)),
        )
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .overflow_hidden()
                .child(content),
        )
        .into_any_element()
}
