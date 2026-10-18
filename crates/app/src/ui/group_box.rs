//! Секция-карточка инспектора и форм: подпись над скруглённой подложкой.
//! (Бывший XP group box — тот же API, новый вид.)

use gpui_kit::{
    AnyElement, App, FontWeight, Hsla, IntoElement, ParentElement, RenderOnce, Styled, Window, div,
    px,
};

use crate::ui::theme::glass;
use crate::ui::theme::metrics::{RADIUS_LG, TEXT_XS};

#[derive(IntoElement)]
pub struct GroupBox {
    title: String,
    border: Hsla,
    title_color: Hsla,
    children: Vec<AnyElement>,
}

impl GroupBox {
    pub fn new(title: impl Into<String>, border: Hsla, title_color: Hsla) -> Self {
        Self {
            title: title.into(),
            border,
            title_color,
            children: Vec::new(),
        }
    }
}

impl ParentElement for GroupBox {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for GroupBox {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let title = div()
            .px(px(4.))
            .text_size(px(TEXT_XS))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(self.title_color)
            .child(self.title);
        let body = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(RADIUS_LG))
            .bg(p.header)
            .border_1()
            .border_color(self.border)
            .children(self.children);
        // flex_none: внутри прокручиваемой колонки карточка не сжимается
        // (иначе содержимое обрезается и прокрутке нечего крутить).
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.))
            .child(title)
            .child(body)
    }
}
