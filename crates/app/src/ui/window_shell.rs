//! Каркас вспомогательных окон (Tools, Settings): размытое окно с
//! прозрачным тайтл-баром, список разделов слева и «стеклянная» панель
//! содержимого справа.

use gpui_kit::component::{Root, TitleBar};
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Bounds, Div, Entity, FontWeight, IntoElement,
    ParentElement, Styled, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, div, point, px, size,
};

use crate::ui::theme::components::glass_panel;
use crate::ui::theme::glass;
use crate::ui::theme::metrics::{
    TEXT, TITLE_BAR_HEIGHT, TRAFFIC_LIGHTS_INSET, TRAFFIC_LIGHTS_X, TRAFFIC_LIGHTS_Y,
};

/// Ширина списка разделов.
const NAV_WIDTH: f32 = 210.;

/// Параметры окна: размытый фон, тайтл-бар рисует приложение.
pub fn window_options(title: &str, width: f32, height: f32, cx: &App) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.to_string().into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(TRAFFIC_LIGHTS_X), px(TRAFFIC_LIGHTS_Y))),
        }),
        app_owns_titlebar_drag: true,
        window_background: WindowBackgroundAppearance::Blurred,
        window_min_size: Some(size(px(640.), px(420.))),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(width), px(height)),
            cx,
        ))),
        ..Default::default()
    }
}

/// Корень окна без собственного фона (иначе размытие не просвечивает).
pub fn transparent_root(view: AnyView, window: &mut Window, cx: &mut App) -> Entity<Root> {
    cx.new(|cx| gpui_kit::Styled::bg(Root::new(view, window, cx), gpui_kit::transparent_black()))
}

/// Тайтл-бар окна: заголовок после «светофоров», drag и двойной клик.
pub fn title_bar(title: impl Into<gpui_kit::SharedString>, cx: &App) -> impl IntoElement {
    let p = glass(cx);
    TitleBar::new()
        .h(px(TITLE_BAR_HEIGHT))
        .pl(px(TRAFFIC_LIGHTS_INSET))
        .bg(gpui_kit::transparent_black())
        .border_color(gpui_kit::transparent_black())
        .child(
            div()
                .text_size(px(TEXT))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.text)
                .child(title.into()),
        )
}

/// Окно: тайтл-бар, список разделов, панель содержимого.
pub fn shell(
    title: impl Into<gpui_kit::SharedString>,
    nav: Vec<AnyElement>,
    content: AnyElement,
    cx: &App,
) -> Div {
    let p = glass(cx);
    div()
        .flex()
        .flex_col()
        .size_full()
        .bg(p.window)
        .text_color(p.text)
        .child(title_bar(title, cx))
        .child(
            div()
                .flex()
                .flex_1()
                .min_h(px(0.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .w(px(NAV_WIDTH))
                        .gap(px(2.))
                        .px(px(8.))
                        .children(nav),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .pr(px(8.))
                        .pb(px(8.))
                        .child(glass_panel(&p).size_full().child(content)),
                ),
        )
}
