//! Сессии: файл `.crabtrap` (Save / Open Session) и импорт HAR.

pub mod format;
pub mod har_import;
mod session_file;

pub use session_file::{IMPORTED_TAG, ImportOutcome, ImportedRow, SessionFileUseCase};
