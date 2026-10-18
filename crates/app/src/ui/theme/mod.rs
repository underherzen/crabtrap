//! Дизайн-система Liquid Glass (Track G): палитра светлой/тёмной темы,
//! семантика цветов, размеры и базовые компоненты.
//!
//! - [`palette`] — поверхности, текст, акцент, тона; тесты контраста;
//! - [`semantic`] — метод/статус/тег/тип содержимого → тон;
//! - [`metrics`] — радиусы, отступы, шрифты, высоты;
//! - [`apply`] — проекция на тему gpui-component + следование за системой;
//! - [`icons`] — штатные + дополнительные Lucide-иконки (AssetSource);
//! - [`components`] — GlassPanel, Pill, SegmentedControl, ToolbarButton,
//!   StatusPill, FilterChip.
//!
//! Компоненты берут цвета из [`glass`], а не из констант: палитра меняется
//! вместе с системной темой.

pub mod apply;
pub mod components;
/// WCAG-контраст — инструмент тестов палитры.
#[cfg(test)]
pub mod contrast;
pub mod icons;
pub mod metrics;
pub mod palette;
pub mod semantic;

use gpui_kit::App;

pub use palette::{Palette, Tone, ToneName};

/// Текущая палитра (до `apply` — светлая).
pub fn glass(cx: &App) -> Palette {
    cx.try_global::<apply::ActivePalette>()
        .map(|active| active.0)
        .unwrap_or_else(Palette::light)
}
