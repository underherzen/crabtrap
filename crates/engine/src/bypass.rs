//! Обмен мимо crabtrap (хост не в Allow List): пайплайн, который
//! применяет только Block List, и публикатор, который ничего не публикует —
//! движок идёт обычным путём проксирования, но без записи и прочих правил.

use application::dto::capture::ProxyEvent;
use application::services::{
    ConnectDecision, EventPublisher, InterceptContext, InterceptOutcome, InterceptionPipeline,
    RouteDecision,
};
use std::sync::Arc;

use async_trait::async_trait;
use domain::value_objects::Host;

/// Публикатор-заглушка: события обмена мимо записи никуда не идут.
pub(crate) struct SilentPublisher;

impl EventPublisher for SilentPublisher {
    fn publish(&self, _event: ProxyEvent) {}
}

/// Пайплайн обмена мимо записи: из правил — только Block List настоящего
/// пайплайна (`on_bypassed_request`), ответ — клиенту как есть.
pub(crate) struct BypassPipeline {
    pub(crate) inner: Arc<dyn InterceptionPipeline>,
}

#[async_trait]
impl InterceptionPipeline for BypassPipeline {
    async fn on_connect(&self, _host: &Host, _port: u16) -> ConnectDecision {
        ConnectDecision::Bypass
    }

    async fn on_request(&self, context: InterceptContext) -> InterceptOutcome {
        self.inner.on_bypassed_request(context).await
    }

    async fn on_response(&self, context: InterceptContext) -> InterceptOutcome {
        InterceptOutcome::Proceed {
            message: context.message,
            route: RouteDecision::Origin,
        }
    }
}
