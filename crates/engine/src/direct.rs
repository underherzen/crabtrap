//! Прямая отправка запросов из UI (Repeat / Edit & Resend / Compose,
//! Phase 7): мимо листенера, но тем же конвейером [`ProxyService::forward`]
//! — правила, скрипты, throttle и захват в трафик работают как для
//! обычного клиента; обмен получает тег `repeated` / `composed`.
//!
//! Транспорт — async-channel (D4): UI держит [`DirectHttpClient`], движок
//! на каждом запуске обслуживает очередь своим состоянием.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use application::dto::capture::{HeaderEntryDto, ProxyEvent};
use application::dto::compose::{ComposedRequestDto, SendResultDto};
use application::services::DirectHttpService;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use domain::entities::exchange::RequestOrigin;
use domain::errors::DomainError;
use domain::value_objects::ConnectionId;
use futures::channel::oneshot;
use http_body_util::BodyExt as _;

use crate::commands::{ConnGuard, EngineState};
use crate::http_proxy::{ProxyService, header_dtos};
use crate::intercept_body::full;

/// Сколько UI ждёт ответа. Дольше таймаута брейкпоинта (300 с): запрос
/// из Compose может стоять на брейкпоинте.
const SEND_TIMEOUT: Duration = Duration::from_secs(330);
/// Пауза потока ответа, после которой UI получает то, что пришло (SSE,
/// long-poll не держат окно вечно; в трафике обмен продолжается).
const RESPONSE_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Потолок тела ответа, возвращаемого в UI (в трафике — полный захват).
const RESPONSE_BODY_CAP: usize = 16 * 1024 * 1024;

/// Задание прямой отправки.
pub struct DirectJob {
    request: ComposedRequestDto,
    reply: oneshot::Sender<Result<SendResultDto, DomainError>>,
}

/// Сторона UI: реализует порт [`DirectHttpService`].
#[derive(Clone)]
pub struct DirectHttpClient {
    jobs: async_channel::Sender<DirectJob>,
    /// Движок сейчас обслуживает очередь (иначе — сразу ошибка, а не
    /// задание, которое выполнится через минуты после следующего Start).
    serving: Arc<AtomicBool>,
}

/// Очередь для [`crate::EngineDeps::direct_requests`] (живёт дольше
/// отдельных запусков движка).
#[derive(Clone)]
pub struct DirectRequests {
    queue: async_channel::Receiver<DirectJob>,
    serving: Arc<AtomicBool>,
}

/// Пара «клиент для UI — очередь для движка».
pub fn direct_channel() -> (DirectHttpClient, DirectRequests) {
    let (jobs, queue) = async_channel::bounded(64);
    let serving = Arc::new(AtomicBool::new(false));
    (
        DirectHttpClient {
            jobs,
            serving: Arc::clone(&serving),
        },
        DirectRequests { queue, serving },
    )
}

/// Флаг «очередь обслуживается», снимается при остановке движка (drop
/// задачи `serve`).
struct Serving(Arc<AtomicBool>);

impl Drop for Serving {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[async_trait]
impl DirectHttpService for DirectHttpClient {
    async fn send(&self, request: ComposedRequestDto) -> Result<SendResultDto, DomainError> {
        if !self.serving.load(Ordering::SeqCst) {
            return Err(DomainError::Conflict(
                "the proxy is not running — Start Proxy first".into(),
            ));
        }
        let (reply, answer) = oneshot::channel();
        self.jobs
            .try_send(DirectJob { request, reply })
            .map_err(|e| match e {
                async_channel::TrySendError::Full(_) => {
                    DomainError::Conflict("too many requests in flight".into())
                }
                async_channel::TrySendError::Closed(_) => {
                    DomainError::InfrastructureError("proxy engine is gone".into())
                }
            })?;
        match tokio::time::timeout(SEND_TIMEOUT, answer).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(DomainError::InfrastructureError(
                "proxy engine stopped while sending".into(),
            )),
            Err(_) => Err(DomainError::InfrastructureError(
                "no answer — is the proxy running? (Start Proxy)".into(),
            )),
        }
    }
}

impl DirectRequests {
    /// Движок готов принимать: выбросить задания прошлого запуска (их
    /// ожидающие уже получили ошибку) и открыть очередь. Синхронно — до
    /// объявления листенера, чтобы первый же запрос из UI не получил
    /// «прокси не запущен».
    pub(crate) fn open(&self) {
        while self.queue.try_recv().is_ok() {}
        self.serving.store(true, Ordering::SeqCst);
    }
}

/// Обслуживать очередь, пока движок работает. Stop прерывает задачу —
/// вместе с ней (JoinSet) и запросы в полёте; флаг `serving` снимается.
pub(crate) async fn serve(
    state: Arc<EngineState>,
    semaphore: Arc<tokio::sync::Semaphore>,
    requests: DirectRequests,
) {
    let _serving = Serving(Arc::clone(&requests.serving));
    let mut in_flight = tokio::task::JoinSet::new();
    while let Ok(job) = requests.queue.recv().await {
        while in_flight.try_join_next().is_some() {}
        if job.reply.is_canceled() {
            continue;
        }
        let (state, semaphore) = (Arc::clone(&state), Arc::clone(&semaphore));
        in_flight.spawn(async move {
            let result = send(state, semaphore, job.request).await;
            let _ = job.reply.send(result);
        });
    }
}

async fn send(
    state: Arc<EngineState>,
    semaphore: Arc<tokio::sync::Semaphore>,
    dto: ComposedRequestDto,
) -> Result<SendResultDto, DomainError> {
    let request = build_request(&dto)?;
    let permit = semaphore
        .try_acquire_owned()
        .map_err(|_| DomainError::Conflict("connection limit reached, try again".into()))?;
    let conn_id = ConnectionId::new();
    state.events.publish(ProxyEvent::ConnectionOpened {
        id: conn_id,
        peer: "crabtrap".into(),
        ts: Utc::now(),
    });
    state.conn_opened();
    let guard = ConnGuard::new(Arc::clone(&state), conn_id, permit);
    let service = ProxyService::new(state, conn_id, guard).with_origin(dto.origin);
    let response = service.forward(request).await.map_err(|_| {
        DomainError::InfrastructureError(
            "connection closed (Block List or breakpoint Abort)".into(),
        )
    })?;
    let (parts, body) = response.into_parts();
    let (body, error) = read_capped(body).await;
    Ok(SendResultDto {
        status: parts.status.as_u16(),
        headers: header_dtos(&parts.headers),
        body,
        timing: Default::default(),
        error,
    })
}

/// Тело ответа для UI: до потолка и до паузы потока. Остаток дочитывается
/// в фоне — обмен в трафике завершается как обычно.
async fn read_capped(mut body: crate::body::MonitoredBody) -> (Option<Vec<u8>>, Option<String>) {
    let mut out = Vec::new();
    let error = loop {
        match tokio::time::timeout(RESPONSE_IDLE_TIMEOUT, body.frame()).await {
            Err(_) => break Some("response is still streaming — showing what arrived".to_string()),
            Ok(None) => break None,
            Ok(Some(Err(e))) => break Some(format!("response body failed: {e}")),
            Ok(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let room = RESPONSE_BODY_CAP.saturating_sub(out.len());
                    out.extend_from_slice(&data[..data.len().min(room)]);
                }
            }
        }
    };
    if error.is_some() {
        tokio::spawn(async move { while let Some(Ok(_)) = body.frame().await {} });
    }
    (Some(out), error)
}

/// DTO → absolute-form запрос с буферным телом.
fn build_request(
    dto: &ComposedRequestDto,
) -> Result<http::Request<crate::intercept_body::PipeBody>, DomainError> {
    let invalid = |what: String| DomainError::ValidationError(what);
    let uri: http::Uri = dto
        .url
        .trim()
        .parse()
        .map_err(|e| invalid(format!("URL is invalid: {e}")))?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.authority().is_none() {
        return Err(invalid("URL must be absolute http(s)://host/…".into()));
    }
    let method = http::Method::from_bytes(dto.method.trim().as_bytes())
        .map_err(|_| invalid(format!("method '{}' is invalid", dto.method)))?;
    let mut builder = http::Request::builder().method(method).uri(uri);
    for HeaderEntryDto { name, value } in &dto.headers {
        builder = builder.header(name.trim(), value.as_str());
    }
    let body = dto.body.clone().map(Bytes::from).unwrap_or_default();
    let mut request = builder
        .body(full(body.clone()))
        .map_err(|e| invalid(format!("request is invalid: {e}")))?;
    // Тело буферное: точная длина вместо chunked (ручной Transfer-Encoding /
    // Content-Length из формы заменяются).
    let headers = request.headers_mut();
    headers.remove(http::header::TRANSFER_ENCODING);
    if body.is_empty() && !headers.contains_key(http::header::CONTENT_LENGTH) {
        return Ok(request);
    }
    headers.insert(
        http::header::CONTENT_LENGTH,
        http::HeaderValue::from(body.len()),
    );
    Ok(request)
}

/// Тег строки трафика для происхождения запроса.
pub(crate) fn origin_tag(origin: RequestOrigin) -> Option<&'static str> {
    match origin {
        RequestOrigin::Client => None,
        RequestOrigin::Repeat => Some("repeated"),
        RequestOrigin::Compose => Some("composed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn send_without_running_engine_fails_fast() {
        let (client, _requests) = direct_channel();
        let request = ComposedRequestDto {
            method: "GET".into(),
            url: "http://a.test/".into(),
            headers: Vec::new(),
            body: None,
            origin: RequestOrigin::Compose,
        };
        let error = tokio::time::timeout(Duration::from_secs(1), client.send(request))
            .await
            .expect("no hang")
            .unwrap_err();
        assert!(matches!(error, DomainError::Conflict(_)), "{error:?}");
    }
}
