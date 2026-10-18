//! Персистентность RuleSet (rules.json в app-support dir).

use async_trait::async_trait;

use crate::entities::rule::RuleSet;
use crate::errors::DomainError;

#[async_trait]
pub trait RuleRepository: Send + Sync {
    /// Текущий снимок правил (load-or-init при первом обращении).
    async fn get(&self) -> Result<RuleSet, DomainError>;

    /// Сохранить снимок; версия назначается репозиторием (version+1)
    /// и возвращается. Hot-reload движка идёт отдельной командой
    /// `EngineCommand::UpdateRules`.
    async fn update(&self, rules: RuleSet) -> Result<u64, DomainError>;

    /// Текущая версия (без загрузки всего набора).
    async fn version(&self) -> Result<u64, DomainError>;
}
