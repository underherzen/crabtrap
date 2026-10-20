//! Спроецировать `ProxyEvent` движка в ExchangeRepository.
//!
//! Это ядро capture-конвейера (Phase 4): движок шлёт события, данный use case
//! сводит их в доменные HttpExchange. События приходят по одному на обмен
//! последовательно (один consumer канала), поэтому merge через get+update
//! безопасен.
//!
//! Событие для вытесненного ring buffer'ом обмена — НЕ ошибка:
//! `DomainError::NotFound` смягчается в warning (штатная жизнь ring buffer).

use std::sync::Arc;

use domain::entities::ExchangeStatus;
use domain::entities::exchange::{HttpExchange, HttpRequest, HttpResponse};
use domain::errors::DomainError;
use domain::repositories::{BodyRepository, ExchangeRepository, RequestHead};
use domain::value_objects::ExchangeId;
use domain::value_objects::Host;
use domain::value_objects::TimingPhases;
use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method};

use crate::dto::capture::{ExchangeSummaryDto, ProxyEvent};

pub struct RecordExchangeEventUseCase {
    exchange_repository: Arc<dyn ExchangeRepository>,
    body_repository: Arc<dyn BodyRepository>,
}

impl RecordExchangeEventUseCase {
    pub fn new(
        exchange_repository: Arc<dyn ExchangeRepository>,
        body_repository: Arc<dyn BodyRepository>,
    ) -> Self {
        Self {
            exchange_repository,
            body_repository,
        }
    }

    /// Удалить тела обменов, вытесненных из ring buffer'а (GC, Phase 5).
    ///
    /// Вызывается после `append` (единственная вытесняющая операция).
    /// `NotFound` от хранилища тел — не ошибка (тело мог удалить Clear).
    async fn gc_evicted_bodies(&self) -> Result<(), DomainError> {
        for body_id in self.exchange_repository.drain_evicted_bodies().await? {
            let _ = self.body_repository.delete(body_id).await;
        }
        Ok(())
    }

    pub async fn execute(&self, event: ProxyEvent) -> Result<(), DomainError> {
        match event {
            ProxyEvent::ExchangeStarted {
                exchange,
                tls,
                request_headers,
                request_version,
            } => {
                let mut entity = exchange_from_summary(&exchange);
                entity.request.headers = request_headers.into_iter().map(Into::into).collect();
                entity.request.version = request_version;
                // TLS-параметры события (Phase 4): MITM-расшифровка видна
                // в деталях/таблице.
                entity.tls = tls.map(Into::into);
                self.exchange_repository.append(entity).await?;
                // GC (Phase 5): удалить тела обменов, вытесненных этим
                // append'ом из ring buffer'а.
                self.gc_evicted_bodies().await
            }
            ProxyEvent::RequestBodyDone { id, body, raw_size } => self.soften_missing(
                id,
                self.exchange_repository
                    .update_request_body(&id, body, raw_size)
                    .await,
            ),
            ProxyEvent::WebSocketMessage { id, message } => self.soften_missing(
                id,
                self.exchange_repository
                    .append_ws_message(&id, message)
                    .await,
            ),
            ProxyEvent::RequestEdited {
                id,
                method,
                uri,
                headers,
                client_body,
                ..
            } => {
                let head = RequestHead {
                    method: Method::new(&method),
                    uri,
                    headers: headers.into_iter().map(HeaderEntry::from).collect(),
                    body: None,
                    body_lost: false,
                };
                self.soften_missing(
                    id,
                    self.exchange_repository
                        .update_request_head(&id, head, client_body)
                        .await,
                )
            }
            ProxyEvent::ResponseHeaders {
                id,
                status,
                version,
                headers,
                ..
            } => {
                let response = HttpResponse {
                    status,
                    reason: None,
                    headers: headers.into_iter().map(HeaderEntry::from).collect(),
                    body: None,
                    raw_size: 0,
                    // Версия приезжает с провода (Phase 2+); reason-фразы в
                    // http-крейте нет — домен достроит canonical позже.
                    version,
                };
                let timing = self.current_timing(&id).await?;
                self.soften_missing(
                    id,
                    self.exchange_repository
                        .update_response(&id, response, timing)
                        .await,
                )
            }
            ProxyEvent::ExchangeCompleted {
                id,
                timing,
                body,
                raw_size,
            } => self.complete_exchange(id, timing, body, raw_size).await,
            ProxyEvent::ExchangeTagged { id, tag } => {
                self.soften_missing(id, self.exchange_repository.tag(&id, tag).await)
            }
            ProxyEvent::ExchangeFailed { id, error } => {
                self.soften_missing(
                    id,
                    self.exchange_repository
                        .set_note(&id, format!("failed: {error}"))
                        .await,
                )?;
                self.soften_missing(
                    id,
                    self.exchange_repository
                        .update_status(&id, ExchangeStatus::Failed)
                        .await,
                )
            }
            // Прогресс тел не мутирует домен (UI-стриминг); коннекты,
            // брейкпоинты и статус движка в хранилище обменов не пишутся.
            ProxyEvent::BodyProgress { .. }
            | ProxyEvent::ConnectionOpened { .. }
            | ProxyEvent::ConnectionClosed { .. }
            | ProxyEvent::InterceptWaiting { .. }
            | ProxyEvent::InterceptResolved { .. }
            | ProxyEvent::ScriptLog { .. }
            | ProxyEvent::EngineStatus { .. } => Ok(()),
        }
    }

    /// Сводка статистики для статус-бара (обмены + фактический размер тел).
    pub async fn stats(&self) -> Result<crate::dto::capture::TrafficStatsDto, DomainError> {
        let stats = self.exchange_repository.stats().await?;
        let bodies_size = self.body_repository.total_size().await?;
        Ok(crate::dto::capture::TrafficStatsDto {
            total_exchanges: stats.total_exchanges,
            total_bytes: stats.total_bytes + bodies_size,
            by_status: stats.by_status,
        })
    }

    async fn complete_exchange(
        &self,
        id: ExchangeId,
        timing: TimingPhases,
        body: Option<domain::entities::BodyRef>,
        raw_size: u64,
    ) -> Result<(), DomainError> {
        let Some(mut exchange) = self.exchange_repository.get(&id).await? else {
            tracing::warn!(%id, "ExchangeCompleted for missing exchange (evicted?)");
            return Ok(());
        };
        let mut response = exchange.response.take().unwrap_or_else(HttpResponse::empty);
        response.body = body;
        response.raw_size = raw_size;
        self.exchange_repository
            .update_response(&id, response, timing)
            .await?;
        self.exchange_repository
            .update_status(&id, ExchangeStatus::Completed)
            .await
    }

    /// Сохранить текущие тайминги при промежуточном update_response.
    async fn current_timing(&self, id: &ExchangeId) -> Result<TimingPhases, DomainError> {
        Ok(self
            .exchange_repository
            .get(id)
            .await?
            .map(|e| e.timing)
            .unwrap_or_default())
    }

    /// NotFound (вытеснен из ring buffer) — не ошибка для потока событий.
    fn soften_missing(
        &self,
        id: ExchangeId,
        result: Result<(), DomainError>,
    ) -> Result<(), DomainError> {
        match result {
            Err(DomainError::NotFound(_)) => {
                tracing::warn!(%id, "event for missing exchange (evicted from ring buffer)");
                Ok(())
            }
            other => other,
        }
    }
}

/// Authority → хост без порта: `a.com:8443` → `a.com`, `[::1]:443` → `::1`.
/// Голый IPv6 (`::1`, несколько двоеточий) порта не содержит.
fn bare_host(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split_once(']').map_or(rest, |(ip, _)| ip);
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.parse::<u16>().is_ok() => host,
        _ => authority,
    }
}

/// Воссоздать Pending-обмен из события ExchangeStarted.
fn exchange_from_summary(dto: &ExchangeSummaryDto) -> HttpExchange {
    let path = if dto.path.starts_with('/') {
        dto.path.clone()
    } else {
        format!("/{}", dto.path)
    };
    let uri = format!("{}://{}{}", dto.scheme, dto.host, path);
    HttpExchange {
        id: dto.id,
        connection_id: dto.connection_id,
        started_at: dto.started_at,
        request: HttpRequest {
            method: Method::new(dto.method.clone()),
            uri,
            version: HttpVersionVo::default(),
            headers: Vec::new(),
            body: None,
            raw_size: dto.req_size,
            origin: Default::default(),
        },
        response: None,
        status: ExchangeStatus::Pending,
        timing: TimingPhases::default(),
        tls: None,
        protocol: dto.protocol,
        tags: dto.tags.clone(),
        // dto.host может нести порт (CONNECT-запись `host:8443`): он остаётся
        // в uri, а доменный Host — без порта.
        host: Host::new(bare_host(&dto.host))
            .unwrap_or_else(|_| Host::new("invalid").expect("static host")),
        note: None,
        client_request: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::capture::HeaderEntryDto;
    use async_trait::async_trait;
    use chrono::Utc;
    use domain::entities::body::{BodyDescriptor, BodyRef, BodyStorage};
    use domain::repositories::TrafficStats;
    use domain::value_objects::filter_query::FilterQuery;
    use domain::value_objects::http_vo::Protocol;
    use domain::value_objects::{BodyId, ConnectionId};
    use std::collections::HashMap;

    /// Тестовый in-memory репозиторий обменов (гексагональный дубль
    /// инфраструктурного — application не зависит от infrastructure).
    struct MemExchangeRepo {
        exchanges: std::sync::Mutex<Vec<HttpExchange>>,
        /// Симуляция очереди вытесненных тел (как ring buffer в infra).
        evicted: std::sync::Mutex<Vec<BodyId>>,
    }

    impl MemExchangeRepo {
        fn new() -> Self {
            Self {
                exchanges: std::sync::Mutex::new(Vec::new()),
                evicted: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn simulate_eviction(&self, ids: Vec<BodyId>) {
            self.evicted.lock().expect("lock").extend(ids);
        }
    }

    #[async_trait]
    impl ExchangeRepository for MemExchangeRepo {
        async fn append(&self, exchange: HttpExchange) -> Result<(), DomainError> {
            self.exchanges.lock().expect("lock").push(exchange);
            Ok(())
        }

        async fn update_response(
            &self,
            id: &ExchangeId,
            response: HttpResponse,
            timing: TimingPhases,
        ) -> Result<(), DomainError> {
            let mut guard = self.exchanges.lock().expect("lock");
            let ex = guard
                .iter_mut()
                .find(|e| e.id == *id)
                .ok_or_else(|| DomainError::NotFound(id.to_string()))?;
            ex.response = Some(response);
            ex.timing = timing;
            Ok(())
        }

        async fn update_request_head(
            &self,
            id: &ExchangeId,
            head: domain::repositories::RequestHead,
            _client_body: domain::entities::ClientBody,
        ) -> Result<(), DomainError> {
            let mut guard = self.exchanges.lock().expect("lock");
            let ex = guard
                .iter_mut()
                .find(|e| e.id == *id)
                .ok_or_else(|| DomainError::NotFound(id.to_string()))?;
            ex.request.method = head.method;
            ex.request.uri = head.uri;
            ex.request.headers = head.headers;
            Ok(())
        }

        async fn append_ws_message(
            &self,
            id: &ExchangeId,
            message: domain::entities::websocket::WebSocketMessage,
        ) -> Result<(), DomainError> {
            let _ = (id, message);
            Ok(())
        }

        async fn update_request_body(
            &self,
            id: &ExchangeId,
            body: Option<BodyRef>,
            raw_size: u64,
        ) -> Result<(), DomainError> {
            let mut guard = self.exchanges.lock().expect("lock");
            let ex = guard
                .iter_mut()
                .find(|e| e.id == *id)
                .ok_or_else(|| DomainError::NotFound(id.to_string()))?;
            ex.request.body = body;
            ex.request.raw_size = raw_size;
            Ok(())
        }

        async fn update_status(
            &self,
            id: &ExchangeId,
            status: ExchangeStatus,
        ) -> Result<(), DomainError> {
            let mut guard = self.exchanges.lock().expect("lock");
            let ex = guard
                .iter_mut()
                .find(|e| e.id == *id)
                .ok_or_else(|| DomainError::NotFound(id.to_string()))?;
            ex.status = status;
            Ok(())
        }

        async fn get(&self, id: &ExchangeId) -> Result<Option<HttpExchange>, DomainError> {
            Ok(self
                .exchanges
                .lock()
                .expect("lock")
                .iter()
                .find(|e| e.id == *id)
                .cloned())
        }

        async fn list(
            &self,
            _filter: FilterQuery,
            _offset: usize,
            _limit: usize,
        ) -> Result<Vec<HttpExchange>, DomainError> {
            Ok(self.exchanges.lock().expect("lock").clone())
        }

        async fn tag(&self, _id: &ExchangeId, _tag: String) -> Result<(), DomainError> {
            Ok(())
        }

        async fn set_note(&self, id: &ExchangeId, note: String) -> Result<(), DomainError> {
            let mut guard = self.exchanges.lock().expect("lock");
            let ex = guard
                .iter_mut()
                .find(|e| e.id == *id)
                .ok_or_else(|| DomainError::NotFound(id.to_string()))?;
            ex.note = Some(note);
            Ok(())
        }

        async fn clear(&self) -> Result<(), DomainError> {
            self.exchanges.lock().expect("lock").clear();
            Ok(())
        }

        async fn stats(&self) -> Result<TrafficStats, DomainError> {
            let guard = self.exchanges.lock().expect("lock");
            let mut by_status: HashMap<u16, u64> = HashMap::new();
            let mut total_bytes = 0_u64;
            for ex in guard.iter() {
                total_bytes +=
                    ex.request.raw_size + ex.response.as_ref().map(|r| r.raw_size).unwrap_or(0);
                if let Some(resp) = &ex.response {
                    *by_status.entry(resp.status).or_default() += 1;
                }
            }
            Ok(TrafficStats {
                total_exchanges: guard.len() as u64,
                total_bytes,
                by_status,
            })
        }

        async fn count(&self) -> Result<u64, DomainError> {
            Ok(self.exchanges.lock().expect("lock").len() as u64)
        }

        async fn drain_evicted_bodies(&self) -> Result<Vec<BodyId>, DomainError> {
            Ok(self.evicted.lock().expect("lock").drain(..).collect())
        }
    }

    struct MemBodyRepo {
        total: std::sync::Mutex<u64>,
        deleted: std::sync::Mutex<Vec<BodyId>>,
    }

    #[async_trait]
    impl BodyRepository for MemBodyRepo {
        async fn put(
            &self,
            _id: BodyId,
            _data: Vec<u8>,
            descriptor: BodyDescriptor,
        ) -> Result<BodyRef, DomainError> {
            Ok(BodyRef {
                id: BodyId::new(),
                size: descriptor.size,
                content_type: descriptor.content_type,
                encoding: descriptor.encoding,
                storage: BodyStorage::Memory,
            })
        }

        async fn get(&self, _body_ref: &BodyRef) -> Result<Option<Vec<u8>>, DomainError> {
            Ok(None)
        }

        async fn get_range(
            &self,
            _body_ref: &BodyRef,
            _offset: u64,
            _len: u64,
        ) -> Result<Option<Vec<u8>>, DomainError> {
            Ok(None)
        }

        async fn delete(&self, id: BodyId) -> Result<(), DomainError> {
            self.deleted.lock().expect("lock").push(id);
            Ok(())
        }

        async fn total_size(&self) -> Result<u64, DomainError> {
            Ok(*self.total.lock().expect("lock"))
        }

        async fn clear(&self) -> Result<(), DomainError> {
            *self.total.lock().expect("lock") = 0;
            Ok(())
        }
    }

    fn started_event() -> ProxyEvent {
        ProxyEvent::ExchangeStarted {
            exchange: ExchangeSummaryDto {
                id: ExchangeId::new(),
                connection_id: ConnectionId::new(),
                started_at: Utc::now(),
                method: "GET".into(),
                scheme: "https".into(),
                host: "api.example.com".into(),
                path: "/v1/users".into(),
                status: None,
                req_size: 120,
                resp_size: 0,
                duration_ms: 0.0,
                protocol: Protocol::Http1,
                tls_mitm: Some(true),
                tags: Vec::new(),
            },
            tls: Some(crate::dto::capture::TlsInfoDto {
                sni: Some("api.example.com".into()),
                alpn: Some("http/1.1".into()),
                version: Some("TLSv1.3".into()),
                cipher_suite: Some("TLS13_AES_128_GCM_SHA256".into()),
                mitm: true,
            }),
            request_headers: Vec::new(),
            request_version: Default::default(),
        }
    }

    fn use_case() -> (RecordExchangeEventUseCase, Arc<MemExchangeRepo>) {
        let (uc, exchanges, _) = use_case_full();
        (uc, exchanges)
    }

    fn use_case_full() -> (
        RecordExchangeEventUseCase,
        Arc<MemExchangeRepo>,
        Arc<MemBodyRepo>,
    ) {
        let exchanges = Arc::new(MemExchangeRepo::new());
        let bodies = Arc::new(MemBodyRepo {
            total: std::sync::Mutex::new(0),
            deleted: std::sync::Mutex::new(Vec::new()),
        });
        (
            RecordExchangeEventUseCase::new(exchanges.clone(), bodies.clone()),
            exchanges,
            bodies,
        )
    }

    fn run<F>(f: F) -> F::Output
    where
        F: std::future::Future,
    {
        // В application нет tokio (по дизайну) — tests используют
        // минимальный executor из futures.
        futures::executor::block_on(f)
    }

    #[test]
    fn full_event_flow_builds_completed_exchange() {
        let (uc, repo) = use_case();
        let start = started_event();
        let ProxyEvent::ExchangeStarted { exchange, .. } = &start else {
            unreachable!()
        };
        let id = exchange.id;
        let body_ref = BodyRef {
            id: BodyId::new(),
            size: 3,
            content_type: Some("application/json".into()),
            encoding: None,
            storage: BodyStorage::Memory,
        };

        run(uc.execute(start)).expect("start");
        run(uc.execute(ProxyEvent::RequestBodyDone {
            id,
            body: Some(body_ref.clone()),
            raw_size: 200,
        }))
        .expect("req body");
        run(uc.execute(ProxyEvent::ResponseHeaders {
            id,
            status: 200,
            version: HttpVersionVo::Http11,
            headers: vec![HeaderEntryDto {
                name: "Content-Type".into(),
                value: "application/json".into(),
            }],
            ts: Utc::now(),
        }))
        .expect("resp headers");
        run(uc.execute(ProxyEvent::ExchangeCompleted {
            id,
            timing: TimingPhases {
                total_ms: 42.0,
                ..Default::default()
            },
            body: Some(body_ref),
            raw_size: 1024,
        }))
        .expect("completed");

        let ex = run(repo.get(&id)).expect("get").expect("exchange present");
        assert_eq!(ex.status, ExchangeStatus::Completed);
        assert_eq!(ex.host.as_str(), "api.example.com");
        assert_eq!(ex.request.method.as_str(), "GET");
        assert_eq!(ex.request.uri, "https://api.example.com/v1/users");
        assert_eq!(ex.request.raw_size, 200);
        // TLS-параметры из ExchangeStarted попадают в сущность.
        let tls = ex.tls.as_ref().expect("tls info merged");
        assert!(tls.mitm);
        assert_eq!(tls.sni.as_deref(), Some("api.example.com"));
        assert_eq!(tls.alpn.as_deref(), Some("http/1.1"));
        assert_eq!(tls.version.as_deref(), Some("TLSv1.3"));
        assert_eq!(
            tls.cipher_suite.as_deref(),
            Some("TLS13_AES_128_GCM_SHA256")
        );
        let resp = ex.response.expect("response");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.raw_size, 1024);
        assert_eq!(resp.headers.len(), 1);
        assert_eq!(ex.timing.total_ms, 42.0);
    }

    #[test]
    fn request_headers_and_version_from_started_event_are_stored() {
        let (uc, exchanges) = use_case();
        let mut event = started_event();
        let ProxyEvent::ExchangeStarted {
            exchange,
            request_headers,
            request_version,
            ..
        } = &mut event
        else {
            unreachable!()
        };
        *request_headers = vec![crate::dto::capture::HeaderEntryDto {
            name: "User-Agent".into(),
            value: "curl/8".into(),
        }];
        *request_version = HttpVersionVo::H2;
        let id = exchange.id;
        run(uc.execute(event)).expect("record");

        let stored = run(exchanges.get(&id)).expect("get").expect("stored");
        assert_eq!(stored.request.headers.len(), 1);
        assert_eq!(stored.request.headers[0].name, "User-Agent");
        assert_eq!(stored.request.headers[0].value, "curl/8");
        assert_eq!(stored.request.version, HttpVersionVo::H2);
    }

    #[test]
    fn connect_record_with_port_keeps_bare_host_and_port_in_uri() {
        let (uc, exchanges) = use_case();
        let mut event = started_event();
        let ProxyEvent::ExchangeStarted { exchange, tls, .. } = &mut event else {
            unreachable!()
        };
        exchange.method = "CONNECT".into();
        exchange.host = "tunnel.example.com:8443".into();
        exchange.path = String::new();
        *tls = None;
        let id = exchange.id;
        run(uc.execute(event)).expect("record");

        let stored = run(exchanges.get(&id)).expect("get").expect("stored");
        assert_eq!(stored.host.as_str(), "tunnel.example.com");
        assert_eq!(stored.request.uri, "https://tunnel.example.com:8443/");
    }

    #[test]
    fn bare_host_strips_port_but_not_ipv6() {
        assert_eq!(bare_host("a.com:8443"), "a.com");
        assert_eq!(bare_host("a.com"), "a.com");
        assert_eq!(bare_host("[::1]:443"), "::1");
        assert_eq!(bare_host("::1"), "::1");
    }

    #[test]
    fn failure_event_marks_exchange_failed_with_note() {
        let (uc, repo) = use_case();
        let start = started_event();
        let ProxyEvent::ExchangeStarted { exchange, .. } = &start else {
            unreachable!()
        };
        let id = exchange.id;
        run(uc.execute(start)).expect("start");
        run(uc.execute(ProxyEvent::ExchangeFailed {
            id,
            error: "connection reset".into(),
        }))
        .expect("failed");

        let ex = run(repo.get(&id)).expect("get").expect("exchange");
        assert_eq!(ex.status, ExchangeStatus::Failed);
        assert_eq!(ex.note.as_deref(), Some("failed: connection reset"));
    }

    #[test]
    fn events_for_evicted_exchange_are_softened() {
        let (uc, _repo) = use_case();
        let ghost = ExchangeId::new();
        // NotFound от репозитория не должен всплывать ошибкой use case.
        run(uc.execute(ProxyEvent::RequestBodyDone {
            id: ghost,
            body: Some(BodyRef {
                id: BodyId::new(),
                size: 0,
                content_type: None,
                encoding: None,
                storage: BodyStorage::Memory,
            }),
            raw_size: 10,
        }))
        .expect("softened");
    }

    #[test]
    fn non_mutating_events_are_ignored() {
        let (uc, repo) = use_case();
        run(uc.execute(ProxyEvent::ConnectionOpened {
            id: ConnectionId::new(),
            peer: "127.0.0.1:55555".into(),
            ts: Utc::now(),
        }))
        .expect("conn opened");
        run(uc.execute(ProxyEvent::EngineStatus {
            listening: Some("127.0.0.1:8080".into()),
            active_conns: 1,
            error: None,
        }))
        .expect("engine status");
        run(uc.execute(ProxyEvent::BodyProgress {
            id: ExchangeId::new(),
            direction: crate::dto::capture::Direction::Response,
            bytes_so_far: 100,
        }))
        .expect("progress");
        assert_eq!(run(repo.count()).expect("count"), 0);
    }

    #[test]
    fn evicted_exchange_bodies_are_deleted_after_append() {
        // GC (Phase 5): append вытесняет обмен из ring buffer'а →
        // его тела удаляются из BodyRepository.
        let (uc, repo, bodies) = use_case_full();
        let evicted_body = BodyId::new();
        repo.simulate_eviction(vec![evicted_body]);

        run(uc.execute(started_event())).expect("start");

        let deleted = bodies.deleted.lock().expect("lock").clone();
        assert_eq!(deleted, vec![evicted_body]);
        // Очередь вытеснения опустошена — повторный append ничего не удаляет.
        run(uc.execute(started_event())).expect("start 2");
        assert_eq!(bodies.deleted.lock().expect("lock").len(), 1);
    }

    #[test]
    fn request_edited_replaces_request_head() {
        let (uc, repo) = use_case();
        let start = started_event();
        let ProxyEvent::ExchangeStarted { exchange, .. } = &start else {
            unreachable!()
        };
        let id = exchange.id;
        run(uc.execute(start)).expect("start");
        run(uc.execute(ProxyEvent::RequestEdited {
            id,
            method: "POST".into(),
            uri: "https://api.example.com/v2".into(),
            path: "/v2".into(),
            headers: vec![crate::dto::capture::HeaderEntryDto {
                name: "x-script".into(),
                value: "1".into(),
            }],
            client_body: Default::default(),
        }))
        .expect("edited");
        let stored = run(repo.get(&id)).expect("get").expect("exchange");
        assert_eq!(stored.request.method.to_string(), "POST");
        assert_eq!(stored.request.uri, "https://api.example.com/v2");
        assert_eq!(stored.request.headers.len(), 1);
    }
}
