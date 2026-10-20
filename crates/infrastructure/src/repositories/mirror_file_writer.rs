//! Запись файлов Mirror: каталоги создаются по пути, файл пишется во
//! временный рядом и переименовывается — читатель папки не видит обрезка.
//! Путь проверяется ещё раз: только обычные компоненты внутри папки.
//!
//! Один путь бывает и ресурсом, и коллекцией (`/users` и `/users/7`): файл,
//! стоящий на месте нужной папки, переезжает в `<он>/index.html`, а ответ
//! для пути, где уже папка, пишется в `<папка>/index.html` — порядок
//! запросов не важен.

use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use application::services::MirrorFileWriter;
use domain::errors::DomainError;

/// Файл для пути, который одновременно и папка (`/docs` при `/docs/x`).
const DIRECTORY_INDEX: &str = "index.html";

pub struct FsMirrorFileWriter;

impl MirrorFileWriter for FsMirrorFileWriter {
    fn write(&self, folder: &Path, relative: &Path, data: &[u8]) -> Result<PathBuf, DomainError> {
        let inside = relative.components().count() > 0
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_)));
        if !inside {
            return Err(DomainError::ValidationError(format!(
                "mirror path {} would leave the folder",
                relative.display()
            )));
        }
        let mut target = folder.join(relative);
        let parent = target.parent().unwrap_or(folder).to_path_buf();
        make_directories(folder, &parent)?;
        if is_directory(&target) {
            target.push(DIRECTORY_INDEX);
        }
        replace_file(&target, data)?;
        Ok(target)
    }
}

/// Создать папки от `folder` до `dir`; файл на месте папки переезжает в
/// `<папка>/index.html`.
fn make_directories(folder: &Path, dir: &Path) -> Result<(), DomainError> {
    fs::create_dir_all(folder).map_err(|e| failed("create folder", folder, &e))?;
    let Ok(below) = dir.strip_prefix(folder) else {
        return Ok(());
    };
    let mut current = folder.to_path_buf();
    for component in below.components() {
        current.push(component);
        let is_file = fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_file());
        if is_file {
            file_into_directory(&current)?;
        } else if !current.exists() {
            fs::create_dir(&current).map_err(|e| failed("create folder", &current, &e))?;
        }
    }
    Ok(())
}

/// `path` (файл) → папка `path` с этим файлом как `index.html`.
fn file_into_directory(path: &Path) -> Result<(), DomainError> {
    let parent = path.parent().unwrap_or(path);
    let moved = parent.join(temp_name());
    fs::rename(path, &moved).map_err(|e| failed("move", path, &e))?;
    let index = path.join(DIRECTORY_INDEX);
    let placed = fs::create_dir(path).and_then(|()| fs::rename(&moved, &index));
    if let Err(error) = placed {
        // Вернуть файл на место: копия пути не должна пропасть из-за сбоя.
        let _ = fs::remove_dir(path);
        let _ = fs::rename(&moved, path);
        return Err(failed("move", &index, &error));
    }
    Ok(())
}

/// Записать во временный файл рядом (имя короткое — не упирается в
/// 255 байт имени), сбросить на диск и переименовать поверх цели.
fn replace_file(target: &Path, data: &[u8]) -> Result<(), DomainError> {
    let parent = target.parent().unwrap_or(target);
    let tmp = parent.join(temp_name());
    let written = fs::File::create(&tmp).and_then(|mut file| {
        file.write_all(data)?;
        file.sync_all()
    });
    if let Err(error) = written.and_then(|()| fs::rename(&tmp, target)) {
        // Временный файл не нужен ни при какой ошибке; не удалился — не
        // важнее исходной ошибки.
        let _ = fs::remove_file(&tmp);
        return Err(failed("write", target, &error));
    }
    Ok(())
}

fn temp_name() -> String {
    format!(".mirror-{}.tmp", uuid::Uuid::new_v4().simple())
}

fn is_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

fn failed(action: &str, path: &Path, error: &std::io::Error) -> DomainError {
    DomainError::InfrastructureError(format!("mirror: {action} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: PathBuf) -> Vec<u8> {
        fs::read(path).expect("read")
    }

    #[test]
    fn writes_the_tree_and_replaces_the_previous_copy() {
        let dir = tempfile::tempdir().expect("dir");
        let relative = Path::new("api.test/v1/users");

        FsMirrorFileWriter
            .write(dir.path(), relative, b"old")
            .expect("first");
        let path = FsMirrorFileWriter
            .write(dir.path(), relative, b"new")
            .expect("second");

        assert_eq!(path, dir.path().join(relative));
        assert_eq!(read(path), b"new");
        let leftovers = fs::read_dir(dir.path().join("api.test/v1"))
            .expect("dir")
            .count();
        assert_eq!(leftovers, 1, "no temporary files left");
    }

    #[test]
    fn a_resource_that_is_also_a_collection_keeps_both_in_either_order() {
        for (first, second) in [("users", "users/7"), ("users/7", "users")] {
            let dir = tempfile::tempdir().expect("dir");
            let root = dir.path();
            FsMirrorFileWriter
                .write(root, &Path::new("api.test").join(first), first.as_bytes())
                .expect("first");
            FsMirrorFileWriter
                .write(root, &Path::new("api.test").join(second), second.as_bytes())
                .expect("second");

            assert_eq!(read(root.join("api.test/users/index.html")), b"users");
            assert_eq!(read(root.join("api.test/users/7")), b"users/7");
        }
    }

    #[test]
    fn long_names_fit_with_the_temporary_file() {
        let dir = tempfile::tempdir().expect("dir");
        let name = "a".repeat(255);
        let path = FsMirrorFileWriter
            .write(dir.path(), &Path::new("api.test").join(&name), b"x")
            .expect("255-byte name");
        assert_eq!(read(path), b"x");
    }

    #[test]
    fn refuses_paths_that_leave_the_folder() {
        let dir = tempfile::tempdir().expect("dir");
        for bad in ["../escape", "/etc/passwd", "a/../../b", ""] {
            assert!(
                FsMirrorFileWriter
                    .write(dir.path(), Path::new(bad), b"x")
                    .is_err(),
                "{bad}"
            );
        }
    }
}
