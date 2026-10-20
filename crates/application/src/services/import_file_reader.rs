//! ПОРТ чтения файла, выбранного пользователем в диалоге открытия (сессия
//! `.crabtrap`, HAR); реализация — infrastructure. Читается потоком.

use std::io::Read;
use std::path::Path;

use domain::errors::DomainError;

pub trait ImportFileReader: Send + Sync {
    fn open(&self, path: &Path) -> Result<Box<dyn Read + Send>, DomainError>;
}
