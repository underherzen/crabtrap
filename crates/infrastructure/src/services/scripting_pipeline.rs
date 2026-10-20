//! Пайплайн со скриптами (Track S) поверх пайплайна правил.
//!
//! Порядок на запросе: Block List → `onRequest` → Map Local/Remote по уже
//! изменённому URL; заблокированный запрос скрипт не видит. Mock API:
//! `onRequest` + `onResponse` над пустым ответом → ответ без origin
//! (фаза ответа для такого обмена скрипт повторно не запускает). На ответе:
//! `onResponse` → пайплайн правил.
//!
//! Тела отдаются скрипту распакованными (gzip/br/deflate/zstd); не
//! тронутое скриптом тело уходит дальше как было (сжатым), заменённое —
//! без Content-Encoding. Консоль и ошибки — `ProxyEvent::ScriptLog`.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, RwLock};

use application::dto::capture::ProxyEvent;
use application::dto::rules::InterceptResolutionDto;
use application::dto::scripting::{
    ScriptCall, ScriptLogDto, ScriptOutput, ScriptRequestDto, ScriptResponseDto, ScriptSourceDto,
};
use application::matching::RequestTarget;
use application::services::{
    BodyDecoderService, ConnectDecision, EventPublisher, InterceptContext, InterceptOutcome,
    InterceptableMessage, InterceptionPipeline, RouteDecision, ScriptRuntime,
};
use async_trait::async_trait;
use domain::value_objects::InterceptId;

use super::message_body::{MessageBodies, finish_body};
use chrono::Utc;
use domain::entities::rule::{BreakPhase, RuleSet};
use domain::entities::script_rule::{ScriptPhase, ScriptRule};
use domain::value_objects::{ExchangeId, Host, RuleId};
use lru::LruCache;

/// Тег обмена, на котором выполнился скрипт.
const SCRIPTED_TAG: &str = "scripted";

pub struct ScriptingInterceptionPipeline {
    inner: Arc<dyn InterceptionPipeline>,
    rules: RwLock<RuleSet>,
    runtime: Arc<dyn ScriptRuntime>,
    bodies: MessageBodies,
    events: Arc<dyn EventPublisher>,
    /// Что выбрано на фазе запроса: фаза ответа идёт тем же скриптом (даже
    /// если `onRequest` поменял URL), ответ Mock API — без скрипта. LRU:
    /// обмены, не дошедшие до ответа, вытесняются, а не копятся.
    exchanges: Mutex<LruCache<ExchangeId, Chosen>>,
}

/// Решение фазы запроса для обмена.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chosen {
    Rule(RuleId),
    Mocked,
}

/// Обменов «между запросом и ответом», которые помнит пайплайн.
const REMEMBERED_EXCHANGES: usize = 4096;

/// Зависимости пайплайна.
pub struct ScriptingDeps {
    pub runtime: Arc<dyn ScriptRuntime>,
    pub decoder: Arc<dyn BodyDecoderService>,
    pub events: Arc<dyn EventPublisher>,
}

impl ScriptingInterceptionPipeline {
    pub fn new(inner: Arc<dyn InterceptionPipeline>, rules: RuleSet, deps: ScriptingDeps) -> Self {
        Self {
            inner,
            rules: RwLock::new(rules),
            runtime: deps.runtime,
            bodies: MessageBodies::new(deps.decoder),
            events: deps.events,
            exchanges: Mutex::new(LruCache::new(
                NonZeroUsize::new(REMEMBERED_EXCHANGES).expect("non-zero cache"),
            )),
        }
    }

    /// Первый совпавший с запросом скрипт (без учёта фазы).
    fn matching(&self, request: &InterceptableMessage) -> Option<ScriptRule> {
        let rules = self.rules.read().expect("rules lock");
        let target = RequestTarget::of_message(request, rules.uses_graphql())?;
        rules.matching_script(&target.facts()).cloned()
    }

    /// Скрипт фазы ответа: выбранный на запросе (если ещё включён), Mock
    /// API — никакого; обмен без записи — по итоговому запросу. `take` —
    /// забыть обмен (сам `on_response`; `wants_body` только смотрит).
    fn response_script(&self, context: &InterceptContext, take: bool) -> Option<ScriptRule> {
        let chosen = {
            let mut exchanges = self.exchanges.lock().expect("exchanges lock");
            match take {
                true => exchanges.pop(&context.exchange_id),
                false => exchanges.peek(&context.exchange_id).copied(),
            }
        };
        let rule = match chosen {
            Some(Chosen::Mocked) => None,
            Some(Chosen::Rule(id)) => self
                .rules
                .read()
                .expect("rules lock")
                .active_script(id)
                .cloned(),
            None => self.matching(context.request.as_ref()?),
        };
        rule.filter(|rule| rule.runs_on(ScriptPhase::Response))
    }

    fn remember(&self, id: ExchangeId, chosen: Chosen) {
        self.exchanges
            .lock()
            .expect("exchanges lock")
            .put(id, chosen);
    }

    async fn run(
        &self,
        rule: &ScriptRule,
        exchange_id: ExchangeId,
        request: ScriptRequestDto,
        response: Option<ScriptResponseDto>,
    ) -> Option<ScriptOutput> {
        let call = ScriptCall {
            script: ScriptSourceDto {
                rule_id: rule.id,
                name: rule.name.clone(),
                source: rule.source.clone(),
            },
            exchange_id: Some(exchange_id),
            request,
            response,
        };
        let run = self.runtime.run(call).await;
        self.publish_logs(rule, exchange_id, run.logs);
        match run.output {
            Ok(ScriptOutput::NotDefined) => None,
            Ok(output) => {
                self.events.publish(ProxyEvent::ExchangeTagged {
                    id: exchange_id,
                    tag: SCRIPTED_TAG.to_string(),
                });
                Some(output)
            }
            Err(error) => {
                tracing::debug!(rule = %rule.name, %error, "script failed, exchange unchanged");
                None
            }
        }
    }

    fn publish_logs(&self, rule: &ScriptRule, exchange_id: ExchangeId, logs: Vec<ScriptLogDto>) {
        for log in logs {
            self.events.publish(ProxyEvent::ScriptLog {
                rule_id: rule.id,
                exchange_id: Some(exchange_id),
                level: log.level,
                message: log.message,
                ts: Utc::now(),
            });
        }
    }

    async fn script_request(&self, message: &InterceptableMessage) -> ScriptRequestDto {
        ScriptRequestDto {
            method: message.method.clone().unwrap_or_default(),
            url: message.uri.clone().unwrap_or_default(),
            headers: message.headers.clone(),
            body: self.bodies.plain(message).await,
        }
    }

    async fn script_response(&self, message: &InterceptableMessage) -> ScriptResponseDto {
        ScriptResponseDto {
            status: message.status.unwrap_or(200),
            headers: message.headers.clone(),
            body: self.bodies.plain(message).await,
        }
    }

    /// `onRequest` (+ Mock API): изменённый запрос и маршрут (`Origin`
    /// или готовый ответ Mock API).
    async fn script_on_request(
        &self,
        context: &InterceptContext,
        rule: &ScriptRule,
    ) -> InterceptOutcome {
        let before = self.script_request(&context.message).await;
        let request = match rule.run_on_request {
            true => match self
                .run(rule, context.exchange_id, before.clone(), None)
                .await
            {
                Some(ScriptOutput::Request(edited)) => edited,
                _ => before.clone(),
            },
            false => before.clone(),
        };
        let message = apply_request(&context.message, &before, &request);
        if !rule.mock_api {
            return InterceptOutcome::Proceed {
                message,
                route: RouteDecision::Origin,
            };
        }
        let blank = ScriptResponseDto::blank();
        let response = match self
            .run(rule, context.exchange_id, request, Some(blank.clone()))
            .await
        {
            Some(ScriptOutput::Response(response)) => response,
            _ => blank,
        };
        self.remember(context.exchange_id, Chosen::Mocked);
        InterceptOutcome::Proceed {
            message,
            route: RouteDecision::Respond {
                status: response.status,
                headers: response.headers,
                body: response.body.unwrap_or_default(),
            },
        }
    }
}

fn apply_request(
    original: &InterceptableMessage,
    before: &ScriptRequestDto,
    after: &ScriptRequestDto,
) -> InterceptableMessage {
    let (headers, body) = finish_body(
        original,
        before.body.as_deref(),
        after.body.clone(),
        after.headers.clone(),
    );
    InterceptableMessage {
        method: Some(after.method.clone()),
        uri: Some(after.url.clone()),
        headers,
        body,
        ..original.clone()
    }
}

fn apply_response(
    original: &InterceptableMessage,
    before: &ScriptResponseDto,
    after: ScriptResponseDto,
) -> InterceptableMessage {
    let (headers, body) = finish_body(original, before.body.as_deref(), after.body, after.headers);
    InterceptableMessage {
        status: Some(after.status),
        headers,
        body,
        ..original.clone()
    }
}

#[async_trait]
impl InterceptionPipeline for ScriptingInterceptionPipeline {
    async fn on_connect(&self, host: &Host, port: u16) -> ConnectDecision {
        self.inner.on_connect(host, port).await
    }

    fn wants_body(&self, context: &InterceptContext) -> bool {
        let scripted = match context.phase {
            BreakPhase::Request => self
                .matching(&context.message)
                .is_some_and(|rule| rule.runs_on(ScriptPhase::Request)),
            BreakPhase::Response | BreakPhase::Both => {
                self.response_script(context, false).is_some()
            }
        };
        scripted || self.inner.wants_body(context)
    }

    async fn on_request(&self, context: InterceptContext) -> InterceptOutcome {
        let Some(rule) = self.matching(&context.message) else {
            return self.inner.on_request(context).await;
        };
        self.remember(context.exchange_id, Chosen::Rule(rule.id));
        if !rule.runs_on(ScriptPhase::Request) {
            return self.inner.on_request(context).await;
        }
        // Block List — до скрипта: заблокированный запрос скрипт не видит.
        match self.inner.on_request(context.clone()).await {
            outcome @ (InterceptOutcome::Abort { .. }
            | InterceptOutcome::Proceed {
                route: RouteDecision::Blocked(_),
                ..
            }) => return outcome,
            InterceptOutcome::Proceed { .. } => {}
        }
        match self.script_on_request(&context, &rule).await {
            InterceptOutcome::Proceed {
                message,
                route: RouteDecision::Origin,
            } => {
                // Map Local/Remote — по изменённому запросу.
                let edited = InterceptContext {
                    message: message.clone(),
                    ..context
                };
                match self.inner.on_request(edited).await {
                    InterceptOutcome::Proceed { route, .. } => {
                        InterceptOutcome::Proceed { message, route }
                    }
                    abort => abort,
                }
            }
            other => other,
        }
    }

    async fn on_response(&self, context: InterceptContext) -> InterceptOutcome {
        let Some(rule) = self.response_script(&context, true) else {
            return self.inner.on_response(context).await;
        };
        let request = match &context.request {
            Some(request) => self.script_request(request).await,
            None => return self.inner.on_response(context).await,
        };
        let before = self.script_response(&context.message).await;
        let message = match self
            .run(&rule, context.exchange_id, request, Some(before.clone()))
            .await
        {
            Some(ScriptOutput::Response(after)) => apply_response(&context.message, &before, after),
            _ => context.message.clone(),
        };
        self.inner
            .on_response(InterceptContext { message, ..context })
            .await
    }

    fn update_rules(&self, rules: RuleSet) {
        *self.rules.write().expect("rules lock") = rules.clone();
        self.inner.update_rules(rules);
    }

    fn resolve_intercept(&self, id: InterceptId, resolution: InterceptResolutionDto) {
        self.inner.resolve_intercept(id, resolution);
    }
}

#[cfg(test)]
#[path = "scripting_pipeline_tests.rs"]
mod tests;
