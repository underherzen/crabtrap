//! Проксирующий HTTP-сервис: маршрутизация запросов.
//!
//! 1. `CONNECT host:port` → туннель ([`crate::connect`]).
//! 2. absolute-form (`scheme://authority/...`) → forward в origin.
//! 3. origin-form (прямое попадание в листенер) → заглушка 200
//!    (Phase 3 добавит раздачу CA через `GET /ssl`).
//!
//! Forward-путь стримит тело без агрегации, фильтрует hop-by-hop заголовки
//! в обе стороны и публикует события обмена:
//! ExchangeStarted → (RequestBodyDone) → ResponseHeaders → (BodyProgress) →
//! ExchangeCompleted | ExchangeFailed.

use std::sync::Arc;

use application::dto::capture::HeaderEntryDto;
use application::services::InterceptableMessage;
use domain::entities::body::BodyDescriptor;
use domain::entities::exchange::{HttpRequest, HttpResponse};
use domain::value_objects::ConnectionId;
use domain::value_objects::Host;
use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo};
use http::header::{HeaderMap, HeaderName, HeaderValue};
use http::{Method, Request, Response, Version};
use hyper::body::Incoming;
use hyper::service::Service;

use crate::body::{MonitoredBody, StreamTracker};
use crate::commands::{ConnGuard, EngineState};
use crate::connect;

/// Заголовки, живущие ровно один хоп (RFC 9110 §7.6.1). Transfer-Encoding
/// управляет hyper на каждой стороне сам.
const HOP_BY_HOP: &[&str] = &[
    "proxy-connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Контекст расшифрованного MITM-соединения: origin-form запросы
/// переписываются в absolute-form `https://<host>:<port>`.
#[derive(Clone, Debug)]
pub(crate) struct MitmConnInfo {
    host: String,
    port: u16,
    /// Хендшейк клиент↔crabtrap (серверная сторона), мс — тайминг tls_ms.
    pub(crate) tls_ms: f64,
    /// Параметры TLS-сессии клиент↔crabtrap (версия/cipher/ALPN, Phase 4).
    pub(crate) tls: domain::value_objects::tls::TlsInfo,
}

/// Сервис на каждое клиентское соединение (клонируется на запрос).
#[derive(Clone)]
pub(crate) struct ProxyService {
    pub(crate) state: Arc<EngineState>,
    pub(crate) conn_id: ConnectionId,
    pub(crate) guard: Arc<ConnGuard>,
    /// Some — соединение расшифровано MITM (CONNECT→TLS в connect.rs).
    pub(crate) mitm: Option<MitmConnInfo>,
    /// Откуда запрос: клиент или Repeat/Compose из UI (тег обмена).
    pub(crate) origin: domain::entities::exchange::RequestOrigin,
    /// Адрес листенера, к которому подключился клиент (реальный IP
    /// интерфейса даже при 0.0.0.0) — адрес прокси для страницы устройства.
    pub(crate) listener_addr: Option<std::net::SocketAddr>,
}

/// Ошибка сервиса = «закрыть клиентское соединение без ответа» (Block с
/// действием Close): hyper рвёт соединение при ошибке сервиса.
#[derive(Debug)]
pub(crate) struct CloseConnection;

impl std::fmt::Display for CloseConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("connection closed by rules")
    }
}

impl std::error::Error for CloseConnection {}

impl Service<Request<Incoming>> for ProxyService {
    type Response = Response<MonitoredBody>;
    type Error = CloseConnection;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let svc = self.clone();
        Box::pin(async move { svc.handle(req).await })
    }
}

impl ProxyService {
    pub(crate) fn new(
        state: Arc<EngineState>,
        conn_id: ConnectionId,
        guard: Arc<ConnGuard>,
    ) -> Self {
        Self {
            state,
            conn_id,
            guard,
            mitm: None,
            origin: Default::default(),
            listener_addr: None,
        }
    }

    /// Адрес листенера соединения (см. поле `listener_addr`).
    pub(crate) fn with_listener_addr(mut self, addr: Option<std::net::SocketAddr>) -> Self {
        self.listener_addr = addr;
        self
    }

    /// Запрос из UI (Repeat / Compose).
    pub(crate) fn with_origin(mut self, origin: domain::entities::exchange::RequestOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// MITM-контекст (вызывается из connect.rs на расшифрованном стриме).
    pub(crate) fn for_mitm(
        mut self,
        host: String,
        port: u16,
        tls_ms: f64,
        tls: domain::value_objects::tls::TlsInfo,
    ) -> Self {
        self.mitm = Some(MitmConnInfo {
            host,
            port,
            tls_ms,
            tls,
        });
        self
    }

    async fn handle(
        self,
        req: Request<Incoming>,
    ) -> Result<Response<MonitoredBody>, CloseConnection> {
        let method = req.method().clone();
        let has_authority = req.uri().scheme().is_some() && req.uri().authority().is_some();

        if method == Method::CONNECT {
            return Ok(connect::handle_connect(self.state, self.guard, req).await);
        }
        if has_authority && self.mitm.is_none() {
            if crate::direct_hit::is_setup_host(&req) {
                return Ok(self.direct_hit(req).await);
            }
            self.forward(req).await
        } else if let Some(mitm) = self.mitm.clone() {
            // Внутри MITM цель — хост CONNECT (h2 присылает absolute-form с
            // :authority/:scheme клиента — им не доверяем: SSL-решение
            // принималось для этого хоста, и схема туннеля — https).
            // Расшифрованный запрос приходит origin-form: восстанавливаем
            // absolute-form https по хосту CONNECT (порт по умолчанию —
            // без суффикса :443, как пишут браузеры).
            let (mut parts, body) = req.into_parts();
            let pq = parts
                .uri
                .path_and_query()
                .map(|p| p.as_str().to_string())
                .unwrap_or_else(|| "/".to_string());
            let authority = if mitm.port == 443 {
                mitm.host.clone()
            } else {
                format!("{}:{}", mitm.host, mitm.port)
            };
            match format!("https://{authority}{pq}").parse::<http::Uri>() {
                Ok(uri) => {
                    parts.uri = uri;
                    self.forward(Request::from_parts(parts, body)).await
                }
                Err(e) => {
                    tracing::warn!(host = %mitm.host, error = %e, "cannot rebuild https URI");
                    Ok(text_response(400, "crabtrap: invalid request target"))
                }
            }
        } else {
            // Прямое попадание в порт прокси (кто-то открыл его как сайт):
            // /ssl — инструкция+скачивание CA (бутстрап ДО настройки прокси).
            tracing::debug!(path = ?req.uri().path(), "origin-form request on proxy listener");
            Ok(self.direct_hit(req).await)
        }
    }
}

// ==== вспомогательные ====

/// Дескриптор тела из заголовков сообщения (Content-Type/Content-Encoding)
/// для tee-захвата (Phase 4).
pub(crate) fn body_descriptor(headers: &HeaderMap<HeaderValue>) -> BodyDescriptor {
    let pick = |name: &str| {
        headers
            .get(name)
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
    };
    BodyDescriptor {
        size: 0,
        content_type: pick("content-type"),
        encoding: pick("content-encoding"),
    }
}

/// Синтетический text/plain ответ.
pub(crate) fn text_response(status: u16, body: impl Into<String>) -> Response<MonitoredBody> {
    let body = body.into();
    let tracker = StreamTracker::new(|_, _, _| {});
    // Content-Length ставим сами: MonitoredBody не даёт точного size_hint
    // (стрим), а для синтетики длина известна.
    let builder = Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(http::header::CONTENT_LENGTH, body.len().to_string());
    builder
        .body(MonitoredBody::from_body(
            http_body_util::Full::new(bytes::Bytes::from(body)),
            tracker,
        ))
        .expect("static response builds")
}

/// Убрать hop-by-hop заголовки (+ те, что перечислены в Connection).
pub(crate) fn strip_hop_by_hop(headers: &mut HeaderMap<HeaderValue>) {
    let mut listed: Vec<String> = Vec::new();
    if let Some(conn_values) = headers.remove(http::header::CONNECTION) {
        // Значения Connection: список токенов через запятую.
        if let Ok(value) = conn_values.to_str() {
            for token in value.split(',') {
                let token = token.trim().to_ascii_lowercase();
                if !token.is_empty() {
                    listed.push(token);
                }
            }
        }
    }
    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
    for token in listed {
        if let Ok(name) = HeaderName::from_bytes(token.as_bytes()) {
            headers.remove(name);
        }
    }
}

/// Оценка размера заголовочной части запроса «по проводу» (для req_size).
pub(crate) fn estimate_request_head_size(
    method: &Method,
    uri: &str,
    headers: &HeaderMap<HeaderValue>,
) -> u64 {
    let mut size: u64 =
        method.as_str().len() as u64 + 1 + uri.len() as u64 + 1 + "HTTP/1.1\r\n".len() as u64;
    for (name, value) in headers.iter() {
        size += name.as_str().len() as u64 + 2; // "name: "
        size += value.as_bytes().len() as u64 + 2; // value + CRLF
    }
    size + 2 // финальный CRLF
}

/// wire-версия → доменная VO.
/// HeaderMap → DTO (значения не-UTF-8 — lossy).
pub(crate) fn header_dtos(headers: &http::HeaderMap) -> Vec<HeaderEntryDto> {
    headers
        .iter()
        .map(|(n, v)| HeaderEntryDto {
            name: n.as_str().to_string(),
            value: String::from_utf8_lossy(v.as_bytes()).into_owned(),
        })
        .collect()
}

pub(crate) fn version_vo(v: Version) -> HttpVersionVo {
    match v {
        Version::HTTP_09 => HttpVersionVo::Http09,
        Version::HTTP_10 => HttpVersionVo::Http10,
        Version::HTTP_11 => HttpVersionVo::Http11,
        Version::HTTP_2 => HttpVersionVo::H2,
        Version::HTTP_3 => HttpVersionVo::H3,
        _ => HttpVersionVo::Http11,
    }
}

/// Запрос → InterceptableMessage без тела: тело подставляет `forward`
/// (через `intercept_body`), только если пайплайн просит его (`wants_body`).
pub(crate) fn interceptable_request(
    method: &Method,
    uri: &str,
    version: Version,
    headers: &HeaderMap<HeaderValue>,
) -> InterceptableMessage {
    let headers = headers
        .iter()
        .map(|(n, v)| HeaderEntry {
            name: n.as_str().to_string(),
            value: String::from_utf8_lossy(v.as_bytes()).into_owned(),
        })
        .collect();
    HttpRequest {
        method: domain::value_objects::http_vo::Method::new(method.as_str()),
        uri: uri.to_string(),
        version: version_vo(version),
        headers,
        body: None,
        raw_size: 0,
        origin: Default::default(),
    }
    .into_interceptable()
}

/// Ответ → InterceptableMessage без тела (тело — как у запроса, по `wants_body`).
pub(crate) fn interceptable_response(
    status: u16,
    version: Version,
    headers: &HeaderMap<HeaderValue>,
) -> InterceptableMessage {
    let headers = headers
        .iter()
        .map(|(n, v)| HeaderEntry {
            name: n.as_str().to_string(),
            value: String::from_utf8_lossy(v.as_bytes()).into_owned(),
        })
        .collect();
    HttpResponse {
        status,
        reason: None,
        headers,
        body: None,
        raw_size: 0,
        version: version_vo(version),
    }
    .into_interceptable()
}

/// Сахар: доменное сообщение → InterceptableMessage.
trait IntoInterceptable {
    fn into_interceptable(self) -> InterceptableMessage;
}

impl IntoInterceptable for HttpRequest {
    fn into_interceptable(self) -> InterceptableMessage {
        InterceptableMessage::from_request(&self)
    }
}

impl IntoInterceptable for HttpResponse {
    fn into_interceptable(self) -> InterceptableMessage {
        InterceptableMessage::from_response(&self)
    }
}

// Host строится с деградацией: даже невалидный хост не должен ронять конвейер.
pub(crate) fn lenient_host(raw: &str) -> Host {
    Host::new(raw).unwrap_or_else(|e| {
        tracing::warn!(raw, error = %e, "invalid host, using placeholder");
        Host::new("invalid.host").expect("static host")
    })
}
