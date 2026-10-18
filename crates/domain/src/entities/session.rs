//! Сохранённая сессия трафика.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::value_objects::SessionId;

/// Метаданные сессии (сами обмены — в каталоге сессии, см. SessionRepository).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub exchange_count: u64,
    #[serde(default)]
    pub size_bytes: u64,
}
