//! ПОРТ записи файла Mirror: `relative` (уже безопасный: без `..`,
//! абсолютных частей и разделителей в именах) внутри папки записи `folder`;
//! реализация — infrastructure. Возврат — полный путь файла.

use std::path::{Path, PathBuf};

use domain::errors::DomainError;

pub trait MirrorFileWriter: Send + Sync {
    fn write(&self, folder: &Path, relative: &Path, data: &[u8]) -> Result<PathBuf, DomainError>;
}
