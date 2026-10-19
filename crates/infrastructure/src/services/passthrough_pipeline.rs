//! Пайплайн-заглушка: все соединения — туннелем, все сообщения — в origin.
//!
//! Нужен тестам движка/рантайма, где правила не важны; приложение собирает
//! настоящую цепочку (Rules → Breakpoints → Rewrite → Scripting) в
//! `app/main.rs`. Методы обязаны быть infallible — см. порт.

use application::services::InterceptionPipeline;
use application::services::{ConnectDecision, InterceptContext, InterceptOutcome, RouteDecision};
use async_trait::async_trait;
use domain::value_objects::Host;

pub struct PassthroughInterceptionPipeline;

impl Default for PassthroughInterceptionPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl PassthroughInterceptionPipeline {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl InterceptionPipeline for PassthroughInterceptionPipeline {
    async fn on_connect(&self, _host: &Host, _port: u16) -> ConnectDecision {
        ConnectDecision::Passthrough
    }

    async fn on_request(&self, context: InterceptContext) -> InterceptOutcome {
        InterceptOutcome::Proceed {
            message: context.message,
            route: RouteDecision::Origin,
        }
    }

    async fn on_response(&self, context: InterceptContext) -> InterceptOutcome {
        InterceptOutcome::Proceed {
            message: context.message,
            route: RouteDecision::Origin,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use application::services::InterceptableMessage;

    fn message() -> InterceptableMessage {
        InterceptableMessage {
            method: Some("GET".to_string()),
            uri: Some("http://example.com/".to_string()),
            version: None,
            status: None,
            reason: None,
            headers: Vec::new(),
            body: None,
        }
    }

    #[tokio::test]
    async fn everything_proceeds_to_origin() {
        let pipeline = PassthroughInterceptionPipeline::new();
        let ctx = InterceptContext {
            exchange_id: domain::value_objects::ExchangeId::new(),
            phase: domain::entities::rule::BreakPhase::Request,
            message: message(),
            request: None,
        };

        assert_eq!(
            pipeline
                .on_connect(&Host::new("example.com").unwrap(), 443)
                .await,
            ConnectDecision::Passthrough
        );
        match pipeline.on_request(ctx).await {
            InterceptOutcome::Proceed { route, .. } => assert_eq!(route, RouteDecision::Origin),
            other => panic!("expected Proceed, got {other:?}"),
        }
    }
}
