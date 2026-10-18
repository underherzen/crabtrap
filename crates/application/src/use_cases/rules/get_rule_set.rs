//! Текущий снимок правил для UI-редактора.

use std::sync::Arc;

use domain::errors::DomainError;
use domain::repositories::RuleRepository;

use crate::dto::rules::RuleSetDto;

pub struct GetRuleSetUseCase {
    rule_repository: Arc<dyn RuleRepository>,
}

impl GetRuleSetUseCase {
    pub fn new(rule_repository: Arc<dyn RuleRepository>) -> Self {
        Self { rule_repository }
    }

    pub async fn execute(&self) -> Result<RuleSetDto, DomainError> {
        let rules = self.rule_repository.get().await?;
        Ok(RuleSetDto::from_rule_set(&rules))
    }
}
