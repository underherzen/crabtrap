//! SegmentedControl (как NSSegmentedControl): одна выбранная позиция,
//! выбранный сегмент «приподнят» над подложкой.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div, px,
};

use super::{child_id, soft_shadow};

use crate::ui::theme::glass;
use crate::ui::theme::metrics::{RADIUS, RADIUS_SM, TEXT_SM};

type OnSelect = Rc<dyn Fn(usize, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct SegmentedControl {
    id: ElementId,
    labels: Vec<SharedString>,
    selected: usize,
    on_select: Option<OnSelect>,
}

impl SegmentedControl {
    pub fn new(
        id: impl Into<ElementId>,
        labels: impl IntoIterator<Item = impl Into<SharedString>>,
    ) -> Self {
        Self {
            id: id.into(),
            labels: labels.into_iter().map(Into::into).collect(),
            selected: 0,
            on_select: None,
        }
    }

    pub fn selected(mut self, ix: usize) -> Self {
        self.selected = ix;
        self
    }

    pub fn on_select(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for SegmentedControl {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let segments = self.labels.into_iter().enumerate().map(|(ix, label)| {
            let selected = ix == self.selected;
            let on_select = self.on_select.clone();
            div()
                .id(child_id(&self.id, ix))
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .px(px(10.))
                .rounded(px(RADIUS_SM))
                .text_size(px(TEXT_SM))
                .when(selected, |this| {
                    this.bg(p.raised)
                        .text_color(p.text)
                        .font_weight(FontWeight::MEDIUM)
                        .shadow(soft_shadow(&p, 2.))
                })
                .when(!selected, |this| {
                    this.text_color(p.text_secondary)
                        .hover(|style| style.text_color(p.text))
                })
                .when_some(on_select, |this, on_select| {
                    this.on_click(move |_, window, cx| on_select(ix, window, cx))
                })
                .child(label)
        });
        div()
            .id(self.id.clone())
            .flex()
            .flex_row()
            .flex_none()
            .h(px(24.))
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(RADIUS))
            .bg(p.control)
            .children(segments)
    }
}
