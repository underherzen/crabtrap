//! ПОРТ декомпрессии тел для инспектора (Phase 5).
//!
//! Движок захватывает кадры тела ПОСЛЕ http1-кодека hyper, но
//! Content-Encoding (gzip/br/deflate) остаётся на теле — инспектору нужен
//! расшифрованный вид. Декомпрессия — деталь infrastructure (flate2/brotli);
//! UI знает только этот порт.
//!
//! Контракт:
//! - `encoding` — значение Content-Encoding: "gzip" | "br" | "deflate"
//!   (case-insensitive). Всё остальное (включая "identity" и пустую строку) —
//!   pass-through: `decode` возвращает данные как есть, `supported` = false.
//! - Реализация обязана защищаться от zip-bomb: превышение лимита выхода —
//!   `DomainError::InvalidEntity`, а не OOM.

use async_trait::async_trait;
use domain::errors::DomainError;

#[async_trait]
pub trait BodyDecoderService: Send + Sync {
    /// Декодировать тело. Для неподдерживаемой/пустой кодировки —
    /// идентичный `data` (pass-through), не ошибка.
    async fn decode(&self, data: &[u8], encoding: &str) -> Result<Vec<u8>, DomainError>;

    /// Известна ли реализации эта кодировка (для UI-бейджа «encoded»).
    async fn supported(&self, encoding: &str) -> bool;
}
