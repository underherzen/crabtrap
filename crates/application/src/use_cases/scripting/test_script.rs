//! Пробный запуск скрипта из редактора (кнопка Test): последний
//! захваченный обмен, подходящий под Matching Rule, прогоняется через
//! черновик скрипта без сети. Итог — строки для консоли: что скрипт
//! изменил бы в запросе и ответе, плюс его собственный вывод.

use std::sync::Arc;

use domain::entities::BodyRef;
use domain::entities::exchange::HttpExchange;
use domain::errors::DomainError;
use domain::repositories::{BodyRepository, ExchangeRepository};
use domain::value_objects::filter_query::FilterQuery;
use domain::value_objects::http_vo::HeaderEntry;
use domain::value_objects::{RuleId, TrafficMatch};

use crate::dto::scripting::{
    ScriptCall, ScriptLogDto, ScriptLogLevel, ScriptOutput, ScriptRequestDto, ScriptResponseDto,
    ScriptSourceDto,
};
use crate::services::{BodyDecoderService, ScriptRuntime};
use crate::use_cases::rules::script_edit::ScriptDraft;

/// Сколько последних обменов просматривается в поиске подходящего.
const SEARCH_DEPTH: usize = 5000;

pub struct TestScriptUseCase {
    exchanges: Arc<dyn ExchangeRepository>,
    bodies: Arc<dyn BodyRepository>,
    decoder: Arc<dyn BodyDecoderService>,
    runtime: Arc<dyn ScriptRuntime>,
}

/// Зависимости пробного запуска.
pub struct TestScriptDeps {
    pub exchanges: Arc<dyn ExchangeRepository>,
    pub bodies: Arc<dyn BodyRepository>,
    pub decoder: Arc<dyn BodyDecoderService>,
    pub runtime: Arc<dyn ScriptRuntime>,
}

impl TestScriptUseCase {
    pub fn new(deps: TestScriptDeps) -> Self {
        Self {
            exchanges: deps.exchanges,
            bodies: deps.bodies,
            decoder: deps.decoder,
            runtime: deps.runtime,
        }
    }

    /// Прогнать черновик `draft` (правило `rule_id`) на последнем подходящем
    /// обмене. `NotFound` — подходящего обмена в трафике нет.
    pub async fn execute(
        &self,
        rule_id: RuleId,
        draft: &ScriptDraft,
    ) -> Result<Vec<ScriptLogDto>, DomainError> {
        let location = draft.location.to_match().map_err(|errors| {
            let message = [errors.host, errors.path]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; ");
            DomainError::ValidationError(message)
        })?;
        let exchange = self.latest_matching(&location).await?;
        let script = ScriptSourceDto {
            rule_id,
            name: draft.name.clone(),
            source: draft.source.clone(),
        };
        let mut lines = vec![info(format!(
            "Test on {} {} — captured {}",
            exchange.request.method,
            exchange.request.uri,
            exchange.started_at.format("%H:%M:%S")
        ))];

        let request = self.request_dto(&exchange).await;
        let request = if draft.run_on_request || draft.mock_api {
            let (edited, mut logs) = self.run(&script, request.clone(), None).await;
            lines.append(&mut logs);
            let edited = match edited {
                Some(ScriptOutput::Request(edited)) => edited,
                _ => request.clone(),
            };
            lines.extend(describe_request(&request, &edited).into_iter().map(info));
            edited
        } else {
            request
        };

        let response = match (draft.mock_api, &exchange.response) {
            (true, _) => Some(ScriptResponseDto::blank()),
            (false, Some(_)) if draft.run_on_response => Some(self.response_dto(&exchange).await),
            (false, None) if draft.run_on_response => {
                lines.push(info("Response: none captured for this request".into()));
                None
            }
            _ => None,
        };
        if let Some(response) = response {
            let (edited, mut logs) = self.run(&script, request, Some(response.clone())).await;
            lines.append(&mut logs);
            if let Some(ScriptOutput::Response(edited)) = edited {
                let label = if draft.mock_api {
                    "Mock response"
                } else {
                    "Response"
                };
                lines.extend(
                    describe_response(label, &response, &edited)
                        .into_iter()
                        .map(info),
                );
            }
        }
        Ok(lines)
    }

    async fn latest_matching(&self, location: &TrafficMatch) -> Result<HttpExchange, DomainError> {
        let recent = self
            .exchanges
            .list(FilterQuery::default(), 0, SEARCH_DEPTH)
            .await?;
        recent
            .into_iter()
            .find(|exchange| {
                let (host, path) = host_and_path(exchange);
                location.matches(
                    &host,
                    &path,
                    &exchange.request.method.to_string(),
                    None,
                    None,
                )
            })
            .ok_or_else(|| {
                DomainError::NotFound(
                    "no captured request matches this rule yet — send one through the proxy".into(),
                )
            })
    }

    async fn run(
        &self,
        script: &ScriptSourceDto,
        request: ScriptRequestDto,
        response: Option<ScriptResponseDto>,
    ) -> (Option<ScriptOutput>, Vec<ScriptLogDto>) {
        let run = self
            .runtime
            .run(ScriptCall {
                script: script.clone(),
                exchange_id: None,
                request,
                response,
            })
            .await;
        (run.output.ok(), run.logs)
    }

    async fn request_dto(&self, exchange: &HttpExchange) -> ScriptRequestDto {
        let headers = exchange.request.headers.clone();
        ScriptRequestDto {
            method: exchange.request.method.to_string(),
            url: absolute_url(exchange),
            body: self.body(exchange.request.body.as_ref(), &headers).await,
            headers,
        }
    }

    async fn response_dto(&self, exchange: &HttpExchange) -> ScriptResponseDto {
        let Some(response) = &exchange.response else {
            return ScriptResponseDto::blank();
        };
        ScriptResponseDto {
            status: response.status,
            body: self.body(response.body.as_ref(), &response.headers).await,
            headers: response.headers.clone(),
        }
    }

    /// Захваченное тело, распакованное по Content-Encoding (`None` — не
    /// захвачено или не читается).
    async fn body(&self, body: Option<&BodyRef>, headers: &[HeaderEntry]) -> Option<Vec<u8>> {
        let Some(body) = body else {
            return Some(Vec::new());
        };
        let raw = self.bodies.get(body).await.ok().flatten()?;
        let encoding = headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("content-encoding"))
            .map(|h| h.value.clone())
            .unwrap_or_default();
        self.decoder.decode(&raw, &encoding).await.ok()
    }
}

fn info(message: String) -> ScriptLogDto {
    ScriptLogDto {
        level: ScriptLogLevel::Info,
        message,
    }
}

/// Хост и path+query обмена (URI захвачен absolute-form).
fn host_and_path(exchange: &HttpExchange) -> (String, String) {
    match url::Url::parse(&exchange.request.uri) {
        Ok(url) => {
            let path = match url.query() {
                Some(query) => format!("{}?{query}", url.path()),
                None => url.path().to_string(),
            };
            (url.host_str().unwrap_or_default().to_string(), path)
        }
        Err(_) => (
            exchange.host.as_str().to_string(),
            exchange.request.uri.clone(),
        ),
    }
}

fn absolute_url(exchange: &HttpExchange) -> String {
    if url::Url::parse(&exchange.request.uri).is_ok() {
        return exchange.request.uri.clone();
    }
    format!("http://{}{}", exchange.host.as_str(), exchange.request.uri)
}

/// Что поменялось в заголовках: добавлены / удалены / изменены (имена).
fn describe_headers(before: &[HeaderEntry], after: &[HeaderEntry]) -> Option<String> {
    let names = |list: &[HeaderEntry]| -> Vec<String> {
        let mut names: Vec<String> = list.iter().map(|h| h.name.to_ascii_lowercase()).collect();
        names.dedup();
        names
    };
    let values = |list: &[HeaderEntry], name: &str| -> Vec<String> {
        list.iter()
            .filter(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.clone())
            .collect()
    };
    let (old, new) = (names(before), names(after));
    let added: Vec<_> = new.iter().filter(|n| !old.contains(n)).cloned().collect();
    let removed: Vec<_> = old.iter().filter(|n| !new.contains(n)).cloned().collect();
    let changed: Vec<_> = new
        .iter()
        .filter(|n| old.contains(n) && values(before, n) != values(after, n))
        .cloned()
        .collect();
    let parts: Vec<String> = [("added", added), ("removed", removed), ("changed", changed)]
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(what, names)| format!("{what} {}", names.join(", ")))
        .collect();
    (!parts.is_empty()).then(|| format!("headers {}", parts.join("; ")))
}

fn describe_body(before: Option<&Vec<u8>>, after: Option<&Vec<u8>>) -> Option<String> {
    (before != after).then(|| {
        let size = |b: Option<&Vec<u8>>| {
            b.map_or_else(|| "not loaded".to_string(), |b| format!("{} B", b.len()))
        };
        format!("body {} → {}", size(before), size(after))
    })
}

fn summarize(label: &str, changes: Vec<String>) -> Vec<String> {
    if changes.is_empty() {
        vec![format!("{label}: unchanged")]
    } else {
        vec![format!("{label}: {}", changes.join("; "))]
    }
}

pub fn describe_request(before: &ScriptRequestDto, after: &ScriptRequestDto) -> Vec<String> {
    let mut changes = Vec::new();
    if before.method != after.method {
        changes.push(format!("method {} → {}", before.method, after.method));
    }
    if before.url != after.url {
        changes.push(format!("URL → {}", after.url));
    }
    changes.extend(describe_headers(&before.headers, &after.headers));
    changes.extend(describe_body(before.body.as_ref(), after.body.as_ref()));
    summarize("Request", changes)
}

pub fn describe_response(
    label: &str,
    before: &ScriptResponseDto,
    after: &ScriptResponseDto,
) -> Vec<String> {
    let mut changes = Vec::new();
    if before.status != after.status {
        changes.push(format!("status {} → {}", before.status, after.status));
    }
    changes.extend(describe_headers(&before.headers, &after.headers));
    changes.extend(describe_body(before.body.as_ref(), after.body.as_ref()));
    summarize(label, changes)
}

#[cfg(test)]
#[path = "test_script_tests.rs"]
mod tests;
