//! ПОРТ записи экспортированного файла (HAR) по пути, выбранному
//! пользователем в диалоге сохранения (реализация — infrastructure).
//! Файл пишется потоком: большая сессия не собирается в памяти целиком.

use std::path::Path;

use domain::errors::DomainError;

pub trait ExportFileWriter: Send + Sync {
    /// Начать запись `path` (во временный файл рядом с ним).
    fn create(&self, path: &Path) -> Result<Box<dyn ExportFile>, DomainError>;
}

/// Записываемый файл экспорта. До `commit` цель не тронута; брошенный без
/// `commit` файл удаляется — прерванный экспорт не оставляет обрезка.
pub trait ExportFile: std::io::Write + Send {
    /// Сбросить на диск и атомарно заменить цель (подтверждение замены —
    /// в диалоге сохранения).
    fn commit(self: Box<Self>) -> Result<(), DomainError>;
}
