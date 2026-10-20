//! Запись экспортированного файла: временный файл рядом с целью (права
//! 0600 — в HAR куки и токены; уникальное имя — параллельные экспорты не
//! мешают друг другу), fsync, затем rename. Брошенный без commit файл
//! удаляется — прерванный экспорт не оставляет обрезка вместо прежнего.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use application::services::{ExportFile, ExportFileWriter};
use domain::errors::DomainError;

/// Права файла экспорта: только владелец.
const EXPORT_FILE_MODE: u32 = 0o600;

pub struct FsExportFileWriter;

impl ExportFileWriter for FsExportFileWriter {
    fn create(&self, path: &Path) -> Result<Box<dyn ExportFile>, DomainError> {
        let file_name = path.file_name().ok_or_else(|| {
            DomainError::ValidationError(format!("not a file path: {}", path.display()))
        })?;
        let tmp = path.with_file_name(format!(
            ".{}.{}.tmp",
            file_name.to_string_lossy(),
            uuid::Uuid::new_v4().simple()
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(EXPORT_FILE_MODE)
            .open(&tmp)
            .map_err(|e| failed("create", path, e))?;
        Ok(Box::new(FsExportFile {
            file: Some(file),
            tmp,
            target: path.to_path_buf(),
        }))
    }
}

struct FsExportFile {
    /// `None` — уже закоммичен.
    file: Option<File>,
    tmp: PathBuf,
    target: PathBuf,
}

impl Write for FsExportFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.open()?.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.open()?.flush()
    }
}

impl FsExportFile {
    fn open(&mut self) -> std::io::Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("export file already committed"))
    }
}

impl ExportFile for FsExportFile {
    fn commit(mut self: Box<Self>) -> Result<(), DomainError> {
        let target = self.target.clone();
        let file = self
            .file
            .take()
            .ok_or_else(|| failed("save", &target, std::io::Error::other("already committed")))?;
        file.sync_all().map_err(|e| failed("save", &target, e))?;
        drop(file);
        std::fs::rename(&self.tmp, &target).map_err(|e| failed("save", &target, e))
    }
}

impl Drop for FsExportFile {
    fn drop(&mut self) {
        // Не закоммичен (ошибка, отмена) или rename не удался — убрать tmp.
        if self.file.take().is_some() || self.tmp.exists() {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

fn failed(what: &str, path: &Path, e: std::io::Error) -> DomainError {
    tracing::warn!(path = %path.display(), error = %e, "export: {what} failed");
    DomainError::InfrastructureError(format!("{what} {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn names(dir: &Path) -> Vec<std::ffi::OsString> {
        std::fs::read_dir(dir)
            .expect("dir")
            .map(|e| e.expect("entry").file_name())
            .collect()
    }

    #[test]
    fn commit_replaces_file_owner_only_and_leaves_no_temp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.har");
        std::fs::write(&path, b"old").expect("seed");

        let mut file = FsExportFileWriter.create(&path).expect("create");
        file.write_all(b"new").expect("write");
        assert_eq!(
            std::fs::read(&path).expect("read"),
            b"old",
            "untouched before commit"
        );
        file.commit().expect("commit");

        assert_eq!(std::fs::read(&path).expect("read"), b"new");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, EXPORT_FILE_MODE);
        assert_eq!(
            names(dir.path()),
            vec![std::ffi::OsString::from("session.har")]
        );
    }

    #[test]
    fn dropped_without_commit_keeps_the_old_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.har");
        std::fs::write(&path, b"old").expect("seed");

        let mut file = FsExportFileWriter.create(&path).expect("create");
        file.write_all(b"half").expect("write");
        drop(file);

        assert_eq!(std::fs::read(&path).expect("read"), b"old");
        assert_eq!(
            names(dir.path()),
            vec![std::ffi::OsString::from("session.har")]
        );
    }

    #[test]
    fn missing_folder_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            FsExportFileWriter
                .create(&dir.path().join("nope/x.har"))
                .is_err()
        );
    }
}
