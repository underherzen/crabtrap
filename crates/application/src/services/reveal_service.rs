//! Порт «показать в Finder» (реализация — infrastructure, `open -R`).

use std::path::Path;

pub trait RevealService: Send + Sync {
    /// Раскрыть путь в Finder. Ошибки — только в лог (best effort).
    fn reveal(&self, path: &Path);
}
