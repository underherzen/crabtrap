//! Пайплайн перехвата по правилам (Phase 6): Block List, Map Local,
//! Map Remote. Решение «какое правило» — доменное
//! ([`RuleSet::route_request`]); здесь — проекция в [`RouteDecision`]:
//! итоговый файл Map Local, итоговый URL Map Remote.
//!
//! Снапшот правил подменяется атомарно ([`InterceptionPipeline::update_rules`]),
//! соединения не рвутся. Методы infallible: непарсимый URI → Origin.

use std::path::PathBuf;
use std::sync::RwLock;

use application::matching::RequestTarget;
use application::services::{
    AbortMode, ConnectDecision, InterceptContext, InterceptOutcome, InterceptionPipeline,
    RouteDecision,
};
use async_trait::async_trait;
use domain::entities::RequestRoute;
use domain::entities::rule::BreakPhase;
use domain::entities::rule::{BlockAction, MapRemoteRule, RuleSet};
use domain::value_objects::Host;

/// Файл, который отдаётся для запроса каталога (`/` на конце).
const DIRECTORY_INDEX: &str = "index.html";

pub struct RulesInterceptionPipeline {
    rules: RwLock<RuleSet>,
}

impl RulesInterceptionPipeline {
    pub fn new(rules: RuleSet) -> Self {
        Self {
            rules: RwLock::new(rules),
        }
    }

    fn snapshot(&self) -> RuleSet {
        self.rules.read().expect("rules lock").clone()
    }
}

#[async_trait]
impl InterceptionPipeline for RulesInterceptionPipeline {
    async fn on_connect(&self, host: &Host, _port: u16) -> ConnectDecision {
        let rules = self.rules.read().expect("rules lock");
        if rules.connect_blocked(host.as_str()).is_some() {
            ConnectDecision::Block
        } else if !rules.records_host(host.as_str()) {
            ConnectDecision::Bypass
        } else {
            // MITM/туннель решает SSL-Proxying список движка.
            ConnectDecision::Passthrough
        }
    }

    fn records_host(&self, host: &str) -> bool {
        self.rules.read().expect("rules lock").records_host(host)
    }

    /// Самый внутренний пайплайн цепочки — здесь решается, нужно ли тело
    /// запроса ради имени GraphQL-операции (правилу любого инструмента).
    fn wants_body(&self, context: &InterceptContext) -> bool {
        context.phase == BreakPhase::Request
            && RequestTarget::of_message(&context.message, false).is_some_and(|target| {
                self.rules
                    .read()
                    .expect("rules lock")
                    .graphql_body_needed(&target.facts())
            })
    }

    async fn on_request(&self, context: InterceptContext) -> InterceptOutcome {
        let rules = self.snapshot();
        let route = match RequestTarget::of_message(&context.message, rules.uses_graphql()) {
            Some(target) => request_route(&rules, &target),
            None => Ok(RouteDecision::Origin),
        };
        match route {
            Ok(route) => InterceptOutcome::Proceed {
                message: context.message,
                route,
            },
            // Обрыв на фазе запроса даёт только Block List (действие Close).
            Err(mode) => InterceptOutcome::Abort {
                mode,
                tag: Some("blocked"),
            },
        }
    }

    async fn on_bypassed_request(&self, context: InterceptContext) -> InterceptOutcome {
        let rules = self.snapshot();
        let block = RequestTarget::of_message(&context.message, false)
            .and_then(|target| rules.block_for(&target.facts()).map(|rule| rule.action));
        match block.map(block_outcome) {
            Some(Ok(route)) => InterceptOutcome::Proceed {
                message: context.message,
                route,
            },
            Some(Err(mode)) => InterceptOutcome::Abort {
                mode,
                tag: Some("blocked"),
            },
            None => InterceptOutcome::Proceed {
                message: context.message,
                route: RouteDecision::Origin,
            },
        }
    }

    async fn on_response(&self, context: InterceptContext) -> InterceptOutcome {
        InterceptOutcome::Proceed {
            message: context.message,
            route: RouteDecision::Origin,
        }
    }

    fn update_rules(&self, rules: RuleSet) {
        *self.rules.write().expect("rules lock") = rules;
    }
}

/// Маршрут запроса; `Err` — оборвать соединение (Block с действием Close).
fn request_route(rules: &RuleSet, target: &RequestTarget) -> Result<RouteDecision, AbortMode> {
    decide(rules.route_request(&target.facts()), target.url())
}

/// Относительный путь файла для правила-каталога Map Local: path запроса
/// (без query) декодируется по сегментам; `..`/`.`/пустые сегменты
/// выкидываются; `/` на конце → index.html. `None` — путь отклонён:
/// скрытый сегмент (`.env`, `.git`) или закодированный разделитель
/// (`%2F`, `%5C` — иначе `..%2F` снова открыл бы выход из каталога).
/// Без IO: каталог/файл, симлинки и тип файла проверяет движок.
pub fn map_local_relative(url_path: &str) -> Option<PathBuf> {
    let mut relative = PathBuf::new();
    for raw in url_path.split('/') {
        let segment = percent_encoding::percent_decode_str(raw)
            .decode_utf8()
            .ok()?;
        match segment.as_ref() {
            "" | "." | ".." => continue,
            s if s.contains(['/', '\\']) || s.starts_with('.') => return None,
            s => relative.push(s),
        }
    }
    if url_path.ends_with('/') || relative.as_os_str().is_empty() {
        relative.push(DIRECTORY_INDEX);
    }
    Some(relative)
}

/// Итоговый URL Map Remote: схема/хост/порт — из `remote`; path — из
/// `remote`, если он задан (не пустой и не `/`), иначе исходный; query
/// исходный, если у `remote` своего нет. `None` — `remote` не URL.
pub fn map_remote_url(rule: &MapRemoteRule, original: &url::Url) -> Option<url::Url> {
    let remote = url::Url::parse(&rule.remote).ok()?;
    let mut target = original.clone();
    target.set_scheme(remote.scheme()).ok()?;
    target.set_host(remote.host_str()).ok()?;
    target.set_port(remote.port()).ok()?;
    if !matches!(remote.path(), "" | "/") {
        target.set_path(remote.path());
    }
    if remote.query().is_some() {
        target.set_query(remote.query());
    }
    Some(target)
}

/// Действие Block → маршрут или обрыв.
pub fn block_outcome(action: BlockAction) -> Result<RouteDecision, AbortMode> {
    match action {
        BlockAction::Status(status) => Ok(RouteDecision::Blocked(status)),
        BlockAction::Close => Err(AbortMode::Close),
    }
}

/// Решение по доменному маршруту.
pub fn decide(route: RequestRoute<'_>, url: &url::Url) -> Result<RouteDecision, AbortMode> {
    match route {
        RequestRoute::Block(rule) => block_outcome(rule.action),
        RequestRoute::MapLocal(rule) => Ok(RouteDecision::MapLocal {
            root: rule.local_path.clone(),
            relative: map_local_relative(url.path()),
            status: rule.status,
            content_type: rule.content_type.clone(),
            headers: rule.headers.clone(),
        }),
        RequestRoute::MapRemote(rule) => Ok(match map_remote_url(rule, url) {
            Some(target) => RouteDecision::MapRemote {
                url: target,
                preserve_host: rule.preserve_host,
            },
            None => RouteDecision::Origin,
        }),
        RequestRoute::Origin => Ok(RouteDecision::Origin),
    }
}

#[cfg(test)]
#[path = "rules_pipeline_tests.rs"]
mod tests;
