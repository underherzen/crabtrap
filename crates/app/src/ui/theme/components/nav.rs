//! Пункт навигации в боковом списке окон Tools и Settings: иконка,
//! подпись, необязательная плашка справа («On»), выделение.

use std::rc::Rc;

use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div, px,
};

use super::pill::tone_pill;
use crate::ui::theme::metrics::{RADIUS, TEXT};
use crate::ui::theme::{Tone, glass};

type OnClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct NavItem {
    id: ElementId,
    icon: Icon,
    label: SharedString,
    badge: Option<(SharedString, Tone)>,
    selected: bool,
    on_click: Option<OnClick>,
}

impl NavItem {
    pub fn new(
        id: impl Into<ElementId>,
        icon: impl Into<Icon>,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon: icon.into(),
            label: label.into(),
            badge: None,
            selected: false,
            on_click: None,
        }
    }

    pub fn badge(mut self, label: impl Into<SharedString>, tone: Tone) -> Self {
        self.badge = Some((label.into(), tone));
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for NavItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        div()
            .id(self.id)
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(30.))
            .px(px(10.))
            .rounded(px(RADIUS))
            .text_size(px(TEXT))
            .text_color(p.text)
            .when(self.selected, |this| this.bg(p.selection_soft))
            .when(!self.selected, |this| {
                this.hover(|style| style.bg(p.row_hover))
            })
            .child(self.icon.small().text_color(if self.selected {
                p.accent_text
            } else {
                p.text_secondary
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(self.label),
            )
            .when_some(self.badge, |this, (label, tone)| {
                this.child(tone_pill(label, tone))
            })
            .when_some(self.on_click, |this, on_click| {
                this.on_click(move |event, window, cx| on_click(event, window, cx))
            })
    }
}
