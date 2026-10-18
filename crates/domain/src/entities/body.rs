//! Тело обмена хранится ВНЕ сущности — обмен держит только ссылку (BodyRef).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::value_objects::BodyId;

/// Где физически лежат байты тела.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum BodyStorage {
    /// В памяти (или не сохранено — cap/потоковая семантика).
    #[default]
    Memory,
    /// Spill на диск (кэш-каталог).
    Disk(PathBuf),
}

/// Ссылка на захваченное тело.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BodyRef {
    pub id: BodyId,
    /// Фактически сохранённый размер (0 — если тело не хранилось).
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub encoding: Option<String>,
    #[serde(default)]
    pub storage: BodyStorage,
}

/// Описание тела на момент захвата (вход `BodyRepository::put`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BodyDescriptor {
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub encoding: Option<String>,
}
