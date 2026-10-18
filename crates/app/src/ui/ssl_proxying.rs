//! SslProxyingStore — тоггл «SSL Proxying» (MITM): on = один entry
//! `host_pattern: "*"` (расшифровывать все хосты), off = пустой список.
//! Персистится в rules.json (жить после рестарта) + hot-reload в движок.

use std::sync::Arc;

use application::dto::proxy_config::EngineCommand;
use application::use_cases::UpdateRuleSetUseCase;
use domain::repositories::RuleRepository;
use gpui_kit::{App, Entity};

use crate::runtime::EngineHandle;
use crate::ui::async_bridge::spawn_ui_task;

pub struct SslProxyingStore {
    /// Список сейчас включён (в rules.json есть включённый entry)?
    pub enabled: bool,
    pub busy: bool,
    pub error: Option<String>,
    rule_repository: Arc<dyn RuleRepository>,
    engine: EngineHandle,
    runtime: tokio::runtime::Handle,
}

impl SslProxyingStore {
    pub fn new(
        rule_repository: Arc<dyn RuleRepository>,
        engine: EngineHandle,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            enabled: false,
            busy: false,
            error: None,
            rule_repository,
            engine,
            runtime,
        }
    }

    /// Прочитать состояние из rules.json (при старте окна).
    pub fn refresh(entity: &Entity<Self>, cx: &mut App) {
        let rule_repository = entity.read(cx).rule_repository.clone();
        spawn_ui_task(
            cx,
            entity,
            // Синхронный репозиторий, но единый паттерн D4.
            entity.read(cx).runtime.clone(),
            async move {
                let snapshot = rule_repository.get().await?;
                Ok(snapshot.ssl_proxying_active())
            },
            |state: &mut Self, result, cx| {
                if let Ok(enabled) = result {
                    state.enabled = enabled;
                }
                cx.notify();
            },
        );
    }

    /// Тоггл глобального SSL Proxying: список хостов сохраняется (on без
    /// активных записей добавляет «*»). rules.json + движок.
    pub fn toggle(entity: &Entity<Self>, cx: &mut App) {
        let props = entity.update(cx, |state, cx| {
            if state.busy {
                cx.notify();
                return None;
            }
            state.busy = true;
            state.error = None;
            cx.notify();
            Some((
                state.rule_repository.clone(),
                state.engine.clone(),
                state.runtime.clone(),
                !state.enabled,
            ))
        });
        let Some((rule_repository, engine, runtime, enable)) = props else {
            return;
        };
        spawn_ui_task(
            cx,
            entity,
            runtime,
            async move {
                let snapshot = rule_repository.get().await?.with_ssl_proxying(enable);
                // Валидация + атомарный save + version++ (DTO-обёртка use case).
                UpdateRuleSetUseCase::new(rule_repository)
                    .execute(application::dto::rules::RuleSetDto::from_rule_set(
                        &snapshot,
                    ))
                    .await?;
                // Hot-reload движка без разрыва соединений.
                engine.send(EngineCommand::UpdateSslProxying {
                    entries: snapshot.effective_ssl_entries(),
                });
                Ok(enable)
            },
            |state: &mut Self, result, cx| {
                state.busy = false;
                match result {
                    Ok(enabled) => {
                        state.enabled = enabled;
                        state.error = None;
                    }
                    Err(e) => state.error = Some(e.to_string()),
                }
                cx.notify();
            },
        );
    }
}
