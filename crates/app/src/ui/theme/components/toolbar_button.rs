//! Кнопка тулбара: иконка, подсказка, состояние «включено».

use std::rc::Rc;

use gpui_kit::component::{Icon, Sizable as _, tooltip::Tooltip};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, ElementId, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    px,
};

use crate::ui::theme::glass;
use crate::ui::theme::metrics::{CONTROL_HEIGHT, RADIUS};

type OnClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ToolbarButton {
    id: ElementId,
    icon: Option<Icon>,
    tooltip: Option<SharedString>,
    /// Тумблер включён: акцентная подложка.
    active: bool,
    /// Цвет иконки вместо обычного (запись — красная точка).
    icon_color: Option<Hsla>,
    on_click: Option<OnClick>,
}

impl ToolbarButton {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            tooltip: None,
            active: false,
            icon_color: None,
            on_click: None,
        }
    }

    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn icon_color(mut self, color: Hsla) -> Self {
        self.icon_color = Some(color);
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for ToolbarButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let (bg, fg) = if self.active {
            (
                p.accent.opacity(if p.dark { 0.32 } else { 0.14 }),
                p.accent_text,
            )
        } else {
            (gpui_kit::transparent_black(), p.text_secondary)
        };
        let icon_color = self.icon_color.unwrap_or(fg);
        let interactive = self.on_click.is_some();
        div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(px(CONTROL_HEIGHT))
            .min_w(px(CONTROL_HEIGHT))
            .px(px(6.))
            .rounded(px(RADIUS))
            .bg(bg)
            .text_color(fg)
            .when(interactive, |this| {
                this.hover(|style| style.bg(if self.active { bg } else { p.control_hover }))
                    .active(|style| style.bg(p.control_active))
            })
            .when_some(self.icon, |this, icon| {
                this.child(icon.small().text_color(icon_color))
            })
            .when_some(self.tooltip, |this, tooltip| {
                this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
            // Кнопка живёт в тайтл-баре: mouse-down не должен дойти до
            // TitleBar, иначе дрожание мыши между down/up заодно двигает окно.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .when_some(self.on_click.filter(|_| interactive), |this, on_click| {
                this.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    on_click(event, window, cx)
                })
            })
    }
}
