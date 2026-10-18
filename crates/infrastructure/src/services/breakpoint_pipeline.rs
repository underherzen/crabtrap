//! Брейкпоинты (Phase 7): совпавший обмен паркуется до решения
//! пользователя. Пайплайн публикует `InterceptWaiting` с сообщением
//! (тело распаковано — его можно править текстом), ждёт
//! `EngineCommand::ResolveIntercept` на oneshot и продолжает с правками
//! (Execute) или обрывает обмен (Abort). Нет решения дольше таймаута или
//! клиент ушёл — обмен идёт как был, `InterceptResolved` закрывает окно.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::rules::{
    InterceptPreviewDto, InterceptResolutionDto, InterceptableMessageDto,
};
use application::services::{
    AbortMode, BodyDecoderService, ConnectDecision, EventPublisher, InterceptContext,
    InterceptOutcome, InterceptableMessage, InterceptionPipeline,
};
use async_trait::async_trait;
use chrono::Utc;
use domain::entities::RequestFacts;
use domain::entities::rule::{BreakPhase, BreakpointRule, RuleSet};
use domain::value_objects::{Host, InterceptId};
use futures::channel::oneshot;

use super::message_body::{MessageBodies, finish_body};

/// Сколько обмен ждёт решения по умолчанию.
pub const DEFAULT_BREAKPOINT_TIMEOUT: Duration = Duration::from_secs(300);

type Waiting = Arc<Mutex<HashMap<InterceptId, oneshot::Sender<InterceptResolutionDto>>>>;

pub struct BreakpointInterceptionPipeline {
    inner: Arc<dyn InterceptionPipeline>,
    rules: RwLock<RuleSet>,
    events: Arc<dyn EventPublisher>,
    bodies: MessageBodies,
    waiting: Waiting,
    timeout: Duration,
}

/// Исход парковки.
enum Parked {
    Proceed(InterceptableMessage),
    Abort,
}

/// Снять парковку при любом исходе (решение, таймаут, отмена future —
/// клиент закрыл соединение): окно брейкпоинта в UI закрывается.
struct ParkGuard {
    waiting: Waiting,
    events: Arc<dyn EventPublisher>,
    id: InterceptId,
}

impl Drop for ParkGuard {
    fn drop(&mut self) {
        self.waiting.lock().expect("waiting lock").remove(&self.id);
        self.events
            .publish(ProxyEvent::InterceptResolved { id: self.id });
    }
}

impl BreakpointInterceptionPipeline {
    pub fn new(
        inner: Arc<dyn InterceptionPipeline>,
        rules: RuleSet,
        events: Arc<dyn EventPublisher>,
        decoder: Arc<dyn BodyDecoderService>,
    ) -> Self {
        Self {
            inner,
            rules: RwLock::new(rules),
            events,
            bodies: MessageBodies::new(decoder),
            waiting: Arc::new(Mutex::new(HashMap::new())),
            timeout: DEFAULT_BREAKPOINT_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn rule_for(&self, context: &InterceptContext) -> Option<BreakpointRule> {
        let (request, phase) = match context.phase {
            BreakPhase::Request => (&context.message, BreakPhase::Request),
            BreakPhase::Response | BreakPhase::Both => {
                (context.request.as_ref()?, BreakPhase::Response)
            }
        };
        let url = url::Url::parse(request.uri.as_deref()?).ok()?;
        let path = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_string(),
        };
        let facts = RequestFacts {
            host: url.host_str().unwrap_or_default(),
            path: &path,
            method: request.method.as_deref().unwrap_or_default(),
        };
        let rules = self.rules.read().expect("rules lock");
        rules.breakpoint_for(&facts, phase).cloned()
    }

    /// Припарковать сообщение и дождаться решения.
    async fn park(&self, context: &InterceptContext) -> Parked {
        let message = &context.message;
        let plain = self.bodies.plain(message).await;
        let id = InterceptId::new();
        let (reply, decision) = oneshot::channel();
        self.waiting.lock().expect("waiting lock").insert(id, reply);
        let _guard = ParkGuard {
            waiting: Arc::clone(&self.waiting),
            events: Arc::clone(&self.events),
            id,
        };
        let url = match context.phase {
            BreakPhase::Request => message.uri.clone(),
            _ => context.request.as_ref().and_then(|r| r.uri.clone()),
        };
        self.events.publish(ProxyEvent::InterceptWaiting {
            preview: InterceptPreviewDto {
                intercept_id: id,
                exchange_id: context.exchange_id,
                phase: context.phase,
                url: url.unwrap_or_default(),
                message: to_dto(message, plain.clone()),
            },
            ts: Utc::now(),
        });
        let resolution = match tokio::time::timeout(self.timeout, decision).await {
            Ok(Ok(resolution)) => resolution,
            Ok(Err(_)) | Err(_) => InterceptResolutionDto::Resume { edited: None },
        };
        match resolution {
            InterceptResolutionDto::Abort => Parked::Abort,
            InterceptResolutionDto::Resume { edited: None } => Parked::Proceed(message.clone()),
            InterceptResolutionDto::Resume {
                edited: Some(edited),
            } => Parked::Proceed(apply(message, plain.as_deref(), edited)),
        }
    }
}

fn to_dto(message: &InterceptableMessage, body: Option<Vec<u8>>) -> InterceptableMessageDto {
    InterceptableMessageDto {
        method: message.method.clone(),
        uri: message.uri.clone(),
        status: message.status,
        reason: message.reason.clone(),
        headers: message.headers.clone(),
        body,
    }
}

/// Правки из окна поверх исходного сообщения. Тело: не загружено — уходит
/// как было; не изменено — исходные байты (сжатые); изменено — новый
/// текст без Content-Encoding.
fn apply(
    original: &InterceptableMessage,
    plain: Option<&[u8]>,
    edited: InterceptableMessageDto,
) -> InterceptableMessage {
    let (headers, body) = match plain {
        Some(_) => finish_body(original, plain, edited.body, edited.headers),
        None => (edited.headers, original.body.clone()),
    };
    InterceptableMessage {
        method: if original.is_request() {
            edited.method.or(original.method.clone())
        } else {
            original.method.clone()
        },
        uri: if original.is_request() {
            edited.uri.or(original.uri.clone())
        } else {
            original.uri.clone()
        },
        status: if original.is_request() {
            original.status
        } else {
            edited.status.or(original.status)
        },
        headers,
        body,
        ..original.clone()
    }
}

fn aborted() -> InterceptOutcome {
    InterceptOutcome::Abort {
        mode: AbortMode::Close,
        tag: Some("breakpoint-aborted"),
    }
}

#[async_trait]
impl InterceptionPipeline for BreakpointInterceptionPipeline {
    async fn on_connect(&self, host: &Host, port: u16) -> ConnectDecision {
        self.inner.on_connect(host, port).await
    }

    fn wants_body(&self, context: &InterceptContext) -> bool {
        self.rule_for(context).is_some() || self.inner.wants_body(context)
    }

    async fn on_request(&self, context: InterceptContext) -> InterceptOutcome {
        if self.rule_for(&context).is_none() {
            return self.inner.on_request(context).await;
        }
        match self.park(&context).await {
            Parked::Proceed(message) => {
                self.inner
                    .on_request(InterceptContext { message, ..context })
                    .await
            }
            Parked::Abort => aborted(),
        }
    }

    async fn on_response(&self, context: InterceptContext) -> InterceptOutcome {
        if self.rule_for(&context).is_none() {
            return self.inner.on_response(context).await;
        }
        match self.park(&context).await {
            Parked::Proceed(message) => {
                self.inner
                    .on_response(InterceptContext { message, ..context })
                    .await
            }
            Parked::Abort => aborted(),
        }
    }

    fn update_rules(&self, rules: RuleSet) {
        *self.rules.write().expect("rules lock") = rules.clone();
        self.inner.update_rules(rules);
    }

    fn resolve_intercept(&self, id: InterceptId, resolution: InterceptResolutionDto) {
        let parked = self.waiting.lock().expect("waiting lock").remove(&id);
        match parked {
            Some(reply) => {
                let _ = reply.send(resolution);
            }
            None => self.inner.resolve_intercept(id, resolution),
        }
    }
}

#[cfg(test)]
#[path = "breakpoint_pipeline_tests.rs"]
mod tests;
