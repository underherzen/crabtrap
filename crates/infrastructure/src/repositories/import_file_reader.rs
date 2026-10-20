//! Чтение файла, выбранного в диалоге открытия (сессия, HAR): буферизованный
//! поток файла.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use application::services::ImportFileReader;
use domain::errors::DomainError;

pub struct FsImportFileReader;

impl ImportFileReader for FsImportFileReader {
    fn open(&self, path: &Path) -> Result<Box<dyn Read + Send>, DomainError> {
        let file = File::open(path).map_err(|error| {
            DomainError::InfrastructureError(format!("open {}: {error}", path.display()))
        })?;
        Ok(Box::new(BufReader::new(file)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_file_and_reports_a_missing_one() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("x.txt");
        std::fs::write(&path, b"data").expect("write");
        let mut text = String::new();
        FsImportFileReader
            .open(&path)
            .expect("open")
            .read_to_string(&mut text)
            .expect("read");
        assert_eq!(text, "data");
        assert!(FsImportFileReader.open(&dir.path().join("none")).is_err());
    }
}
