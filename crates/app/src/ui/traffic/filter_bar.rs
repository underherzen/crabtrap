//! Полоса фильтра над таблицей: выбор боковой панели (чип с ✕), чипы
//! быстрого фильтра (All · HTTP · HTTPS · WebSocket | JSON · Form · …) и
//! поле фильтра (`host:`, `status:`, `method:`, `type:`, `re:`) с ошибкой
//! битого `re:`.

use application::use_cases::inspection::parse_filter_query;
use gpui_kit::AppContext as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Entity, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement as _, Styled, Window, div, px,
};

use crate::ui::theme::components::FilterChip;
use crate::ui::theme::icons::AppIcon;
use crate::ui::theme::metrics::{CHIP_HEIGHT, TEXT_SM, TEXT_XS};
use crate::ui::theme::semantic::ContentKind;
use crate::ui::theme::{Palette, ToneName, glass};
use crate::ui::traffic::quick_filter::{ProtocolChip, QuickFilter};
use crate::ui::traffic::scope::Scope;
use crate::ui::traffic::store::TrafficStore;

// ⌘L — фокус поля фильтра (ловится на корневом div окна).
gpui_kit::actions!(crabtrap, [FocusFilter]);

/// Подпись чипа выбора боковой панели (`None` — «All», чипа нет).
pub fn scope_label(scope: &Scope) -> Option<String> {
    match scope {
        Scope::All => None,
        Scope::Pinned => Some("Pinned domains".into()),
        Scope::Host(host) => Some(host.clone()),
        Scope::Path { host, prefix } => Some(format!("{host}{prefix}")),
        Scope::App(name) => Some(name.clone()),
    }
}

/// Полоса фильтра (RenderOnce над TrafficStore + полем).
#[derive(IntoElement)]
pub struct FilterBar {
    traffic: Entity<TrafficStore>,
    filter_input: Entity<InputState>,
}

impl FilterBar {
    pub fn new(traffic: Entity<TrafficStore>, filter_input: Entity<InputState>) -> Self {
        Self {
            traffic,
            filter_input,
        }
    }
}

fn set_quick(traffic: &Entity<TrafficStore>, quick: QuickFilter, cx: &mut App) {
    traffic.update(cx, |store, cx| {
        store.set_quick(quick);
        cx.notify();
    });
}

fn scope_chip(traffic: &Entity<TrafficStore>, label: String, p: &Palette) -> AnyElement {
    let traffic = traffic.clone();
    div()
        .id("filter-scope")
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .h(px(CHIP_HEIGHT))
        .pl(px(9.))
        .pr(px(5.))
        .rounded(px(CHIP_HEIGHT / 2.))
        .bg(p.selection_soft)
        .text_size(px(TEXT_SM))
        .text_color(p.text)
        .child(
            Icon::new(AppIcon::Globe)
                .xsmall()
                .text_color(p.text_secondary),
        )
        .child(label)
        .child(
            div()
                .id("filter-scope-clear")
                .rounded_full()
                .hover(|style| style.bg(p.control_active))
                .child(
                    Icon::new(IconName::Close)
                        .xsmall()
                        .text_color(p.text_secondary),
                )
                .on_click(move |_, _, cx| {
                    traffic.update(cx, |store, cx| {
                        store.set_scope(Scope::All);
                        cx.notify();
                    })
                }),
        )
        .into_any_element()
}

fn chips(traffic: &Entity<TrafficStore>, quick: &QuickFilter, p: &Palette) -> Vec<AnyElement> {
    let all = {
        let traffic = traffic.clone();
        FilterChip::new("chip-all", "All")
            .selected(quick.is_empty())
            .on_click(move |_, _, cx| set_quick(&traffic, QuickFilter::default(), cx))
            .into_any_element()
    };
    let protocols = ProtocolChip::ALL.into_iter().map(|chip| {
        let (traffic, next) = (traffic.clone(), quick.toggle_protocol(chip));
        FilterChip::new(("chip-protocol", chip as usize), chip.label())
            .selected(quick.protocol == Some(chip))
            .on_click(move |_, _, cx| set_quick(&traffic, next.clone(), cx))
            .into_any_element()
    });
    let separator = div()
        .flex_none()
        .w(px(1.))
        .h(px(14.))
        .mx(px(4.))
        .bg(p.hairline)
        .into_any_element();
    let kinds = ContentKind::ALL.into_iter().enumerate().map(|(ix, kind)| {
        let (traffic, next) = (traffic.clone(), quick.toggle_kind(kind));
        FilterChip::new(("chip-kind", ix), kind.label())
            .selected(quick.has_kind(kind))
            .tone(p.tone(kind.tone()))
            .on_click(move |_, _, cx| set_quick(&traffic, next.clone(), cx))
            .into_any_element()
    });
    std::iter::once(all)
        .chain(protocols)
        .chain(std::iter::once(separator))
        .chain(kinds)
        .collect()
}

impl RenderOnce for FilterBar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let store = self.traffic.read(cx);
        // Битый `re:` не прячет строки, а подсвечивается рядом с полем.
        let filter_error = store.filter_error.clone();
        let scope = scope_label(&store.scope);
        let chips = chips(&self.traffic, &store.quick, &p);
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(6.))
            .border_b_1()
            .border_color(p.hairline)
            .when_some(scope, |this, label| {
                this.child(scope_chip(&self.traffic, label, &p))
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .items_center()
                    .gap(px(2.))
                    .children(chips),
            )
            .when_some(filter_error, |el, err| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(TEXT_XS))
                        .text_color(p.tone(ToneName::Red).fg)
                        .child(format!("⚠ {err}")),
                )
            })
            .child(
                Input::new(&self.filter_input)
                    .h(px(24.))
                    .w(px(260.))
                    .small()
                    .prefix(
                        Icon::new(AppIcon::Funnel)
                            .xsmall()
                            .text_color(p.text_tertiary),
                    )
                    .cleanable(true),
            )
    }
}

/// Подписка фильтра: каждое изменение Input → `parse_filter_query` →
/// `TrafficStore::set_filter` (синтаксис: слова, `host:`, `status:`,
/// `method:`, `type:`, `re:` — см. filter_parser в application).
/// Вызывается один раз при сборке окна (InputState::set_value не эмитит
/// Change, поэтому держим единственный источник правды — сам Input).
pub fn bind_filter_input(input: &Entity<InputState>, traffic: &Entity<TrafficStore>, cx: &mut App) {
    let traffic = traffic.clone();
    cx.subscribe(
        input,
        move |input, _: &gpui_kit::component::input::InputEvent, cx| {
            let value = input.read(cx).value().to_string();
            traffic.update(cx, |state, cx| {
                state.set_filter(parse_filter_query(&value));
                cx.notify();
            });
        },
    )
    .detach();
}

/// Сборка InputState фильтра (нужен `&mut Window`).
pub fn build_filter_input(window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx).placeholder("Filter: text host: status:4xx method: type: re:")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_chip_label() {
        assert_eq!(scope_label(&Scope::All), None);
        assert_eq!(
            scope_label(&Scope::Host("a.test".into())).as_deref(),
            Some("a.test")
        );
        assert_eq!(
            scope_label(&Scope::Path {
                host: "a.test".into(),
                prefix: "/v1".into()
            })
            .as_deref(),
            Some("a.test/v1")
        );
    }
}
