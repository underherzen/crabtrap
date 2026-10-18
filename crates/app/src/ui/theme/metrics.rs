//! Размеры дизайн-системы: радиусы, отступы, шрифты, высоты контролов.
//! Значения в пунктах (px GPUI = логические пиксели macOS).

/// Высота тайтл-бара с тулбаром (unified toolbar macOS).
pub const TITLE_BAR_HEIGHT: f32 = 52.;
/// «Светофоры» macOS: по центру тайтл-бара по вертикали.
pub const TRAFFIC_LIGHTS_X: f32 = 18.;
pub const TRAFFIC_LIGHTS_Y: f32 = 19.;
/// Отступ содержимого тайтл-бара под «светофоры».
pub const TRAFFIC_LIGHTS_INSET: f32 = 84.;
/// Нижняя панель состояния.
pub const STATUS_BAR_HEIGHT: f32 = 26.;

// --- Радиусы -----------------------------------------------------------------
pub const RADIUS_SM: f32 = 4.;
pub const RADIUS: f32 = 6.;
pub const RADIUS_LG: f32 = 10.;

// --- Шрифт -------------------------------------------------------------------
// Семейство — системное (SF Pro): его разрешает `Theme::change`.
pub const TEXT_XS: f32 = 11.;
pub const TEXT_SM: f32 = 12.;
pub const TEXT: f32 = 13.;
pub const TEXT_LG: f32 = 15.;

// --- Контролы ----------------------------------------------------------------
/// Кнопка тулбара / сегмент.
pub const CONTROL_HEIGHT: f32 = 28.;
/// Плашки (метод, статус, тег) и чипы фильтра.
pub const PILL_HEIGHT: f32 = 18.;
pub const CHIP_HEIGHT: f32 = 22.;
/// Строка таблицы трафика и сайдбара.
pub const ROW_HEIGHT: f32 = 26.;
pub const SIDEBAR_WIDTH: f32 = 220.;
