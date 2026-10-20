//! ПОРТ каталога моков Map Local (реализация — infrastructure, файлы в
//! `<app-support>/map_local`).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use domain::errors::DomainError;

#[async_trait]
pub trait MockFileStore: Send + Sync {
    /// Записать тело мока по относительному (уже безопасному) пути внутри
    /// каталога моков. Занятое имя не перетирается — к имени добавляется
    /// `-2`, `-3`… (файл мог быть отредактирован руками). Возврат —
    /// абсолютный путь записанного файла.
    async fn save(&self, relative: &Path, bytes: &[u8]) -> Result<PathBuf, DomainError>;
}
