//! Плашка состояния в центре тулбара (как у Proxyman/Xcode): точка
//! состояния и сегменты «Proxy · 127.0.0.1:8080 · SSL · System Proxy».
//! Сегмент может быть кликабельным тумблером.

use std::rc::Rc;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, ElementId, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    px,
};

use super::child_id;
use super::pill::dot;
use crate::ui::theme::glass;
use crate::ui::theme::metrics::{CONTROL_HEIGHT, RADIUS_SM, TEXT_SM};

type OnClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Сегмент плашки.
pub struct StatusSegment {
    label: SharedString,
    /// Включённый тумблер — цвет акцента, выключенный — приглушённый.
    on: Option<bool>,
    /// Цвет текста вместо обычного (ошибка).
    color: Option<Hsla>,
    tooltip: Option<SharedString>,
    on_click: Option<OnClick>,
}

impl StatusSegment {
    pub fn text(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            on: None,
            color: None,
            tooltip: None,
            on_click: None,
        }
    }

    /// Тумблер: подпись подсвечена, когда включён.
    pub fn toggle(label: impl Into<SharedString>, on: bool) -> Self {
        Self {
            on: Some(on),
            ..Self::text(label)
        }
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
}

#[derive(IntoElement)]
pub struct StatusPill {
    id: ElementId,
    dot: Hsla,
    segments: Vec<StatusSegment>,
}

impl StatusPill {
    pub fn new(id: impl Into<ElementId>, dot: Hsla) -> Self {
        Self {
            id: id.into(),
            dot,
            segments: Vec::new(),
        }
    }

    pub fn segment(mut self, segment: StatusSegment) -> Self {
        self.segments.push(segment);
        self
    }
}

impl RenderOnce for StatusPill {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let count = self.segments.len();
        let segments = self.segments.into_iter().enumerate().flat_map(|(ix, seg)| {
            let color = seg.color.unwrap_or(match seg.on {
                Some(true) => p.accent_text,
                Some(false) => p.text_tertiary,
                None => p.text_secondary,
            });
            let clickable = seg.on_click.is_some();
            let segment = div()
                .id(child_id(&self.id, ix))
                .flex()
                .items_center()
                .h(px(20.))
                .px(px(6.))
                .rounded(px(RADIUS_SM))
                .text_color(color)
                .when(ix == 0, |this| this.text_color(seg.color.unwrap_or(p.text)))
                .when(clickable, |this| {
                    // Не отдавать mouse-down TitleBar'у (см. ToolbarButton).
                    this.hover(|style| style.bg(p.control_hover))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                })
                .when_some(seg.tooltip, |this, tooltip| {
                    this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                })
                .when_some(seg.on_click, |this, on_click| {
                    this.on_click(move |event, window, cx| {
                        cx.stop_propagation();
                        on_click(event, window, cx)
                    })
                })
                .child(seg.label);
            let separator = (ix + 1 < count).then(|| {
                div()
                    .flex_none()
                    .w(px(1.))
                    .h(px(12.))
                    .bg(p.hairline)
                    .into_any_element()
            });
            std::iter::once(segment.into_any_element()).chain(separator)
        });
        div()
            .id(self.id.clone())
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .h(px(CONTROL_HEIGHT))
            .pl(px(10.))
            .pr(px(4.))
            .rounded(px(CONTROL_HEIGHT / 2.))
            .bg(p.control)
            .border_1()
            .border_color(p.hairline)
            .text_size(px(TEXT_SM))
            .child(dot(self.dot, 7.).mr(px(4.)))
            .children(segments)
    }
}
