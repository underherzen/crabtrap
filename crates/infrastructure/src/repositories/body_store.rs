//! Хранилище тел: маленькие тела — в памяти, большие (свыше порога) —
//! spill на диск в кэш-каталог. Выше cap тела не сохраняются вовсе
//! (поток продолжает проксироваться — «стриминговая» семантика D5).
//! Сверх общего бюджета памяти на диск уходят и маленькие тела: при
//! 10k обменах в буфере сумма «маленьких» тел иначе ничем не ограничена.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;

use domain::entities::body::{BodyDescriptor, BodyRef, BodyStorage};
use domain::errors::DomainError;
use domain::repositories::BodyRepository;
use domain::value_objects::BodyId;

enum StoredBody {
    /// `None` — тело не сохранено (превышен cap).
    Skipped,
    Memory(Vec<u8>),
    Disk {
        path: PathBuf,
        size: u64,
    },
}

/// Тела + сколько байт из них лежит в памяти (меняются под одним замком).
#[derive(Default)]
struct Bodies {
    map: HashMap<BodyId, StoredBody>,
    memory_bytes: u64,
}

impl Bodies {
    fn insert(&mut self, id: BodyId, body: StoredBody) {
        if let StoredBody::Memory(bytes) = &body {
            self.memory_bytes += bytes.len() as u64;
        }
        if let Some(old) = self.map.insert(id, body) {
            self.forget(&old);
        }
    }

    fn remove(&mut self, id: &BodyId) -> Option<StoredBody> {
        let body = self.map.remove(id)?;
        self.forget(&body);
        Some(body)
    }

    fn forget(&mut self, body: &StoredBody) {
        if let StoredBody::Memory(bytes) = body {
            self.memory_bytes = self.memory_bytes.saturating_sub(bytes.len() as u64);
        }
    }
}

pub struct MemorySpillBodyStore {
    bodies: Mutex<Bodies>,
    /// Тела больше этого размера уходят на диск.
    spill_threshold_bytes: u64,
    /// Выше — не храним вовсе.
    capture_cap_bytes: u64,
    /// Сколько всего держим в памяти; сверх — на диск.
    memory_budget_bytes: u64,
    cache_dir: PathBuf,
}

impl MemorySpillBodyStore {
    pub fn new(spill_threshold_bytes: u64, capture_cap_bytes: u64, cache_dir: PathBuf) -> Self {
        Self {
            bodies: Mutex::new(Bodies::default()),
            spill_threshold_bytes,
            capture_cap_bytes,
            memory_budget_bytes: u64::MAX,
            cache_dir,
        }
    }

    /// Ограничить суммарный объём тел в памяти.
    pub fn with_memory_budget(self, memory_budget_bytes: u64) -> Self {
        Self {
            memory_budget_bytes,
            ..self
        }
    }

    /// Сколько байт тел сейчас в памяти.
    pub fn memory_bytes(&self) -> u64 {
        self.bodies.lock().expect("lock").memory_bytes
    }

    /// Из AppConfig (`capture` + `storage.bodies_cache_dir`).
    pub fn from_config(config: &config::AppConfig) -> Self {
        Self::new(
            config.capture.body_spill_threshold_bytes,
            config.capture.body_capture_cap_bytes,
            config.storage.bodies_cache_dir.clone(),
        )
        .with_memory_budget(config.capture.body_memory_budget_bytes)
    }
}

#[async_trait]
impl BodyRepository for MemorySpillBodyStore {
    async fn put(
        &self,
        id: BodyId,
        data: Vec<u8>,
        descriptor: BodyDescriptor,
    ) -> Result<BodyRef, DomainError> {
        let size = data.len() as u64;
        if size > self.capture_cap_bytes {
            tracing::debug!(%id, size, cap = self.capture_cap_bytes, "body over capture cap, not stored");
            self.bodies
                .lock()
                .expect("lock")
                .insert(id, StoredBody::Skipped);
            return Ok(BodyRef {
                id,
                size: 0,
                content_type: descriptor.content_type,
                encoding: descriptor.encoding,
                storage: BodyStorage::Memory,
            });
        }

        let over_budget = self
            .bodies
            .lock()
            .expect("lock")
            .memory_bytes
            .saturating_add(size)
            > self.memory_budget_bytes;
        let storage = if size > self.spill_threshold_bytes || over_budget {
            let path = self.cache_dir.join(id.to_string());
            fs::create_dir_all(&self.cache_dir)
                .map_err(|e| DomainError::InfrastructureError(format!("bodies cache dir: {e}")))?;
            fs::write(&path, &data)
                .map_err(|e| DomainError::InfrastructureError(format!("body spill: {e}")))?;
            self.bodies.lock().expect("lock").insert(
                id,
                StoredBody::Disk {
                    path: path.clone(),
                    size,
                },
            );
            BodyStorage::Disk(path)
        } else {
            self.bodies
                .lock()
                .expect("lock")
                .insert(id, StoredBody::Memory(data));
            BodyStorage::Memory
        };

        Ok(BodyRef {
            id,
            size,
            content_type: descriptor.content_type,
            encoding: descriptor.encoding,
            storage,
        })
    }

    async fn get(&self, body_ref: &BodyRef) -> Result<Option<Vec<u8>>, DomainError> {
        let guard = self.bodies.lock().expect("lock");
        match guard.map.get(&body_ref.id) {
            Some(StoredBody::Skipped) | None => Ok(None),
            Some(StoredBody::Memory(bytes)) => Ok(Some(bytes.clone())),
            Some(StoredBody::Disk { path, .. }) => {
                let path = path.clone();
                drop(guard);
                // Файл мог быть вычищен — трактуем как «тела нет».
                match fs::read(&path) {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(DomainError::InfrastructureError(format!("body read: {e}"))),
                }
            }
        }
    }

    async fn get_range(
        &self,
        body_ref: &BodyRef,
        offset: u64,
        len: u64,
    ) -> Result<Option<Vec<u8>>, DomainError> {
        let guard = self.bodies.lock().expect("lock");
        match guard.map.get(&body_ref.id) {
            Some(StoredBody::Skipped) | None => Ok(None),
            Some(StoredBody::Memory(bytes)) => {
                let start = (offset as usize).min(bytes.len());
                let end = (offset + len).clamp(0, bytes.len() as u64) as usize;
                Ok(Some(bytes[start..end].to_vec()))
            }
            Some(StoredBody::Disk { path, .. }) => {
                let path = path.clone();
                drop(guard);
                let mut file = match fs::File::open(&path) {
                    Ok(f) => f,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => {
                        return Err(DomainError::InfrastructureError(format!("body open: {e}")));
                    }
                };
                if file.seek(SeekFrom::Start(offset)).is_err() {
                    return Ok(Some(Vec::new()));
                }
                let mut buf = vec![0_u8; len as usize];
                let mut read = 0_usize;
                while read < buf.len() {
                    match file.read(&mut buf[read..]) {
                        Ok(0) => break,
                        Ok(n) => read += n,
                        Err(e) => {
                            return Err(DomainError::InfrastructureError(format!(
                                "body range read: {e}"
                            )));
                        }
                    }
                }
                buf.truncate(read);
                Ok(Some(buf))
            }
        }
    }

    async fn delete(&self, id: BodyId) -> Result<(), DomainError> {
        if let Some(StoredBody::Disk { path, .. }) = self.bodies.lock().expect("lock").remove(&id)
            && let Err(e) = fs::remove_file(&path)
        {
            tracing::warn!(path = %path.display(), error = %e, "cannot remove spilled body");
        }
        Ok(())
    }

    async fn total_size(&self) -> Result<u64, DomainError> {
        let guard = self.bodies.lock().expect("lock");
        Ok(guard
            .map
            .values()
            .map(|b| match b {
                StoredBody::Skipped => 0,
                StoredBody::Memory(bytes) => bytes.len() as u64,
                StoredBody::Disk { size, .. } => *size,
            })
            .sum())
    }

    async fn clear(&self) -> Result<(), DomainError> {
        let mut guard = self.bodies.lock().expect("lock");
        guard.memory_bytes = 0;
        for (_, body) in guard.map.drain() {
            if let StoredBody::Disk { path, .. } = body
                && let Err(e) = fs::remove_file(&path)
            {
                tracing::warn!(path = %path.display(), error = %e, "cannot remove spilled body");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(threshold: u64, cap: u64, dir: &std::path::Path) -> MemorySpillBodyStore {
        MemorySpillBodyStore::new(threshold, cap, dir.to_path_buf())
    }

    fn descriptor(ct: Option<&str>) -> BodyDescriptor {
        BodyDescriptor {
            size: 0,
            content_type: ct.map(String::from),
            encoding: None,
        }
    }

    #[tokio::test]
    async fn small_bodies_stay_in_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(1024, u64::MAX, tmp.path());
        let id = BodyId::new();
        let r = store
            .put(id, b"hello".to_vec(), descriptor(Some("text/plain")))
            .await
            .unwrap();

        assert_eq!(r.size, 5);
        assert_eq!(r.storage, BodyStorage::Memory);
        assert_eq!(store.get(&r).await.unwrap(), Some(b"hello".to_vec()));
        assert_eq!(store.total_size().await.unwrap(), 5);
    }

    #[tokio::test]
    async fn memory_budget_sends_small_bodies_to_disk_and_is_released() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store(1024, 1024 * 1024, dir.path()).with_memory_budget(10);
        let (a, b) = (BodyId::new(), BodyId::new());
        let first = store
            .put(a, b"123456".to_vec(), descriptor(None))
            .await
            .unwrap();
        assert!(matches!(first.storage, BodyStorage::Memory));
        assert_eq!(store.memory_bytes(), 6);
        // 6 + 6 > 10 — второе (маленькое!) тело уходит на диск.
        let second = store
            .put(b, b"abcdef".to_vec(), descriptor(None))
            .await
            .unwrap();
        assert!(matches!(second.storage, BodyStorage::Disk(_)));
        assert_eq!(
            store.get(&second).await.unwrap().as_deref(),
            Some(&b"abcdef"[..])
        );
        assert_eq!(store.memory_bytes(), 6);
        store.delete(a).await.unwrap();
        assert_eq!(store.memory_bytes(), 0, "budget released on delete");
        store.clear().await.unwrap();
        assert_eq!(store.memory_bytes(), 0);
    }

    #[tokio::test]
    async fn large_bodies_spill_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(8, u64::MAX, tmp.path());
        let id = BodyId::new();
        let payload: Vec<u8> = (0..100_u32).map(|i| (i % 251) as u8).collect();
        let r = store
            .put(id, payload.clone(), descriptor(None))
            .await
            .unwrap();

        assert_eq!(r.size, 100);
        assert!(matches!(r.storage, BodyStorage::Disk(_)));
        assert_eq!(store.get(&r).await.unwrap(), Some(payload));
        // Файл лежит в кэш-каталоге.
        assert!(tmp.path().join(id.to_string()).exists());
    }

    #[tokio::test]
    async fn over_cap_bodies_are_not_stored_but_referenced() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(8, 16, tmp.path());
        let r = store
            .put(BodyId::new(), vec![0_u8; 32], descriptor(None))
            .await
            .unwrap();

        assert_eq!(r.size, 0); // фактически сохранено ничего
        assert_eq!(store.get(&r).await.unwrap(), None);
        assert_eq!(store.total_size().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn range_reads_work_for_memory_and_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(4, u64::MAX, tmp.path());

        let mem = store
            .put(BodyId::new(), b"0123456789".to_vec(), descriptor(None))
            .await
            .unwrap();
        assert_eq!(
            store.get_range(&mem, 2, 3).await.unwrap(),
            Some(b"234".to_vec())
        );

        let disk = store
            .put(BodyId::new(), b"abcdefghij".to_vec(), descriptor(None))
            .await
            .unwrap();
        assert!(matches!(disk.storage, BodyStorage::Disk(_)));
        assert_eq!(
            store.get_range(&disk, 4, 3).await.unwrap(),
            Some(b"efg".to_vec())
        );
        // Диапазон за концом — обрезается.
        assert_eq!(
            store.get_range(&disk, 8, 10).await.unwrap(),
            Some(b"ij".to_vec())
        );
    }

    #[tokio::test]
    async fn delete_and_clear_remove_disk_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store(4, u64::MAX, tmp.path());
        let id = BodyId::new();
        store
            .put(id, b"spilled!".to_vec(), descriptor(None))
            .await
            .unwrap();
        let path = tmp.path().join(id.to_string());
        assert!(path.exists());

        store.delete(id).await.unwrap();
        assert!(!path.exists());
        assert_eq!(store.total_size().await.unwrap(), 0);

        let r2 = store
            .put(BodyId::new(), b"again!".to_vec(), descriptor(None))
            .await
            .unwrap();
        let path2 = match &r2.storage {
            BodyStorage::Disk(p) => p.clone(),
            _ => panic!("expected disk"),
        };
        store.clear().await.unwrap();
        assert!(!path2.exists());
    }
}
