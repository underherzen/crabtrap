//! Хранилище тел обменов. Домен оперирует `Vec<u8>`; работа с большими
//! кусками байт (bytes/spill) — деталь реализации в infrastructure.

use async_trait::async_trait;

use crate::entities::body::{BodyDescriptor, BodyRef};
use crate::errors::DomainError;
use crate::value_objects::BodyId;

#[async_trait]
pub trait BodyRepository: Send + Sync {
    /// Сохранить тело и получить ссылку на него.
    /// Если размер превышает cap реализации — тело не сохраняется,
    /// `BodyRef.size` = 0 (поток при этом продолжает проксироваться).
    async fn put(
        &self,
        id: BodyId,
        data: Vec<u8>,
        descriptor: BodyDescriptor,
    ) -> Result<BodyRef, DomainError>;

    /// Тело целиком (None — не сохранено/удалено).
    async fn get(&self, body_ref: &BodyRef) -> Result<Option<Vec<u8>>, DomainError>;

    /// Диапазон тела (для hex-просмотра по страницам).
    async fn get_range(
        &self,
        body_ref: &BodyRef,
        offset: u64,
        len: u64,
    ) -> Result<Option<Vec<u8>>, DomainError>;

    async fn delete(&self, id: BodyId) -> Result<(), DomainError>;

    /// Суммарный размер хранимых тел (для метрик памяти/диска).
    async fn total_size(&self) -> Result<u64, DomainError>;

    /// Удалить все тела (вместе с ExchangeRepository::clear).
    async fn clear(&self) -> Result<(), DomainError>;
}
