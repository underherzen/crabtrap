//! WCAG 2.x: относительная яркость, контраст и наложение полупрозрачного
//! цвета на подложку. Нужны тестам палитры: «стекло» полупрозрачно, и
//! контраст текста считается по цвету, который реально получится на экране.

use gpui_kit::{Hsla, Rgba};

/// Порог AA для обычного текста.
pub const AA_TEXT: f32 = 4.5;
/// Порог AA для крупного текста и не-текстовых элементов (иконки, рамки).
pub const AA_LARGE: f32 = 3.0;

fn channel(c: f32) -> f32 {
    if c <= 0.039_28 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Относительная яркость (альфа игнорируется — сначала [`over`]).
pub fn luminance(color: Hsla) -> f32 {
    let Rgba { r, g, b, .. } = color.to_rgb();
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// Контраст двух непрозрачных цветов (1.0 … 21.0).
pub fn contrast(a: Hsla, b: Hsla) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// `top` поверх непрозрачного `bottom` (source-over) — непрозрачный итог.
pub fn over(top: Hsla, bottom: Hsla) -> Hsla {
    let (t, b) = (top.to_rgb(), bottom.to_rgb());
    let a = t.a;
    Rgba {
        r: t.r * a + b.r * (1. - a),
        g: t.g * a + b.g * (1. - a),
        b: t.b * a + b.b * (1. - a),
        a: 1.,
    }
    .into()
}

/// Стопка слоёв снизу вверх поверх непрозрачной подложки.
pub fn stack(backdrop: Hsla, layers: &[Hsla]) -> Hsla {
    layers.iter().fold(backdrop, |acc, layer| over(*layer, acc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{black, rgb, white};

    #[test]
    fn black_on_white_is_21() {
        assert!((contrast(black(), white()) - 21.).abs() < 0.01);
    }

    #[test]
    fn contrast_is_symmetric_and_at_least_one() {
        let (a, b): (Hsla, Hsla) = (rgb(0x336699).into(), rgb(0xEEEEEE).into());
        assert!((contrast(a, b) - contrast(b, a)).abs() < 1e-6);
        assert!((contrast(a, a) - 1.).abs() < 1e-6);
    }

    #[test]
    fn known_pair_matches_reference() {
        // #767676 на белом — классическая граница AA (4.54:1).
        let gray: Hsla = rgb(0x767676).into();
        assert!((contrast(gray, white()) - 4.54).abs() < 0.02);
    }

    #[test]
    fn over_blends_by_alpha() {
        let half_black = black().opacity(0.5);
        let mixed = over(half_black, white()).to_rgb();
        assert!((mixed.r - 0.5).abs() < 0.01);
        assert!((mixed.a - 1.).abs() < 1e-6);
        assert_eq!(over(white(), black()).to_rgb().r, 1.);
    }

    #[test]
    fn stack_applies_layers_bottom_up() {
        let result = stack(black(), &[white().opacity(0.5), black().opacity(0.5)]);
        assert!((result.to_rgb().r - 0.25).abs() < 0.01);
    }
}
