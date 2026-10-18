//! Список SSL-Proxying (какие хосты расшифровываются через MITM).
//! Хранится внутри RuleSet, но имеет отдельный контракт для UI-списка.

use async_trait::async_trait;

use crate::entities::rule::SslProxyEntry;
use crate::errors::DomainError;

#[async_trait]
pub trait SslProxyRepository: Send + Sync {
    async fn list(&self) -> Result<Vec<SslProxyEntry>, DomainError>;

    /// Полностью заменить список.
    async fn replace(&self, entries: Vec<SslProxyEntry>) -> Result<(), DomainError>;

    /// Разрешён ли MITM для хоста (первый включённый совпавший шаблон).
    async fn matches_host(&self, host: &str) -> Result<bool, DomainError>;
}
