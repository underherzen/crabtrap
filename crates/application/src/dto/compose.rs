//! DTO Compose / Edit & Resend (Phase 7).

use serde::{Deserialize, Serialize};

use domain::value_objects::timing::TimingPhases;

use crate::dto::capture::HeaderEntryDto;

/// Запрос, составленный пользователем (отправляется мимо листенера —
/// без петли через прокси).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposedRequestDto {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<HeaderEntryDto>,
    #[serde(default)]
    pub body: Option<Vec<u8>>,
    /// Repeat / Compose — тег строки трафика.
    #[serde(default)]
    pub origin: domain::entities::exchange::RequestOrigin,
}

/// Результат прямой отправки.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SendResultDto {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<HeaderEntryDto>,
    #[serde(default)]
    pub body: Option<Vec<u8>>,
    #[serde(default)]
    pub timing: TimingPhases,
    /// Транспортная ошибка (таймаут/DNS/...), если была.
    pub error: Option<String>,
}
