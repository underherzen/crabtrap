//! «Стеклянная» панель: полупрозрачная поверхность поверх размытого окна,
//! тонкая рамка, светлая кромка сверху и мягкая тень.

use gpui_kit::{BoxShadow, Div, Hsla, ParentElement, Styled, div, point, px};

use crate::ui::theme::Palette;
use crate::ui::theme::metrics::RADIUS_LG;

/// Панель контента (таблица, инспектор, формы).
pub fn glass_panel(p: &Palette) -> Div {
    glass_surface(p, p.content)
}

/// Боковая панель: та же плавающая форма, но прозрачнее контента.
pub fn glass_sidebar(p: &Palette) -> Div {
    glass_surface(p, p.sidebar)
}

fn glass_surface(p: &Palette, fill: Hsla) -> Div {
    div()
        .bg(fill)
        .relative()
        .border_1()
        .border_color(p.hairline)
        .rounded(px(RADIUS_LG))
        .shadow(soft_shadow(p, 3.))
        .overflow_hidden()
        // Светлая кромка сверху: «стекло» ловит свет.
        .child(
            div()
                .absolute()
                .top_0()
                .left(px(RADIUS_LG))
                .right(px(RADIUS_LG))
                .h(px(1.))
                .bg(p.edge),
        )
}

/// Мягкая тень под приподнятым элементом.
pub fn soft_shadow(p: &Palette, blur: f32) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: p.shadow,
        offset: point(px(0.), px(1.)),
        blur_radius: px(blur),
        spread_radius: px(0.),
        inset: false,
    }]
}
