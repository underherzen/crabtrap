//! Палитра Liquid Glass: светлая и тёмная.
//!
//! Окно размыто ЗА собой (NSVisualEffectView, см. `main`), поэтому
//! поверхности — полупрозрачные слои поверх размытого рабочего стола:
//! `window` (общая подложка) → `sidebar`/`content` → `header`/`stripe`.
//! Поповеры и диалоги почти непрозрачны (`raised`): размытия внутри окна
//! GPUI не умеет, а текст поверх «голого» стекла нечитаем.
//!
//! Цвета текста подобраны под контраст ≥ 4.5:1 на типовых подложках
//! (светлая тема — белый и средне-серый рабочий стол, тёмная — чёрный и
//! средне-серый); это проверяют тесты внизу.

use gpui_kit::{Hsla, rgb};

/// Цвет из `0xRRGGBB` с непрозрачностью `alpha`.
fn c(hex: u32, alpha: f32) -> Hsla {
    Hsla::from(rgb(hex)).opacity(alpha)
}

/// Цветовой тон: текст/иконка (`fg`) и подложка плашки (`bg`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tone {
    pub fg: Hsla,
    pub bg: Hsla,
}

/// Именованные тона (семантика — в [`super::semantic`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneName {
    Blue,
    Green,
    Orange,
    Red,
    Purple,
    Teal,
    Gray,
    Yellow,
    Pink,
}

impl ToneName {
    pub const ALL: [ToneName; 9] = [
        ToneName::Blue,
        ToneName::Green,
        ToneName::Orange,
        ToneName::Red,
        ToneName::Purple,
        ToneName::Teal,
        ToneName::Gray,
        ToneName::Yellow,
        ToneName::Pink,
    ];
}

/// Семантические цвета интерфейса.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    // --- Поверхности (снизу вверх) -----------------------------------------
    /// Общая подложка окна поверх размытия.
    pub window: Hsla,
    /// Боковая панель — плавающая панель поверх `window`: светлее подложки
    /// (как сайдбар macOS), но прозрачнее контента.
    pub sidebar: Hsla,
    /// Таблица, инспектор, формы (поверх `window`).
    pub content: Hsla,
    /// Шапки таблиц/секций (поверх `content`).
    pub header: Hsla,
    /// Поповеры, меню, диалоги — почти непрозрачные.
    pub raised: Hsla,
    /// Разделители и рамки.
    pub hairline: Hsla,
    /// Светлая кромка сверху у «стеклянных» панелей.
    pub edge: Hsla,
    pub shadow: Hsla,
    // --- Контролы ----------------------------------------------------------
    pub control: Hsla,
    pub control_hover: Hsla,
    pub control_active: Hsla,
    /// Полоса зебры и подсветка строки под курсором.
    pub stripe: Hsla,
    pub row_hover: Hsla,
    /// Выделенная строка (фокус в таблице) и текст на ней.
    pub selection: Hsla,
    pub selection_fg: Hsla,
    /// Выделение без фокуса / выбранный пункт сайдбара.
    pub selection_soft: Hsla,
    // --- Текст -------------------------------------------------------------
    pub text: Hsla,
    pub text_secondary: Hsla,
    /// Подсказки и неважное (плейсхолдеры, «…»): контраст не гарантирован.
    pub text_tertiary: Hsla,
    // --- Акцент ------------------------------------------------------------
    /// Заливка акцентных кнопок/переключателей.
    pub accent: Hsla,
    pub accent_fg: Hsla,
    /// Акцент как цвет текста/иконки на `content`.
    pub accent_text: Hsla,
    tones: [Tone; 9],
}

impl Palette {
    pub fn light() -> Self {
        let tone = |hex: u32| Tone {
            fg: c(hex, 1.),
            bg: c(hex, 0.11),
        };
        Self {
            dark: false,
            window: c(0xF2F2F5, 0.72),
            sidebar: c(0xFFFFFF, 0.42),
            content: c(0xFFFFFF, 0.88),
            header: c(0x000000, 0.03),
            raised: c(0xFCFCFD, 0.98),
            hairline: c(0x000000, 0.10),
            edge: c(0xFFFFFF, 0.75),
            shadow: c(0x000000, 0.14),
            control: c(0x000000, 0.05),
            control_hover: c(0x000000, 0.08),
            control_active: c(0x000000, 0.13),
            stripe: c(0x000000, 0.025),
            row_hover: c(0x000000, 0.045),
            selection: c(0x0A62D6, 1.),
            selection_fg: c(0xFFFFFF, 1.),
            selection_soft: c(0x000000, 0.08),
            text: c(0x1D1D1F, 1.),
            text_secondary: c(0x56565B, 1.),
            text_tertiary: c(0x8A8A8F, 1.),
            accent: c(0x0A62D6, 1.),
            accent_fg: c(0xFFFFFF, 1.),
            accent_text: c(0x0A5CC8, 1.),
            tones: [
                tone(0x0A5CC8), // blue
                tone(0x1A6E30), // green
                tone(0xA04800), // orange
                tone(0xB8241A), // red
                tone(0x6E36B8), // purple
                tone(0x0C6A85), // teal
                tone(0x5A5A5F), // gray
                tone(0x7A5600), // yellow
                tone(0xA82C64), // pink
            ],
        }
    }

    pub fn dark() -> Self {
        let tone = |hex: u32| Tone {
            fg: c(hex, 1.),
            bg: c(hex, 0.18),
        };
        Self {
            dark: true,
            window: c(0x1C1C1E, 0.72),
            sidebar: c(0x000000, 0.18),
            content: c(0x1E1E20, 0.90),
            header: c(0xFFFFFF, 0.04),
            raised: c(0x2A2A2D, 0.98),
            hairline: c(0xFFFFFF, 0.10),
            edge: c(0xFFFFFF, 0.10),
            shadow: c(0x000000, 0.45),
            control: c(0xFFFFFF, 0.07),
            control_hover: c(0xFFFFFF, 0.11),
            control_active: c(0xFFFFFF, 0.16),
            stripe: c(0xFFFFFF, 0.025),
            row_hover: c(0xFFFFFF, 0.05),
            selection: c(0x0A62D6, 1.),
            selection_fg: c(0xFFFFFF, 1.),
            selection_soft: c(0xFFFFFF, 0.10),
            text: c(0xF2F2F7, 1.),
            text_secondary: c(0xA8A8AE, 1.),
            text_tertiary: c(0x6E6E73, 1.),
            accent: c(0x0A62D6, 1.),
            accent_fg: c(0xFFFFFF, 1.),
            accent_text: c(0x5AA2FF, 1.),
            tones: [
                tone(0x5AA2FF), // blue
                tone(0x3DD263), // green
                tone(0xFF9F43), // orange
                tone(0xFF7B72), // red
                tone(0xBF8CFF), // purple
                tone(0x4FD1E6), // teal
                tone(0xA1A1A6), // gray
                tone(0xFFD24D), // yellow
                tone(0xFF7AB6), // pink
            ],
        }
    }

    pub fn for_dark(dark: bool) -> Self {
        if dark { Self::dark() } else { Self::light() }
    }

    pub fn tone(&self, name: ToneName) -> Tone {
        let ix = ToneName::ALL.iter().position(|n| *n == name).unwrap_or(0);
        self.tones[ix]
    }

    /// Подложки рабочего стола, на которых проверяется контраст.
    #[cfg(test)]
    pub fn reference_backdrops(&self) -> [Hsla; 2] {
        let mid = c(0x808080, 1.);
        if self.dark {
            [c(0x000000, 1.), mid]
        } else {
            [c(0xFFFFFF, 1.), mid]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::contrast::{AA_LARGE, AA_TEXT, contrast, stack};
    use super::*;

    fn palettes() -> [Palette; 2] {
        [Palette::light(), Palette::dark()]
    }

    /// Минимальный контраст `fg` на стопке `layers` по всем подложкам.
    fn worst(p: &Palette, fg: Hsla, layers: &[Hsla]) -> f32 {
        p.reference_backdrops()
            .iter()
            .map(|b| contrast(fg, stack(*b, layers)))
            .fold(f32::INFINITY, f32::min)
    }

    #[test]
    fn body_text_is_readable_on_every_surface() {
        for p in palettes() {
            let surfaces: [&[Hsla]; 4] = [
                &[p.window],
                &[p.window, p.sidebar],
                &[p.window, p.content],
                &[p.window, p.content, p.header],
            ];
            for layers in surfaces {
                for (name, fg) in [("text", p.text), ("secondary", p.text_secondary)] {
                    let ratio = worst(&p, fg, layers);
                    assert!(
                        ratio >= AA_TEXT,
                        "dark={} {name}: {ratio:.2} on {} layers",
                        p.dark,
                        layers.len()
                    );
                }
            }
        }
    }

    #[test]
    fn striped_and_hovered_rows_keep_contrast() {
        for p in palettes() {
            for row in [p.stripe, p.row_hover] {
                let ratio = worst(&p, p.text_secondary, &[p.window, p.content, row]);
                assert!(ratio >= AA_TEXT, "dark={} row: {ratio:.2}", p.dark);
            }
        }
    }

    #[test]
    fn selection_text_is_readable() {
        for p in palettes() {
            let ratio = contrast(p.selection_fg, p.selection);
            assert!(ratio >= AA_TEXT, "dark={} selection: {ratio:.2}", p.dark);
            let ratio = contrast(p.accent_fg, p.accent);
            assert!(ratio >= AA_TEXT, "dark={} accent: {ratio:.2}", p.dark);
        }
    }

    #[test]
    fn tones_are_readable_as_text_and_inside_pills() {
        for p in palettes() {
            for name in ToneName::ALL {
                let t = p.tone(name);
                let plain = worst(&p, t.fg, &[p.window, p.content]);
                assert!(
                    plain >= AA_TEXT,
                    "dark={} {name:?} text: {plain:.2}",
                    p.dark
                );
                let pill = worst(&p, t.fg, &[p.window, p.content, t.bg]);
                assert!(pill >= AA_TEXT, "dark={} {name:?} pill: {pill:.2}", p.dark);
            }
            let accent = worst(&p, p.accent_text, &[p.window, p.content]);
            assert!(
                accent >= AA_TEXT,
                "dark={} accent text: {accent:.2}",
                p.dark
            );
        }
    }

    #[test]
    fn hairlines_are_visible_but_subtle() {
        for p in palettes() {
            let base = stack(p.reference_backdrops()[0], &[p.window, p.content]);
            let line = stack(base, &[p.hairline]);
            let ratio = contrast(line, base);
            assert!(
                ratio > 1.1 && ratio < AA_LARGE,
                "dark={} hairline: {ratio:.2}",
                p.dark
            );
        }
    }

    #[test]
    fn popovers_are_nearly_opaque() {
        for p in palettes() {
            assert!(p.raised.a >= 0.95, "dark={}", p.dark);
            assert!(
                p.content.a > p.sidebar.a,
                "content should be denser than sidebar"
            );
        }
    }
}
