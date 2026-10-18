//! Чип фильтра (полоса над таблицей: All · HTTP · HTTPS · WS · JSON · …):
//! тумблер с подписью и необязательным счётчиком.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, Window, div, px,
};

use crate::ui::theme::metrics::{CHIP_HEIGHT, TEXT_SM};
use crate::ui::theme::{Tone, glass};

type OnClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct FilterChip {
    id: ElementId,
    label: SharedString,
    selected: bool,
    /// Тон выбранного чипа (по умолчанию — акцент).
    tone: Option<Tone>,
    on_click: Option<OnClick>,
}

impl FilterChip {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            selected: false,
            tone: None,
            on_click: None,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for FilterChip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let tone = self.tone.unwrap_or(Tone {
            fg: p.accent_text,
            bg: p.accent.opacity(if p.dark { 0.30 } else { 0.13 }),
        });
        div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .h(px(CHIP_HEIGHT))
            .px(px(9.))
            .rounded(px(CHIP_HEIGHT / 2.))
            .text_size(px(TEXT_SM))
            .when(self.selected, |this| {
                this.bg(tone.bg)
                    .text_color(tone.fg)
                    .font_weight(FontWeight::MEDIUM)
            })
            .when(!self.selected, |this| {
                this.text_color(p.text_secondary)
                    .hover(|style| style.bg(p.control_hover).text_color(p.text))
            })
            .when_some(self.on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            })
            .child(self.label)
    }
}
