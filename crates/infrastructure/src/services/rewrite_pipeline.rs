//! Пайплайн Rewrite (Phase 7) поверх пайплайна правил: совпавшие
//! Rewrite-правила меняют запрос до Map Local/Remote (маршрут считается по
//! переписанному URL) и ответ до отдачи клиенту. Что делают операции —
//! домен ([`RewriteRule::apply`]); здесь — тела (распаковка, как у
//! скриптов) и проекция в [`InterceptableMessage`].

use std::sync::{Arc, RwLock};

use application::dto::rules::InterceptResolutionDto;
use application::services::{
    BodyDecoderService, ConnectDecision, InterceptContext, InterceptOutcome, InterceptableMessage,
    InterceptionPipeline,
};
use async_trait::async_trait;
use domain::entities::rule::{BreakPhase, RewriteRule, RuleSet};
use domain::entities::{RequestFacts, ResponseFacts, RewriteTarget};
use domain::value_objects::Host;
use domain::value_objects::InterceptId;

use super::message_body::{MessageBodies, finish_body};

pub struct RewriteInterceptionPipeline {
    inner: Arc<dyn InterceptionPipeline>,
    rules: RwLock<RuleSet>,
    bodies: MessageBodies,
}

impl RewriteInterceptionPipeline {
    pub fn new(
        inner: Arc<dyn InterceptionPipeline>,
        rules: RuleSet,
        decoder: Arc<dyn BodyDecoderService>,
    ) -> Self {
        Self {
            inner,
            rules: RwLock::new(rules),
            bodies: MessageBodies::new(decoder),
        }
    }

    /// Правила фазы: запрос — по нему самому; ответ — по итоговому запросу
    /// и статусу / Content-Type ответа.
    fn rules_for(&self, context: &InterceptContext) -> Vec<RewriteRule> {
        let request = match context.phase {
            BreakPhase::Request => &context.message,
            BreakPhase::Response | BreakPhase::Both => match &context.request {
                Some(request) => request,
                None => return Vec::new(),
            },
        };
        let Some((host, path)) = request.uri.as_deref().and_then(host_and_path) else {
            return Vec::new();
        };
        let facts = RequestFacts {
            host: &host,
            path: &path,
            method: request.method.as_deref().unwrap_or_default(),
        };
        let content_type = header_value(&context.message, "content-type");
        let response = match context.phase {
            BreakPhase::Request => None,
            _ => Some(ResponseFacts {
                status: context.message.status.unwrap_or_default(),
                content_type: content_type.as_deref(),
            }),
        };
        let rules = self.rules.read().expect("rules lock");
        rules
            .rewrites_for(&facts, response)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Применить правила к сообщению.
    async fn rewrite(
        &self,
        message: &InterceptableMessage,
        rules: &[RewriteRule],
    ) -> InterceptableMessage {
        let touches_body = rules.iter().any(RewriteRule::touches_body);
        let before = match touches_body {
            true => self.bodies.plain(message).await,
            false => None,
        };
        let target = RewriteTarget {
            url: message.uri.clone(),
            status: message.status,
            headers: message.headers.clone(),
            body: before.clone(),
        };
        let after = rules.iter().fold(target, |target, rule| rule.apply(target));
        let (headers, body) = match touches_body {
            true => finish_body(message, before.as_deref(), after.body, after.headers),
            false => (after.headers, message.body.clone()),
        };
        InterceptableMessage {
            uri: after.url,
            status: after.status,
            headers,
            body,
            ..message.clone()
        }
    }

    async fn rewritten(&self, context: InterceptContext) -> InterceptContext {
        let rules = self.rules_for(&context);
        if rules.is_empty() {
            return context;
        }
        let message = self.rewrite(&context.message, &rules).await;
        InterceptContext { message, ..context }
    }
}

fn header_value(message: &InterceptableMessage, name: &str) -> Option<String> {
    message
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.clone())
}

/// Хост и path+query absolute-URI.
fn host_and_path(uri: &str) -> Option<(String, String)> {
    let url = url::Url::parse(uri).ok()?;
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    Some((url.host_str().unwrap_or_default().to_string(), path))
}

#[async_trait]
impl InterceptionPipeline for RewriteInterceptionPipeline {
    async fn on_connect(&self, host: &Host, port: u16) -> ConnectDecision {
        self.inner.on_connect(host, port).await
    }

    fn wants_body(&self, context: &InterceptContext) -> bool {
        self.rules_for(context)
            .iter()
            .any(RewriteRule::touches_body)
            || self.inner.wants_body(context)
    }

    async fn on_request(&self, context: InterceptContext) -> InterceptOutcome {
        let context = self.rewritten(context).await;
        self.inner.on_request(context).await
    }

    async fn on_response(&self, context: InterceptContext) -> InterceptOutcome {
        let context = self.rewritten(context).await;
        self.inner.on_response(context).await
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
#[path = "rewrite_pipeline_tests.rs"]
mod tests;
