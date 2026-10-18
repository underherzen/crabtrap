//! Доменные ошибки. Единый тип для всех контрактов репозиториев и портов.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("Entity not found: {0}")]
    NotFound(String),

    #[error("Invalid entity: {0}")]
    InvalidEntity(String),

    #[error("Repository error: {0}")]
    RepositoryError(String),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Duplicate entity: {0}")]
    DuplicateEntity(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Infrastructure error: {0}")]
    InfrastructureError(String),
}
