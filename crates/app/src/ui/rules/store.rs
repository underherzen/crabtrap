//! RulesStore — состояние экрана правил (Tools): снапшот RuleSet из
//! rules.json и применение правок.
//!
//! Правка = чистая функция `RuleSet → RuleSet` (application::rules::edit).
//! Чтение свежего снапшота, валидация+сохранение (UpdateRuleSetUseCase) и
//! горячая перезагрузка движка — одна задача под `edit_lock`: быстрые правки
//! подряд выполняются по очереди и не теряют друг друга (а не отбрасываются,
//! пока предыдущая в полёте).

use std::sync::Arc;

use application::dto::proxy_config::EngineCommand;
use application::dto::rules::{RuleSetDto, ThrottleSpecDto};
use application::use_cases::rules::UpdateRuleSetUseCase;
use domain::DomainError;
use domain::entities::rule::RuleSet;
use domain::repositories::RuleRepository;
use gpui_kit::{App, Entity};

use crate::app_state::AppState;
use crate::runtime::EngineHandle;
use crate::ui::async_bridge::spawn_ui_task;

pub struct RulesStore {
    pub rules: RuleSet,
    pub error: Option<String>,
    edit_lock: Arc<tokio::sync::Mutex<()>>,
    repository: Arc<dyn RuleRepository>,
    engine: EngineHandle,
    runtime: tokio::runtime::Handle,
}

impl RulesStore {
    pub fn new(state: &AppState, runtime: tokio::runtime::Handle) -> Self {
        Self {
            rules: RuleSet::default(),
            error: None,
            edit_lock: Arc::new(tokio::sync::Mutex::new(())),
            repository: state.rule_repository.clone(),
            engine: state.engine_command.clone(),
            runtime,
        }
    }

    /// Перечитать rules.json (при открытии экрана).
    pub fn refresh(entity: &Entity<Self>, cx: &mut App) {
        let (repository, runtime) = {
            let s = entity.read(cx);
            (s.repository.clone(), s.runtime.clone())
        };
        spawn_ui_task(
            cx,
            entity,
            runtime,
            async move { repository.get().await },
            |state: &mut Self, result, cx| {
                match result {
                    Ok(rules) => {
                        state.rules = rules;
                        state.error = None;
                    }
                    Err(e) => state.error = Some(e.to_string()),
                }
                cx.notify();
            },
        );
    }

    /// Применить правку: свежий снапшот → `edit` → валидация и сохранение →
    /// движок. Ошибка (валидация, IO) — в `error`.
    pub fn apply(
        entity: &Entity<Self>,
        cx: &mut App,
        edit: impl FnOnce(RuleSet) -> Result<RuleSet, DomainError> + Send + 'static,
    ) {
        Self::apply_then(entity, cx, edit, |_, _| {});
    }

    /// То же, плюс `on_done` с результатом сохранения — диалог закрывается
    /// только после успеха, а ошибку показывает у себя.
    pub fn apply_then(
        entity: &Entity<Self>,
        cx: &mut App,
        edit: impl FnOnce(RuleSet) -> Result<RuleSet, DomainError> + Send + 'static,
        on_done: impl FnOnce(Result<(), String>, &mut App) + Send + 'static,
    ) {
        let (repository, engine, runtime, lock) = entity.update(cx, |state, cx| {
            state.error = None;
            cx.notify();
            (
                state.repository.clone(),
                state.engine.clone(),
                state.runtime.clone(),
                state.edit_lock.clone(),
            )
        });
        spawn_ui_task(
            cx,
            entity,
            runtime,
            async move {
                let _serialized = lock.lock().await;
                let edited = edit(repository.get().await?)?;
                UpdateRuleSetUseCase::new(repository.clone())
                    .execute(RuleSetDto::from_rule_set(&edited))
                    .await?;
                let saved = repository.get().await?;
                push_to_engine(&engine, &saved);
                Ok(saved)
            },
            move |state: &mut Self, result, cx| {
                let outcome = match result {
                    Ok(rules) => {
                        state.rules = rules;
                        Ok(())
                    }
                    Err(e) => {
                        state.error = Some(e.to_string());
                        Err(e.to_string())
                    }
                };
                cx.notify();
                on_done(outcome, cx);
            },
        );
    }
}

/// Горячая перезагрузка движка без разрыва соединений.
fn push_to_engine(engine: &EngineHandle, rules: &RuleSet) {
    engine.send(EngineCommand::UpdateRules {
        snapshot: rules.clone(),
    });
    engine.send(EngineCommand::UpdateThrottle {
        spec: rules
            .effective_throttle()
            .map(ThrottleSpecDto::from_profile),
    });
    engine.send(EngineCommand::UpdateSslProxying {
        entries: rules.effective_ssl_entries(),
    });
    engine.send(EngineCommand::SetDnsOverrides {
        entries: rules.effective_dns_overrides(),
    });
}
