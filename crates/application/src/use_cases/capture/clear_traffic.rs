//! Очистить захваченный трафик (кнопка Clear): обмены + тела.

use std::sync::Arc;

use domain::errors::DomainError;
use domain::repositories::{BodyRepository, ExchangeRepository};

pub struct ClearTrafficUseCase {
    exchange_repository: Arc<dyn ExchangeRepository>,
    body_repository: Arc<dyn BodyRepository>,
}

impl ClearTrafficUseCase {
    pub fn new(
        exchange_repository: Arc<dyn ExchangeRepository>,
        body_repository: Arc<dyn BodyRepository>,
    ) -> Self {
        Self {
            exchange_repository,
            body_repository,
        }
    }

    pub async fn execute(&self) -> Result<(), DomainError> {
        self.exchange_repository.clear().await?;
        self.body_repository.clear().await?;
        Ok(())
    }
}
