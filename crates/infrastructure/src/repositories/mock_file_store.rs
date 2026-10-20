//! Каталог моков Map Local на диске (`<app-support>/map_local`).
//! Существующие файлы не перетираются: имя занято — `name-2.ext`, `-3`…

use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use application::services::MockFileStore;
use async_trait::async_trait;
use domain::errors::DomainError;
use tokio::io::AsyncWriteExt as _;

/// Сколько суффиксов `-N` пробовать, прежде чем сдаться.
const MAX_NAME_ATTEMPTS: u32 = 1000;

pub struct DirectoryMockFileStore {
    root: PathBuf,
}

impl DirectoryMockFileStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl MockFileStore for DirectoryMockFileStore {
    async fn save(&self, relative: &Path, bytes: &[u8]) -> Result<PathBuf, DomainError> {
        if relative.as_os_str().is_empty()
            || !relative
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(DomainError::ValidationError(format!(
                "mock path must stay inside the mocks folder: {}",
                relative.display()
            )));
        }
        let target = self.root.join(relative);
        if let Some(parent) = target.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Err(match file_in_the_way(&self.root, parent).await {
                Some(file) => DomainError::ValidationError(format!(
                    "{} is a file, but a folder with that name is needed — rename or remove it",
                    file.display()
                )),
                None => io_error("create mocks folder", parent, e),
            });
        }
        for attempt in 1..=MAX_NAME_ATTEMPTS {
            let path = numbered(&target, attempt);
            match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
            {
                Ok(file) => return write_new(file, &path, bytes).await.map(|()| path),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_error("create mock file", &path, e)),
            }
        }
        Err(DomainError::InfrastructureError(format!(
            "no free name for mock file {}",
            target.display()
        )))
    }
}

/// Обычный файл на месте одного из каталогов `dir` внутри `root`.
async fn file_in_the_way(root: &Path, dir: &Path) -> Option<PathBuf> {
    for ancestor in dir.ancestors().take_while(|a| a.starts_with(root)) {
        if tokio::fs::metadata(ancestor)
            .await
            .is_ok_and(|m| m.is_file())
        {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

/// Записать новый файл целиком; сбой — файл удаляется (не оставлять обрезок).
async fn write_new(
    mut file: tokio::fs::File,
    path: &Path,
    bytes: &[u8],
) -> Result<(), DomainError> {
    let written = async {
        file.write_all(bytes).await?;
        file.flush().await
    }
    .await;
    if let Err(e) = written {
        let _ = tokio::fs::remove_file(path).await;
        return Err(io_error("write mock file", path, e));
    }
    Ok(())
}

/// `users.json` → `users-2.json` (attempt 1 — без суффикса).
fn numbered(path: &Path, attempt: u32) -> PathBuf {
    if attempt == 1 {
        return path.to_path_buf();
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(ext) => format!("{stem}-{attempt}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{attempt}"),
    };
    path.with_file_name(name)
}

fn io_error(what: &str, path: &Path, e: std::io::Error) -> DomainError {
    tracing::warn!(path = %path.display(), error = %e, "{what} failed");
    DomainError::InfrastructureError(format!("{what}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_nested_file_and_never_overwrites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DirectoryMockFileStore::new(dir.path().join("map_local"));
        let relative = Path::new("api.test/v1/users.json");

        let first = store.save(relative, b"[1]").await.expect("first");
        let second = store.save(relative, b"[2]").await.expect("second");
        let third = store.save(relative, b"[3]").await.expect("third");

        assert_eq!(first, dir.path().join("map_local/api.test/v1/users.json"));
        assert_eq!(
            second,
            dir.path().join("map_local/api.test/v1/users-2.json")
        );
        assert_eq!(third, dir.path().join("map_local/api.test/v1/users-3.json"));
        assert_eq!(std::fs::read(&first).expect("read"), b"[1]");
        assert_eq!(std::fs::read(&third).expect("read"), b"[3]");
    }

    #[tokio::test]
    async fn file_in_place_of_a_folder_is_a_clear_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("users"), b"x").expect("write");
        let store = DirectoryMockFileStore::new(dir.path().to_path_buf());

        let error = store
            .save(Path::new("users/1.json"), b"[]")
            .await
            .expect_err("collision");

        assert!(
            matches!(error, DomainError::ValidationError(_)),
            "{error:?}"
        );
        assert!(error.to_string().contains("users"), "{error}");
    }

    #[test]
    fn numbered_name_without_extension() {
        assert_eq!(
            numbered(Path::new("/m/index"), 2),
            PathBuf::from("/m/index-2")
        );
    }

    #[tokio::test]
    async fn rejects_paths_leaving_the_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DirectoryMockFileStore::new(dir.path().to_path_buf());
        for bad in ["../evil.json", "/etc/passwd", "a/../../b", ""] {
            assert!(
                store.save(Path::new(bad), b"x").await.is_err(),
                "{bad:?} must be rejected"
            );
        }
    }
}
