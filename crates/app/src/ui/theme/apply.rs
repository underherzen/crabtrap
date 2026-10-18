//! Проекция [`Palette`] на тему gpui-component и следование за системной
//! темой macOS.
//!
//! Базу (подсветка синтаксиса, моноширинный шрифт, производные токены)
//! даёт `Theme::change(mode)`; поверх неё переписываются цвета и размеры.
//! Компоненты gpui-component (кнопки, поля, меню, таблица) читают
//! `cx.theme()`, наши — ещё и [`super::palette`] через [`super::glass`].

use gpui_kit::component::theme::{Theme, ThemeColor, ThemeMode, ThemeTokens};
use gpui_kit::{App, Global, Hsla, Window, WindowAppearance, black, px, white};

use super::metrics;
use super::palette::{Palette, ToneName};

/// Текущая палитра (глобально, рядом с темой gpui-component).
pub struct ActivePalette(pub Palette);

impl Global for ActivePalette {}

pub fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// Цвета gpui-component из палитры (поверх базовых `base`).
pub fn theme_colors(p: &Palette, base: ThemeColor) -> ThemeColor {
    let mut c = base;
    let tone = |n: ToneName| p.tone(n);
    let solid_content = if p.dark {
        Hsla::from(gpui_kit::rgb(0x232326))
    } else {
        white()
    };
    let field_border = if p.dark {
        white().opacity(0.16)
    } else {
        black().opacity(0.16)
    };

    // Поверхности. `background` — непрозрачная подложка полей и вкладок:
    // корень окна свой фон не рисует (размытие), см. `main`.
    c.background = solid_content;
    c.foreground = p.text;
    c.border = p.hairline;
    c.window_border = p.hairline;
    c.muted = p.control;
    c.muted_foreground = p.text_secondary;
    c.popover = p.raised;
    c.popover_foreground = p.text;
    c.overlay = black().opacity(if p.dark { 0.45 } else { 0.22 });
    c.group_box = p.header;
    c.group_box_foreground = p.text;
    c.description_list_label = p.header;
    c.description_list_label_foreground = p.text_secondary;
    c.skeleton = p.control;
    c.accordion = gpui_kit::transparent_black();

    // В gpui-component `accent` — подсветка пункта под курсором (меню, списки).
    c.accent = p.control_hover;
    c.accent_foreground = p.text;
    c.primary = p.accent;
    c.primary_hover = p.accent.opacity(0.88);
    c.primary_active = p.accent.opacity(0.78);
    c.primary_foreground = p.accent_fg;
    c.secondary = p.control;
    c.secondary_hover = p.control_hover;
    c.secondary_active = p.control_active;
    c.secondary_foreground = p.text;
    c.button = p.control;
    c.button_hover = p.control_hover;
    c.button_active = p.control_active;
    c.button_foreground = p.text;
    c.button_primary = p.accent;
    c.button_primary_hover = p.accent.opacity(0.88);
    c.button_primary_active = p.accent.opacity(0.78);
    c.button_primary_foreground = p.accent_fg;
    c.button_secondary = p.control;
    c.button_secondary_hover = p.control_hover;
    c.button_secondary_active = p.control_active;
    c.button_secondary_foreground = p.text;

    // Статусные цвета: заливка — тон, текст на заливке — белый.
    for (fill, hover, active, fg, name) in [
        (
            &mut c.danger,
            &mut c.danger_hover,
            &mut c.danger_active,
            &mut c.danger_foreground,
            ToneName::Red,
        ),
        (
            &mut c.success,
            &mut c.success_hover,
            &mut c.success_active,
            &mut c.success_foreground,
            ToneName::Green,
        ),
        (
            &mut c.warning,
            &mut c.warning_hover,
            &mut c.warning_active,
            &mut c.warning_foreground,
            ToneName::Orange,
        ),
        (
            &mut c.info,
            &mut c.info_hover,
            &mut c.info_active,
            &mut c.info_foreground,
            ToneName::Blue,
        ),
    ] {
        let t = tone(name).fg;
        *fill = t;
        *hover = t.opacity(0.88);
        *active = t.opacity(0.78);
        *fg = if p.dark { black() } else { white() };
    }
    c.button_danger = c.danger;
    c.button_danger_hover = c.danger_hover;
    c.button_danger_active = c.danger_active;
    c.button_danger_foreground = c.danger_foreground;

    // Поля, выделение текста, ссылки.
    c.input = field_border;
    c.ring = p.accent;
    c.caret = p.accent_text;
    c.selection = p.accent.opacity(0.32);
    c.link = p.accent_text;
    c.link_hover = p.accent_text.opacity(0.85);
    c.link_active = p.accent_text.opacity(0.7);

    // Списки и таблицы прозрачные: фон даёт панель-«стекло» под ними.
    let clear = gpui_kit::transparent_black();
    c.list = clear;
    c.list_even = p.stripe;
    c.list_hover = p.row_hover;
    c.list_active = p.selection_soft;
    c.list_active_border = clear;
    c.list_head = p.header;
    c.table = clear;
    c.table_even = p.stripe;
    c.table_hover = p.row_hover;
    c.table_active = p.selection;
    c.table_active_border = clear;
    c.table_head = p.header;
    c.table_head_foreground = p.text_secondary;
    c.table_foot = p.header;
    c.table_foot_foreground = p.text_secondary;
    c.table_row_border = clear;

    // Скроллбары — оверлей без трека.
    c.scrollbar = clear;
    c.scrollbar_thumb = p.text.opacity(0.28);
    c.scrollbar_thumb_hover = p.text.opacity(0.45);

    // Сайдбар, вкладки, хром окна.
    c.sidebar = p.sidebar;
    c.sidebar_border = p.hairline;
    c.sidebar_foreground = p.text;
    c.sidebar_accent = p.selection_soft;
    c.sidebar_accent_foreground = p.text;
    c.sidebar_primary = p.accent;
    c.sidebar_primary_foreground = p.accent_fg;
    c.tab_bar = clear;
    c.tab_bar_segmented = p.control;
    c.tab = clear;
    c.tab_active = p.raised;
    c.tab_active_foreground = p.text;
    c.tab_foreground = p.text_secondary;
    c.title_bar = clear;
    c.title_bar_border = p.hairline;
    c.status_bar = clear;
    c.status_bar_border = p.hairline;

    // Прочие контролы.
    c.switch = p.control_active;
    c.switch_thumb = white();
    c.slider_bar = p.accent;
    c.slider_thumb = white();
    c.progress_bar = p.accent;
    c.drag_border = p.accent;
    c.drop_target = p.accent.opacity(0.15);

    // Именованные цвета (графики, подсветка): тона палитры.
    for (strong, light, name) in [
        (&mut c.red, &mut c.red_light, ToneName::Red),
        (&mut c.green, &mut c.green_light, ToneName::Green),
        (&mut c.blue, &mut c.blue_light, ToneName::Blue),
        (&mut c.yellow, &mut c.yellow_light, ToneName::Yellow),
        (&mut c.magenta, &mut c.magenta_light, ToneName::Pink),
        (&mut c.cyan, &mut c.cyan_light, ToneName::Teal),
    ] {
        *strong = tone(name).fg;
        *light = tone(name).bg;
    }
    c.chart_1 = tone(ToneName::Blue).fg;
    c.chart_2 = tone(ToneName::Green).fg;
    c.chart_3 = tone(ToneName::Orange).fg;
    c.chart_4 = tone(ToneName::Purple).fg;
    c.chart_5 = tone(ToneName::Teal).fg;
    c
}

/// Применить палитру ко всему приложению.
pub fn apply(palette: Palette, cx: &mut App) {
    let mode = if palette.dark {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(mode, None, cx);
    let colors = theme_colors(&palette, Theme::global(cx).colors);
    {
        let theme = Theme::global_mut(cx);
        theme.colors = colors;
        theme.tokens = ThemeTokens::from(&colors);
        theme.font_size = px(metrics::TEXT);
        theme.radius = px(metrics::RADIUS);
        theme.radius_lg = px(metrics::RADIUS_LG);
        theme.shadow = true;
    }
    Theme::sync_base(cx);
    cx.set_global(ActivePalette(palette));
}

/// Переменная окружения для принудительной темы (проверка скриншотами,
/// не трогая системную настройку): `light` | `dark`.
pub const THEME_OVERRIDE_ENV: &str = "CRABTRAP_THEME";

/// Разбор значения [`THEME_OVERRIDE_ENV`]: `Some(dark)` или `None` — система.
pub fn parse_theme_override(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "dark" => Some(true),
        "light" => Some(false),
        _ => None,
    }
}

fn forced_dark() -> Option<bool> {
    std::env::var(THEME_OVERRIDE_ENV)
        .ok()
        .and_then(|value| parse_theme_override(&value))
}

/// Применить тему по оформлению системы (до открытия окон).
pub fn apply_system(cx: &mut App) {
    let dark = forced_dark().unwrap_or_else(|| is_dark(cx.window_appearance()));
    apply(Palette::for_dark(dark), cx);
}

/// Следовать за светлой/тёмной темой системы, пока живо окно.
pub fn follow_system(window: &mut Window) {
    if forced_dark().is_some() {
        return;
    }
    window
        .observe_window_appearance(|window, cx| {
            let dark = is_dark(window.appearance());
            if cx.global::<ActivePalette>().0.dark != dark {
                apply(Palette::for_dark(dark), cx);
                cx.refresh_windows();
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_maps_to_dark_flag() {
        assert!(is_dark(WindowAppearance::Dark));
        assert!(is_dark(WindowAppearance::VibrantDark));
        assert!(!is_dark(WindowAppearance::Light));
        assert!(!is_dark(WindowAppearance::VibrantLight));
    }

    #[test]
    fn theme_override_values() {
        assert_eq!(parse_theme_override("dark"), Some(true));
        assert_eq!(parse_theme_override(" Light "), Some(false));
        assert_eq!(parse_theme_override("system"), None);
        assert_eq!(parse_theme_override(""), None);
    }

    #[test]
    fn tables_and_lists_stay_transparent_over_glass() {
        for p in [Palette::light(), Palette::dark()] {
            let c = theme_colors(&p, ThemeColor::default());
            assert_eq!(c.table.a, 0.);
            assert_eq!(c.list.a, 0.);
            assert_eq!(c.scrollbar.a, 0.);
            assert_eq!(c.title_bar.a, 0.);
            assert_eq!(c.foreground, p.text);
            assert_eq!(c.table_active, p.selection);
        }
    }

    #[test]
    fn fields_and_popovers_are_opaque_enough_to_read() {
        for p in [Palette::light(), Palette::dark()] {
            let c = theme_colors(&p, ThemeColor::default());
            assert_eq!(c.background.a, 1.);
            assert!(c.popover.a >= 0.95);
        }
    }
}
