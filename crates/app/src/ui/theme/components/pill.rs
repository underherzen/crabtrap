//! Плашки: метод, статус, тег, тип содержимого — и точка состояния.

use gpui_kit::{Div, FontWeight, Hsla, ParentElement, SharedString, Styled, div, px};

use crate::ui::theme::Tone;
use crate::ui::theme::metrics::{PILL_HEIGHT, TEXT_XS};

/// Скруглённая плашка с текстом цвета `fg` на подложке `bg`.
pub fn pill(label: impl Into<SharedString>, fg: Hsla, bg: Hsla) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(PILL_HEIGHT))
        .px(px(6.))
        .rounded(px(PILL_HEIGHT / 2.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(TEXT_XS))
        .font_weight(FontWeight::SEMIBOLD)
        .line_height(px(PILL_HEIGHT))
        .child(label.into())
}

/// Плашка в цветах тона.
pub fn tone_pill(label: impl Into<SharedString>, tone: Tone) -> Div {
    pill(label, tone.fg, tone.bg)
}

/// Точка состояния (строка таблицы, плашка прокси).
pub fn dot(color: Hsla, size: f32) -> Div {
    div().flex_none().size(px(size)).rounded_full().bg(color)
}
