//! Пересылка absolute-form запроса с учётом правил (Phase 6).
//!
//! Конвейер обмена:
//! 1. ExchangeStarted (сводка + заголовки клиента);
//! 2. пайплайн перехвата → маршрут: origin | Map Remote (другой URL) |
//!    Block (код или обрыв соединения) | Map Local (файл с диска);
//! 3. ответ — от origin или локальный — проходит ОДИН путь
//!    [`relay_response`]: ResponseHeaders, пайплайн ответа, tee-захват тела,
//!    ExchangeCompleted. Поэтому заблокированные и подменённые обмены видны
//!    в трафике так же, как настоящие (плюс тег маршрута).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use application::dto::rules::ThrottleSpecDto;
use application::matching::RequestTarget;
use application::use_cases::rules::response_form::map_local_header_error;

use application::dto::capture::{Direction, ExchangeSummaryDto, ProxyEvent, TlsInfoDto};
use application::services::{
    AbortMode, EventPublisher, InterceptContext, InterceptOutcome, InterceptableMessage,
    InterceptionPipeline, RouteDecision,
};
use chrono::Utc;
use domain::entities::ClientBody;
use domain::entities::rule::BreakPhase;
use domain::value_objects::http_vo::{HeaderEntry, Protocol};
use domain::value_objects::{BodyId, ExchangeId};
use futures::StreamExt as _;
use http::header::{HeaderMap, HeaderValue};
use http::{Request, Response, Version};
use http_body_util::BodyExt;

use crate::body::{MonitoredBody, StreamEnd, StreamTracker};
use crate::bypass::{BypassPipeline, SilentPublisher};
use crate::commands::EngineState;
use crate::exchange_timing::ExchangeTiming;
use crate::http_proxy::{
    CloseConnection, ProxyService, body_descriptor, estimate_request_head_size, header_dtos,
    interceptable_request, interceptable_response, strip_hop_by_hop, text_response, version_vo,
};
use crate::intercept_body::{
    Loaded, PipeBody, RequestEdits, apply_request, apply_response, boxed, finish_body, load,
    merge_headers, set_length, status_has_body,
};
use crate::throttle::ThrottledBody;
use crate::tunnel_record::DNS_SPOOFED_TAG;
use crate::ws;

/// Ответ сервиса: `Err` — закрыть клиентское соединение без ответа.
type ServiceResult = Result<Response<MonitoredBody>, CloseConnection>;

/// Общее состояние одного обмена.
struct ExchangeCtx {
    id: ExchangeId,
    events: Arc<dyn EventPublisher>,
    timing: Arc<ExchangeTiming>,
    /// Первый терминальный исход побеждает: поздние «ошибки хвоста»
    /// запроса не перекрывают уже доставленный ответ.
    settled: Arc<AtomicBool>,
    /// Throttle тела ответа (байт/сек), если профиль применяется к обмену.
    down_bps: Option<u64>,
    /// Итоговый запрос после пайплайна — контекст фазы ответа.
    request: Option<InterceptableMessage>,
    /// Апгрейд клиента, если это WebSocket-рукопожатие (Phase 11b).
    client_upgrade: Option<hyper::upgrade::OnUpgrade>,
    /// Обмен записывается (хост в Allow List или списка нет). Иначе —
    /// мимо: события глушатся, тела не сохраняются, без правил и throttle.
    recorded: bool,
    /// Пайплайн обмена: настоящий или пустой (мимо записи).
    pipeline: Arc<dyn InterceptionPipeline>,
}

/// Тег обмена, сообщение которого пайплайн изменил (скрипт, rewrite).
const MODIFIED_TAG: &str = "modified";

fn publish_tag(ctx: &ExchangeCtx, tag: &str) {
    ctx.events.publish(ProxyEvent::ExchangeTagged {
        id: ctx.id,
        tag: tag.to_string(),
    });
}

impl ProxyService {
    /// Проксирование absolute-form запроса.
    pub(crate) async fn forward<B>(self, mut req: Request<B>) -> ServiceResult
    where
        B: hyper::body::Body + Send + 'static,
        B::Data: Into<bytes::Bytes>,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        // WebSocket: апгрейд клиентской стороны забираем до разбора запроса.
        // Только у настоящего клиента: у Repeat/Compose апгрейдиться нечему.
        let client_upgrade = (self.origin == domain::entities::exchange::RequestOrigin::Client
            && ws::is_upgrade(req.method(), req.version(), req.headers()))
        .then(|| hyper::upgrade::on(&mut req));
        let uri = req.uri().clone();
        let scheme = uri.scheme_str().unwrap_or("http").to_string();
        if scheme != "http" && scheme != "https" {
            return Ok(text_response(
                400,
                "crabtrap: unsupported proxy target scheme",
            ));
        }
        let Some(authority) = uri.authority().cloned() else {
            return Ok(text_response(
                400,
                "crabtrap: proxy request without authority",
            ));
        };
        let method = req.method().clone();
        let version = req.version();
        let (mut parts, body) = req.into_parts();
        if version == Version::HTTP_2 {
            merge_cookies(&mut parts.headers);
        }

        let mut ctx = self.start_exchange(
            &parts.headers,
            &uri,
            &scheme,
            version,
            &method,
            client_upgrade.is_some(),
        );
        ctx.client_upgrade = client_upgrade;
        // Будущее сервиса отменено (клиент ушёл до ответа) — обмен не должен
        // зависнуть в pending: гард публикует ExchangeFailed при drop.
        let pending = PendingExchange::new(ctx.events.clone(), ctx.settled.clone(), ctx.id);

        let original = interceptable_request(&method, &uri.to_string(), version, &parts.headers);
        let mut context = InterceptContext {
            exchange_id: ctx.id,
            phase: BreakPhase::Request,
            message: original,
            request: None,
        };
        let pipeline = ctx.pipeline.clone();
        let loaded = self
            .load_for_pipeline(pipeline.as_ref(), &mut context, &parts.headers, boxed(body))
            .await;
        let seen = context.message.clone();
        let (edited, route) = match pipeline.on_request(context).await {
            InterceptOutcome::Proceed { message, route } => (message, route),
            InterceptOutcome::Abort { mode, tag } => {
                if let Some(tag) = tag {
                    publish_tag(&ctx, tag);
                }
                return abort(mode, &ctx);
            }
        };
        let edits = if edited != seen {
            publish_tag(&ctx, MODIFIED_TAG);
            let edits = apply_request(&mut parts, &seen, &edited);
            let client_body = self.store_client_body(&seen, &edited).await;
            publish_request_edited(&ctx, &parts, client_body);
            edits
        } else {
            RequestEdits::default()
        };
        if let Some(tag) = route.tag() {
            publish_tag(&ctx, tag);
        }
        // Итоговые URI/метод (после правок) — для throttle и маршрута.
        let uri = parts.uri.clone();
        let method = parts.method.clone();
        let authority = uri.authority().cloned().unwrap_or(authority);
        let finished = finish_body(loaded, edited.body.clone());
        if let Some(original) = finished.discarded {
            drain_pipe(original);
        }
        ctx.request = Some(edited);

        let throttle = match ctx.recorded {
            true => self.active_throttle(&uri, &method).await,
            false => None,
        };
        if let Some(spec) = &throttle {
            ctx.events.publish(ProxyEvent::ExchangeTagged {
                id: ctx.id,
                tag: "throttled".to_string(),
            });
            ctx.down_bps = spec.down_bps;
            if spec.latency_ms > 0 {
                tokio::time::sleep(Duration::from_millis(spec.latency_ms)).await;
            }
            if dropped_by_loss(spec.loss_percent, fastrand::u8(0..100)) {
                publish_tag(&ctx, DROPPED_TAG);
                publish_failure(
                    &ctx.events,
                    &ctx.settled,
                    ctx.id,
                    format!("dropped by throttle ({}% loss)", spec.loss_percent),
                );
                return Err(CloseConnection);
            }
        }

        // `te: trailers` — не hop-by-hop для h2 (gRPC без него не работает).
        let wants_trailers = parts.headers.get_all(http::header::TE).iter().any(|v| {
            v.to_str()
                .is_ok_and(|v| v.to_ascii_lowercase().contains("trailers"))
        });
        strip_hop_by_hop(&mut parts.headers);
        if ctx.client_upgrade.is_some() {
            ws::restore_upgrade_headers(&mut parts.headers);
        }
        if wants_trailers {
            // Без `Connection: te`: протокол origin выбирается по ALPN уже в
            // клиенте, а h2-клиент hyper снимает Connection вместе со всеми
            // перечисленными в нём заголовками — TE пропал бы. HTTP/1.1-origin
            // принимают TE и без этой опции.
            parts
                .headers
                .insert(http::header::TE, HeaderValue::from_static("trailers"));
        }
        if let Some(length) = finished.replaced {
            set_length(&mut parts.headers, length);
        }
        let body = finished.body;
        let descriptor = body_descriptor(&parts.headers);
        let tracker = request_tracker(&self.state, &ctx, descriptor);
        let req_body = match throttle.and_then(|t| t.up_bps) {
            Some(rate) => MonitoredBody::from_body(ThrottledBody::new(body, rate), tracker),
            None => MonitoredBody::from_body(body, tracker),
        };

        match route {
            RouteDecision::Origin => {
                // Host, выставленный пайплайном, — намеренный (виртуальный
                // хост на том же адресе), как и Host клиента Reverse Proxy с
                // «Keep original Host»; иначе Host = authority URI.
                if !edits.host_header && !self.keeps_client_host() {
                    set_host(&mut parts.headers, authority.as_str());
                }
                self.relay_upstream(ctx, pending, parts, req_body).await
            }
            RouteDecision::MapRemote { url, preserve_host } => {
                match url.as_str().parse::<http::Uri>() {
                    Ok(target) => parts.uri = target,
                    Err(e) => {
                        tracing::warn!(%url, error = %e, "map remote: bad target, using origin")
                    }
                }
                if !preserve_host {
                    let target_authority = parts.uri.authority().map(|a| a.to_string());
                    set_host(
                        &mut parts.headers,
                        target_authority.as_deref().unwrap_or(authority.as_str()),
                    );
                }
                self.relay_upstream(ctx, pending, parts, req_body).await
            }
            RouteDecision::Blocked(status) => {
                drain(req_body);
                let local = local_response(status, None, bytes::Bytes::new());
                self.relay_response(ctx, pending, local).await
            }
            RouteDecision::MapLocal {
                root,
                relative,
                status,
                content_type,
                headers,
            } => {
                drain(req_body);
                let found = FoundResponse {
                    status,
                    content_type: content_type.as_deref(),
                    headers: &headers,
                };
                let local = map_local_response(&root, relative.as_deref(), found).await;
                self.relay_response(ctx, pending, local).await
            }
            RouteDecision::Respond {
                status,
                headers,
                body,
            } => {
                drain(req_body);
                let local = respond_response(status, &headers, body);
                self.relay_response(ctx, pending, local).await
            }
        }
    }

    /// Собрать тело для пайплайна, если он его просит; `context.message.body`
    /// получает байты загруженного тела.
    /// Тело клиента, если пайплайн его заменил: записанное тело обмена —
    /// отправленное, а Repeat повторяет запрос клиента. Исходное не было
    /// загружено (пайплайн заменил поток), запись тел выключена или
    /// хранилище отказало — `Lost`: Repeat откажет, а не подставит правку.
    async fn store_client_body(
        &self,
        seen: &InterceptableMessage,
        edited: &InterceptableMessage,
    ) -> ClientBody {
        if edited.body == seen.body {
            return ClientBody::Unchanged;
        }
        let Some(original) = seen.body.as_ref() else {
            return ClientBody::Lost;
        };
        if !self.state.record_bodies.load(Ordering::Relaxed) {
            return ClientBody::Lost;
        }
        let header = |name: &str| {
            seen.headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case(name))
                .map(|h| h.value.clone())
        };
        let descriptor = domain::entities::body::BodyDescriptor {
            size: original.len() as u64,
            content_type: header("content-type"),
            encoding: header("content-encoding"),
        };
        match self
            .state
            .bodies
            .put(BodyId::new(), original.clone(), descriptor)
            .await
        {
            Ok(body) => ClientBody::Stored(body),
            Err(error) => {
                tracing::warn!(%error, "client request body not stored, Repeat will refuse");
                ClientBody::Lost
            }
        }
    }

    async fn load_for_pipeline(
        &self,
        pipeline: &dyn InterceptionPipeline,
        context: &mut InterceptContext,
        headers: &HeaderMap<HeaderValue>,
        body: PipeBody,
    ) -> Loaded {
        if !pipeline.wants_body(context) {
            return Loaded::Streaming(body);
        }
        let loaded = load(body, headers, self.state.intercept_body_cap_bytes).await;
        context.message.body = loaded.message_body();
        loaded
    }

    /// ExchangeStarted: полная сводка и заголовки клиента ДО похода в origin.
    fn start_exchange(
        &self,
        headers: &HeaderMap<HeaderValue>,
        uri: &http::Uri,
        scheme: &str,
        version: Version,
        method: &http::Method,
        websocket: bool,
    ) -> ExchangeCtx {
        // Хост не в Allow List — обмен мимо записи. Repeat / Compose —
        // явное действие пользователя: записываются всегда.
        let recorded = self.origin != domain::entities::exchange::RequestOrigin::Client
            || self
                .state
                .pipeline
                .records_host(uri.host().unwrap_or_default());
        let id = ExchangeId::new();
        let path = uri
            .path_and_query()
            .map(|pq| pq.as_str().to_string())
            .unwrap_or_else(|| "/".to_string());
        // MITM-соединение: TLS-параметры клиент↔crabtrap едут с событием.
        let tls_mitm = self.mitm.as_ref().map(|_| true);
        let mitm_tls_ms = self.mitm.as_ref().map(|m| m.tls_ms);
        let tls = self.mitm.as_ref().map(|m| TlsInfoDto::from(m.tls.clone()));
        let events: Arc<dyn EventPublisher> = match recorded {
            true => self.state.events.clone(),
            false => Arc::new(SilentPublisher),
        };
        events.publish(ProxyEvent::ExchangeStarted {
            exchange: ExchangeSummaryDto {
                id,
                connection_id: self.conn_id,
                started_at: Utc::now(),
                method: method.as_str().to_string(),
                scheme: scheme.to_string(),
                host: uri.host().unwrap_or_default().to_string(),
                path,
                status: None,
                req_size: estimate_request_head_size(method, &uri.to_string(), headers),
                resp_size: 0,
                duration_ms: 0.0,
                protocol: match version {
                    _ if websocket => Protocol::WebSocket,
                    Version::HTTP_2 => Protocol::Http2,
                    _ => Protocol::Http1,
                },
                tls_mitm,
                tags: crate::direct::origin_tag(self.origin)
                    .into_iter()
                    .chain(self.reverse.as_ref().map(|_| crate::reverse::REVERSE_TAG))
                    .map(str::to_string)
                    .collect(),
            },
            tls,
            // Как прислал клиент — до чистки hop-by-hop и подмены Host.
            request_headers: header_dtos(headers),
            request_version: version_vo(version),
        });
        ExchangeCtx {
            id,
            events,
            timing: Arc::new(ExchangeTiming::started(mitm_tls_ms)),
            settled: Arc::new(AtomicBool::new(false)),
            down_bps: None,
            request: None,
            client_upgrade: None,
            recorded,
            pipeline: match recorded {
                true => self.state.pipeline.clone(),
                false => Arc::new(BypassPipeline {
                    inner: self.state.pipeline.clone(),
                }),
            },
        }
    }

    /// Профиль throttle, применимый к запросу (`match: None` — ко всему).
    async fn active_throttle(
        &self,
        uri: &http::Uri,
        method: &http::Method,
    ) -> Option<ThrottleSpecDto> {
        let spec = self.state.throttle.read().await.clone()?;
        let applies = spec.r#match.as_ref().is_none_or(|m| {
            RequestTarget::parse(&uri.to_string(), method.as_str())
                .is_some_and(|target| m.matches(&target.facts().target()))
        });
        applies.then_some(spec)
    }

    /// Поход в origin (или Map Remote URL) и отдача его ответа.
    async fn relay_upstream(
        &self,
        ctx: ExchangeCtx,
        pending: PendingExchange,
        mut parts: http::request::Parts,
        body: MonitoredBody,
    ) -> ServiceResult {
        // Адрес origin подменён — тег; итоговый хост (после Map Remote).
        if let Some(host) = parts.uri.host()
            && self.state.spoofed_ip(host).await.is_some()
        {
            publish_tag(&ctx, DNS_SPOOFED_TAG);
        }
        // URI остаётся absolute-form: hyper-клиент сам кладёт в стартовую
        // строку origin-form, а authority использует для коннекта и пула.
        parts.version = Version::HTTP_11;
        // HTTP/1.1-клиент: апгрейд (в h2 нет `Upgrade`) и Host, отличный
        // от адреса (preserve host, правка Host): h2 маршрутизирует по
        // :authority и Host с другим значением отвергает.
        let host_differs = parts.headers.get(http::header::HOST).is_some_and(|host| {
            parts
                .uri
                .authority()
                .is_none_or(|a| !host.as_bytes().eq_ignore_ascii_case(a.as_str().as_bytes()))
        });
        let client = match ctx.client_upgrade.is_some() || host_differs {
            true => self.state.upstream(true),
            false => {
                // Host = authority — дубль: h2 несёт его в `:authority`, и
                // `host` рядом Google Front End сбрасывает
                // RST_STREAM(PROTOCOL_ERROR); для HTTP/1.1 hyper-util
                // выставит Host из URI сам.
                parts.headers.remove(http::header::HOST);
                self.state.upstream(false)
            }
        };
        let resp = match client.request(Request::from_parts(parts, body)).await {
            Ok(resp) => resp,
            Err(e) => {
                let error = format!("upstream request failed: {}", crate::error::error_chain(&e));
                publish_failure(&ctx.events, &ctx.settled, ctx.id, error);
                return Ok(text_response(502, "crabtrap: upstream request failed"));
            }
        };
        // Метрики исходящего диалапа (Connected-расширения коннектора):
        // для pooled-соединений — момент установки ЭТОГО соединения пула.
        if let Some(m) = resp.extensions().get::<crate::client::ConnectMetrics>() {
            ctx.timing.mark_upstream(m.connect_ms, m.tls_ms);
        }
        self.relay_response(ctx, pending, resp).await
    }

    /// Ответ клиенту (origin или локальный): ResponseHeaders, пайплайн
    /// ответа, стрим без агрегации с tee-захватом, ExchangeCompleted.
    async fn relay_response<B>(
        &self,
        mut ctx: ExchangeCtx,
        pending: PendingExchange,
        resp: Response<B>,
    ) -> ServiceResult
    where
        B: hyper::body::Body + Send + 'static,
        B::Data: Into<bytes::Bytes>,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        ctx.timing.mark_resp_headers();
        if resp.status() == http::StatusCode::SWITCHING_PROTOCOLS
            && let Some(client_upgrade) = ctx.client_upgrade.take()
        {
            return self
                .switch_to_websocket(ctx, pending, resp, client_upgrade)
                .await;
        }
        let (mut resp_parts, resp_body) = resp.into_parts();
        let mut context = InterceptContext {
            exchange_id: ctx.id,
            phase: BreakPhase::Response,
            message: interceptable_response(
                resp_parts.status.as_u16(),
                resp_parts.version,
                &resp_parts.headers,
            ),
            request: ctx.request.take(),
        };
        let pipeline = ctx.pipeline.clone();
        let loaded = self
            .load_for_pipeline(
                pipeline.as_ref(),
                &mut context,
                &resp_parts.headers,
                boxed(resp_body),
            )
            .await;
        let seen = context.message.clone();
        let edited = match pipeline.on_response(context).await {
            InterceptOutcome::Proceed { message, .. } => message,
            InterceptOutcome::Abort { mode, .. } => {
                // Ответ origin уже был — он виден в трафике, как до пайплайна.
                publish_response_headers(&ctx, &resp_parts, &resp_parts.headers);
                return abort(mode, &ctx);
            }
        };
        if edited != seen {
            apply_response(&mut resp_parts, &seen, &edited);
            publish_tag(&ctx, MODIFIED_TAG);
        }
        let finished = finish_body(loaded, edited.body);
        let length = finished
            .replaced
            .filter(|_| status_has_body(resp_parts.status));
        // В трафик — ответ после пайплайна (как его увидит клиент): у
        // заменённого тела — новая длина вместо исходного framing.
        match length {
            Some(length) => {
                let mut shown = resp_parts.headers.clone();
                set_length(&mut shown, length);
                publish_response_headers(&ctx, &resp_parts, &shown);
            }
            None => publish_response_headers(&ctx, &resp_parts, &resp_parts.headers),
        }
        let resp_body = finished.body;

        // Дескриптор тела снимаем ДО чистки (Content-Type/Encoding).
        let descriptor = body_descriptor(&resp_parts.headers);
        strip_hop_by_hop(&mut resp_parts.headers);
        if let Some(length) = length {
            set_length(&mut resp_parts.headers, length);
        }
        let tracker = response_tracker(&self.state, &ctx, descriptor);
        let body = match ctx.down_bps {
            Some(rate) => MonitoredBody::from_body(ThrottledBody::new(resp_body, rate), tracker),
            None => MonitoredBody::from_body(resp_body, tracker),
        };
        // Дальше исход обмена определяет трекер тела ответа.
        pending.disarm();
        Ok(Response::from_parts(resp_parts, body))
    }
}

/// h2-клиенты шлют Cookie отдельными полями (RFC 9113 §8.2.3); для
/// HTTP/1.1-origin и для пайплайна — одно поле через `; `.
fn merge_cookies(headers: &mut HeaderMap<HeaderValue>) {
    let values: Vec<Vec<u8>> = headers
        .get_all(http::header::COOKIE)
        .iter()
        .map(|v| v.as_bytes().to_vec())
        .collect();
    if values.len() < 2 {
        return;
    }
    let joined = values.join(b"; ".as_slice());
    if let Ok(value) = HeaderValue::from_bytes(&joined) {
        headers.insert(http::header::COOKIE, value);
    }
}

/// Итоговый запрос после правок пайплайна — в трафик.
fn publish_request_edited(
    ctx: &ExchangeCtx,
    parts: &http::request::Parts,
    client_body: ClientBody,
) {
    ctx.events.publish(ProxyEvent::RequestEdited {
        id: ctx.id,
        method: parts.method.as_str().to_string(),
        uri: parts.uri.to_string(),
        path: parts
            .uri
            .path_and_query()
            .map_or_else(|| "/".to_string(), |pq| pq.as_str().to_string()),
        headers: header_dtos(&parts.headers),
        client_body,
    });
}

impl ProxyService {
    /// Origin принял апгрейд: 101 клиенту, обе стороны апгрейдятся,
    /// сообщения — событиями; обмен завершится с закрытием соединения.
    async fn switch_to_websocket<B>(
        &self,
        ctx: ExchangeCtx,
        pending: PendingExchange,
        mut resp: Response<B>,
        client_upgrade: hyper::upgrade::OnUpgrade,
    ) -> ServiceResult {
        let upstream_upgrade = hyper::upgrade::on(&mut resp);
        let (mut parts, _) = resp.into_parts();
        publish_response_headers(&ctx, &parts, &parts.headers);
        strip_hop_by_hop(&mut parts.headers);
        ws::restore_response_headers(&mut parts.headers);
        let relay = ws::relay(
            ws::WsExchange {
                id: ctx.id,
                events: ctx.events.clone(),
                timing: ctx.timing.clone(),
                settled: ctx.settled.clone(),
                _connection: self.guard.clone(),
            },
            client_upgrade,
            upstream_upgrade,
        );
        // Соединение уходит в сокет: http1-задача его больше не закрывает.
        self.guard.mark_upgraded();
        self.state.spawn_tunnel(relay).await;
        pending.disarm();
        let body = MonitoredBody::from_body(
            http_body_util::Empty::<bytes::Bytes>::new(),
            crate::body::StreamTracker::new(|_, _, _| {}),
        );
        Ok(Response::from_parts(parts, body))
    }
}

/// ResponseHeaders в трафик (статус/версия из частей, заголовки — какие
/// показать).
fn publish_response_headers(
    ctx: &ExchangeCtx,
    parts: &http::response::Parts,
    headers: &HeaderMap<HeaderValue>,
) {
    ctx.events.publish(ProxyEvent::ResponseHeaders {
        id: ctx.id,
        status: parts.status.as_u16(),
        version: version_vo(parts.version),
        headers: header_dtos(headers),
        ts: Utc::now(),
    });
}

/// Трекер тела запроса: подсчёт, tee-захват, RequestBodyDone.
fn request_tracker(
    state: &EngineState,
    ctx: &ExchangeCtx,
    descriptor: domain::entities::body::BodyDescriptor,
) -> Arc<StreamTracker> {
    let (id, events, timing, settled) = (
        ctx.id,
        ctx.events.clone(),
        ctx.timing.clone(),
        ctx.settled.clone(),
    );
    let progress_events = events.clone();
    let tracker = StreamTracker::new(move |end, bytes, body| match end {
        StreamEnd::Done => {
            timing.mark_req_done();
            events.publish(ProxyEvent::RequestBodyDone {
                id,
                body,
                raw_size: bytes,
            });
        }
        StreamEnd::Failed => publish_failure(
            &events,
            &settled,
            id,
            format!("request body failed after {bytes} bytes"),
        ),
        // Тело бросает и hyper-клиент, не сумевший подключиться: терминальное
        // событие опубликует ветка upstream (с причиной) или гард обмена.
        StreamEnd::Dropped => tracing::debug!(%id, bytes, "request body dropped"),
    });
    tracker.set_progress(move |sofar| {
        progress_events.publish(ProxyEvent::BodyProgress {
            id,
            direction: Direction::Request,
            bytes_so_far: sofar,
        });
    });
    if ctx.recorded {
        set_tee(state, &tracker, descriptor);
    }
    tracker
}

/// Трекер тела ответа: подсчёт, tee-захват, ExchangeCompleted.
fn response_tracker(
    state: &EngineState,
    ctx: &ExchangeCtx,
    descriptor: domain::entities::body::BodyDescriptor,
) -> Arc<StreamTracker> {
    let (id, events, timing, settled) = (
        ctx.id,
        ctx.events.clone(),
        ctx.timing.clone(),
        ctx.settled.clone(),
    );
    let progress_events = events.clone();
    let tracker = StreamTracker::new(move |end, bytes, body| match end {
        StreamEnd::Done => {
            if !settled.swap(true, Ordering::SeqCst) {
                events.publish(ProxyEvent::ExchangeCompleted {
                    id,
                    timing: timing.finalize(),
                    body,
                    raw_size: bytes,
                });
            }
        }
        StreamEnd::Failed | StreamEnd::Dropped => publish_failure(
            &events,
            &settled,
            id,
            format!("response body {end:?} after {bytes} bytes"),
        ),
    });
    tracker.set_progress(move |sofar| {
        progress_events.publish(ProxyEvent::BodyProgress {
            id,
            direction: Direction::Response,
            bytes_so_far: sofar,
        });
    });
    if ctx.recorded {
        set_tee(state, &tracker, descriptor);
    }
    tracker
}

fn set_tee(
    state: &EngineState,
    tracker: &Arc<StreamTracker>,
    descriptor: domain::entities::body::BodyDescriptor,
) {
    if state.record_bodies.load(Ordering::Relaxed) {
        tracker.set_tee(crate::body::TeeConfig {
            id: BodyId::new(),
            cap_bytes: state.body_capture_cap_bytes,
            descriptor,
            repo: Arc::clone(&state.bodies),
        });
    }
}

/// Авторитетный Host исходящего запроса.
fn set_host(headers: &mut HeaderMap<HeaderValue>, authority: &str) {
    match HeaderValue::from_str(authority) {
        Ok(value) => {
            headers.insert(http::header::HOST, value);
        }
        Err(_) => tracing::warn!(authority, "invalid Host authority, passing as-is"),
    }
}

/// Сколько тела запроса дочитываем у локального ответа (дальше — бросаем:
/// захват тела ограничен своим капом, а держать соединение ради гигабайтной
/// загрузки на заблокированный URL незачем).
const DRAIN_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Дочитать тело запроса, которое никуда не уходит (локальный ответ), чтобы
/// трекер опубликовал RequestBodyDone и тело попало в захват. Кадры
/// ОТБРАСЫВАЮТСЯ по мере чтения (не `collect`: тот держал бы всё тело в
/// памяти); сверх лимита/таймаута тело бросается.
fn drain(body: MonitoredBody) {
    drain_pipe(boxed(body));
}

/// [`drain`] для тела без трекера (незагруженный оригинал, заменённый
/// пайплайном).
fn drain_pipe(mut body: PipeBody) {
    tokio::spawn(async move {
        let read_all = async {
            let mut seen = 0_u64;
            while let Some(frame) = body.frame().await {
                let Ok(frame) = frame else { break };
                seen += frame.data_ref().map_or(0, |d| d.len() as u64);
                if seen > DRAIN_LIMIT_BYTES {
                    tracing::debug!(seen, "drain limit reached, dropping request body");
                    break;
                }
            }
        };
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, read_all).await;
    });
}

/// Тело локального ответа: буфер (Block, 404) или поток файла (Map Local).
type LocalBody = http_body_util::combinators::BoxBody<bytes::Bytes, std::io::Error>;

/// Локальный ответ из буфера.
fn local_response(
    status: u16,
    content_type: Option<&str>,
    body: bytes::Bytes,
) -> Response<LocalBody> {
    let length = body.len() as u64;
    let body = http_body_util::Full::new(body)
        .map_err(|never| match never {})
        .boxed();
    local_response_with(status, content_type, length, body)
}

fn local_response_with(
    status: u16,
    content_type: Option<&str>,
    length: u64,
    body: LocalBody,
) -> Response<LocalBody> {
    let mut builder = Response::builder()
        .status(status)
        .header(http::header::CONTENT_LENGTH, length.to_string());
    if let Some(ct) = content_type {
        builder = builder.header(http::header::CONTENT_TYPE, ct);
    }
    builder.body(body).unwrap_or_else(|_| {
        let mut fallback = Response::new(
            http_body_util::Empty::new()
                .map_err(|never| match never {})
                .boxed(),
        );
        *fallback.status_mut() = http::StatusCode::INTERNAL_SERVER_ERROR;
        fallback
    })
}

/// Готовый ответ пайплайна (Mock API): заголовки из правила, точный
/// Content-Length. Не финальный статус (вне 200–599) — 502: 1xx итоговым
/// ответом ломает HTTP/1; у 204/304 тела и длины нет.
fn respond_response(status: u16, headers: &[HeaderEntry], body: Vec<u8>) -> Response<LocalBody> {
    let status = if (200..=599).contains(&status) {
        status
    } else {
        tracing::warn!(
            status,
            "pipeline response status is not final, answering 502"
        );
        502
    };
    let code = http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::BAD_GATEWAY);
    let body = if status_has_body(code) {
        body
    } else {
        Vec::new()
    };
    let mut response = local_response(status, None, bytes::Bytes::from(body));
    let length = response
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .cloned();
    *response.headers_mut() = merge_headers(&HeaderMap::new(), headers);
    response
        .headers_mut()
        .remove(http::header::TRANSFER_ENCODING);
    response.headers_mut().remove(http::header::CONTENT_LENGTH);
    if let Some(length) = length.filter(|_| status_has_body(code)) {
        response
            .headers_mut()
            .insert(http::header::CONTENT_LENGTH, length);
    }
    response
}

/// Ответ Map Local, когда файл найден: код, Content-Type и доп. заголовки
/// из правила.
struct FoundResponse<'a> {
    status: u16,
    /// `None` — по расширению файла.
    content_type: Option<&'a str>,
    headers: &'a [HeaderEntry],
}

/// Map Local: файл → `found.status` потоком (Content-Type из правила или по
/// расширению, Content-Length из метаданных); иначе → 404 без локального
/// пути в теле.
async fn map_local_response(
    root: &Path,
    relative: Option<&Path>,
    found: FoundResponse<'_>,
) -> Response<LocalBody> {
    match resolve_map_local(root, relative).await {
        Ok((path, file, length)) => {
            let stream = tokio_util::io::ReaderStream::new(file)
                .map(|chunk| chunk.map(hyper::body::Frame::data));
            let body = BodyExt::boxed(http_body_util::StreamBody::new(stream));
            let content_type = found
                .content_type
                .unwrap_or_else(|| content_type_for(&path));
            let response = local_response_with(found.status, Some(content_type), length, body);
            with_extra_headers(response, found.headers)
        }
        Err(reason) => {
            tracing::warn!(root = %root.display(), ?relative, reason, "map local: not served");
            local_response(
                404,
                Some("text/plain; charset=utf-8"),
                bytes::Bytes::from_static(b"crabtrap: map local file not found"),
            )
        }
    }
}

/// Дописать заголовки правила после Content-Type/Content-Length движка
/// (повторы сохраняются). Зарезервированные (фрейминг, hop-by-hop,
/// Content-Type) и невалидные пропускаются — правила уже проверены при
/// сохранении, это защита от ручной правки rules.json.
fn with_extra_headers(
    mut response: Response<LocalBody>,
    extra: &[HeaderEntry],
) -> Response<LocalBody> {
    for entry in extra {
        if let Some(reason) = map_local_header_error(&entry.name, &entry.value) {
            tracing::warn!(header = %entry.name, reason, "map local: header skipped");
            continue;
        }
        match (
            http::HeaderName::from_bytes(entry.name.as_bytes()),
            HeaderValue::from_str(&entry.value),
        ) {
            (Ok(name), Ok(value)) => {
                response.headers_mut().append(name, value);
            }
            _ => tracing::warn!(header = %entry.name, "map local: invalid header skipped"),
        }
    }
    response
}

/// Итоговый файл Map Local. Правило-файл — сам файл; правило-каталог —
/// каталог + `relative`, причём канонический путь (после симлинков) обязан
/// остаться внутри каталога. Отдаётся только обычный файл (не FIFO, не
/// `/dev/*`, не каталог).
async fn resolve_map_local(
    root: &Path,
    relative: Option<&Path>,
) -> Result<(std::path::PathBuf, tokio::fs::File, u64), &'static str> {
    let root_meta = tokio::fs::metadata(root)
        .await
        .map_err(|_| "rule path not found")?;
    let candidate = if root_meta.is_dir() {
        let relative = relative.ok_or("request path rejected")?;
        let root = tokio::fs::canonicalize(root)
            .await
            .map_err(|_| "rule path not found")?;
        let target = tokio::fs::canonicalize(root.join(relative))
            .await
            .map_err(|_| "file not found")?;
        if !target.starts_with(&root) {
            return Err("path escapes the mapped directory");
        }
        target
    } else {
        root.to_path_buf()
    };
    let file = tokio::fs::File::open(&candidate)
        .await
        .map_err(|_| "cannot open file")?;
    let meta = file.metadata().await.map_err(|_| "cannot stat file")?;
    if !meta.is_file() {
        return Err("not a regular file");
    }
    Ok((candidate, file, meta.len()))
}

/// MIME по расширению файла Map Local.
pub(crate) fn content_type_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "json" | "map" => "application/json",
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "txt" | "log" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

/// `InterceptOutcome::Abort`: Close — оборвать соединение, Respond —
/// синтетический ответ.
fn abort(mode: AbortMode, ctx: &ExchangeCtx) -> ServiceResult {
    publish_failure(
        &ctx.events,
        &ctx.settled,
        ctx.id,
        format!("aborted by rules ({})", abort_label(&mode)),
    );
    match mode {
        AbortMode::Close => Err(CloseConnection),
        AbortMode::Respond { status, body } => Ok(text_response(
            status,
            String::from_utf8_lossy(&body).into_owned(),
        )),
    }
}

/// Тег обмена, оборванного throttle (потери).
const DROPPED_TAG: &str = "dropped";

/// Обрыв по доле потерь: `roll` — случайное 0..100.
fn dropped_by_loss(loss_percent: u8, roll: u8) -> bool {
    roll < loss_percent
}

fn abort_label(mode: &AbortMode) -> String {
    match mode {
        AbortMode::Close => "connection closed".to_string(),
        AbortMode::Respond { status, .. } => format!("responded {status}"),
    }
}

/// Гард обмена до отдачи ответа: drop без `disarm` (отмена future сервиса —
/// клиент закрыл соединение) публикует ExchangeFailed. Ветки ошибок сами
/// ставят `settled`, так что для них drop гарда — no-op.
struct PendingExchange {
    events: Arc<dyn EventPublisher>,
    settled: Arc<AtomicBool>,
    id: ExchangeId,
    armed: bool,
}

impl PendingExchange {
    fn new(events: Arc<dyn EventPublisher>, settled: Arc<AtomicBool>, id: ExchangeId) -> Self {
        Self {
            events,
            settled,
            id,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PendingExchange {
    fn drop(&mut self) {
        if self.armed {
            publish_failure(
                &self.events,
                &self.settled,
                self.id,
                "client closed connection before response".to_string(),
            );
        }
    }
}

/// Опубликовать ExchangeFailed, если обмен ещё не завершён успешно.
fn publish_failure(
    events: &Arc<dyn EventPublisher>,
    settled: &Arc<AtomicBool>,
    id: ExchangeId,
    error: String,
) {
    if settled.swap(true, Ordering::SeqCst) {
        return;
    }
    tracing::debug!(%id, %error, "exchange failed");
    events.publish(ProxyEvent::ExchangeFailed { id, error });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_percent_bounds_the_drop_rate() {
        assert!((0..100).all(|roll| !dropped_by_loss(0, roll)));
        assert!((0..100).all(|roll| dropped_by_loss(100, roll)));
        assert_eq!(
            (0..100).filter(|roll| dropped_by_loss(25, *roll)).count(),
            25
        );
    }

    #[test]
    fn h2_cookie_fields_are_joined() {
        let mut headers = HeaderMap::new();
        headers.append(http::header::COOKIE, HeaderValue::from_static("a=1"));
        headers.append(http::header::COOKIE, HeaderValue::from_static("b=2"));
        merge_cookies(&mut headers);
        assert_eq!(headers.get_all(http::header::COOKIE).iter().count(), 1);
        assert_eq!(headers[http::header::COOKIE], "a=1; b=2");
    }

    #[test]
    fn content_type_by_extension() {
        assert_eq!(
            content_type_for(Path::new("a/users.JSON")),
            "application/json"
        );
        assert_eq!(
            content_type_for(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type_for(Path::new("blob")),
            "application/octet-stream"
        );
    }

    #[test]
    fn extra_headers_skip_reserved_ones_from_hand_edited_rules() {
        let response = local_response(200, Some("application/json"), bytes::Bytes::from("[]"));
        let response = with_extra_headers(
            response,
            &[
                HeaderEntry::new("Content-Length", "999"),
                HeaderEntry::new("Transfer-Encoding", "chunked"),
                HeaderEntry::new("X-Evil", "a\r\nb"),
                HeaderEntry::new("X-Mock", "1"),
            ],
        );
        let headers = response.headers();
        assert_eq!(
            headers.get_all(http::header::CONTENT_LENGTH).iter().count(),
            1
        );
        assert_eq!(headers[http::header::CONTENT_LENGTH], "2");
        assert!(headers.get(http::header::TRANSFER_ENCODING).is_none());
        assert!(headers.get("x-evil").is_none());
        assert_eq!(headers["x-mock"], "1");
    }

    #[test]
    fn content_type_covers_common_mock_assets() {
        for (file, mime) in [
            ("a.avif", "image/avif"),
            ("a.mp4", "video/mp4"),
            ("a.webm", "video/webm"),
            ("a.mp3", "audio/mpeg"),
            ("a.ttf", "font/ttf"),
            ("a.otf", "font/otf"),
            ("a.woff", "font/woff"),
            ("a.csv", "text/csv; charset=utf-8"),
            ("a.yaml", "application/yaml"),
            ("a.yml", "application/yaml"),
            ("app.js.map", "application/json"),
            ("README.md", "text/markdown; charset=utf-8"),
        ] {
            assert_eq!(content_type_for(Path::new(file)), mime, "{file}");
        }
    }
}
