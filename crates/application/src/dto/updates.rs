//! DTO проверки обновлений (GitHub Releases).

use serde::{Deserialize, Serialize};

/// Последний опубликованный релиз.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseInfoDto {
    /// Тег релиза как есть (`v0.2.0`).
    pub tag: String,
    /// Страница релиза (откуда качать .dmg).
    pub url: String,
}

/// Итог проверки относительно запущенной версии.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    UpToDate,
    Available(ReleaseInfoDto),
}
