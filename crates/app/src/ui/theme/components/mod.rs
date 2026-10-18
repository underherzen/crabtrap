//! Базовые компоненты дизайн-системы.
//!
//! Простые — функции, возвращающие стилизованный `Div` (дальше вызывающий
//! докручивает размеры/детей); с состоянием клика и подсказкой —
//! `RenderOnce`-структуры.

mod chip;
mod glass_panel;
mod nav;
mod pill;
mod segmented;
mod status_pill;
mod toolbar_button;

pub use chip::FilterChip;
pub use glass_panel::{glass_panel, glass_sidebar, soft_shadow};
pub use nav::NavItem;
pub use pill::{dot, pill, tone_pill};
pub use segmented::SegmentedControl;
pub use status_pill::{StatusPill, StatusSegment};
pub use toolbar_button::ToolbarButton;

/// id дочернего элемента (сегмент, пункт) от id родителя.
pub(crate) fn child_id(parent: &gpui_kit::ElementId, ix: usize) -> gpui_kit::ElementId {
    gpui_kit::ElementId::NamedChild(std::sync::Arc::new(parent.clone()), ix.to_string().into())
}
