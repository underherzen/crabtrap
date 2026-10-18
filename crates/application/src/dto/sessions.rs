//! DTO сессий (Phase 8).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use domain::value_objects::SessionId;

/// Метаданные сессии для менеджера сессий.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetaDto {
    pub id: SessionId,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub exchange_count: u64,
    pub size_bytes: u64,
}
