//! Сессии: сохранение/загрузка снапшота трафика (Phase 8).
//! Прагматичный контракт: обмены передаются владением, тела — через
//! источник `BodySource` (адаптер над BodyRepository).

use async_trait::async_trait;

use crate::entities::body::BodyRef;
use crate::entities::exchange::HttpExchange;
use crate::entities::session::Session;
use crate::errors::DomainError;
use crate::value_objects::SessionId;

/// Источник байт тел при сохранении сессии.
#[async_trait]
pub trait BodySource: Send + Sync {
    async fn load_body(&self, body_ref: &BodyRef) -> Result<Option<Vec<u8>>, DomainError>;
}

#[async_trait]
pub trait SessionRepository: Send + Sync {
    /// Сохранить сессию: метаданные + обмены + тела (через `bodies`).
    async fn save(
        &self,
        session: &Session,
        exchanges: Vec<HttpExchange>,
        bodies: &dyn BodySource,
    ) -> Result<(), DomainError>;

    /// Загрузить обмены сессии (тела кладутся в хранилище тел реализацией
    /// и перелинковываются в BodyRef).
    async fn load(&self, id: &SessionId) -> Result<Vec<HttpExchange>, DomainError>;

    /// Метаданные всех сессий.
    async fn list(&self) -> Result<Vec<Session>, DomainError>;

    async fn delete(&self, id: &SessionId) -> Result<(), DomainError>;
}
