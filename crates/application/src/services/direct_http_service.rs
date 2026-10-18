//! Порт прямой отправки запросов (Repeat / Edit & Resend / Compose).
//! Идёт мимо листенера — без петли через прокси. Реализация — engine
//! (UpstreamClient), Phase 7.

use domain::errors::DomainError;

use crate::dto::compose::{ComposedRequestDto, SendResultDto};

#[async_trait::async_trait]
pub trait DirectHttpService: Send + Sync {
    async fn send(&self, request: ComposedRequestDto) -> Result<SendResultDto, DomainError>;
}
