//! In-memory ring buffer обменов. Тела живут отдельно (BodyRepository);
//! при вытеснении обмена id его тел попадают в очередь
//! ([`ExchangeRepository::drain_evicted_bodies`]) — потребитель удаляет
//! их из хранилища тел (GC, Phase 5).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::RwLock;

use domain::entities::BodyRef;
use domain::entities::HttpExchange;
use domain::entities::exchange::{ClientBody, ExchangeStatus, HttpResponse};
use domain::entities::websocket::{
    WS_BYTES_CAP, WS_MESSAGES_CAP, WebSocketMessage, WsMessagesPage,
};
use domain::errors::DomainError;
use domain::repositories::{ExchangeRepository, RequestHead, TrafficStats};
use domain::value_objects::filter_query::FilterQuery;
use domain::value_objects::timing::TimingPhases;
use domain::value_objects::{BodyId, ExchangeId};

/// Ring buffer + индекс `id → номер`: поиск обмена по id — O(1), а не
/// проход по всему буферу (pump применяет события на UI-потоке, и при
/// 10k обменах линейный поиск на каждое событие съедал главный поток).
#[derive(Default)]
struct Ring {
    items: VecDeque<HttpExchange>,
    /// Порядковый номер `items[0]` (растёт при вытеснении).
    base: u64,
    index: HashMap<ExchangeId, u64>,
}

impl Ring {
    fn push(&mut self, exchange: HttpExchange) {
        let seq = self.base + self.items.len() as u64;
        self.index.insert(exchange.id, seq);
        self.items.push_back(exchange);
    }

    fn pop_front(&mut self) -> Option<HttpExchange> {
        let evicted = self.items.pop_front()?;
        // Тот же id мог прийти повторно — индекс тогда указывает на новый.
        if self.index.get(&evicted.id) == Some(&self.base) {
            self.index.remove(&evicted.id);
        }
        self.base += 1;
        Some(evicted)
    }

    fn position(&self, id: &ExchangeId) -> Option<usize> {
        let seq = *self.index.get(id)?;
        usize::try_from(seq.checked_sub(self.base)?).ok()
    }

    fn get(&self, id: &ExchangeId) -> Option<&HttpExchange> {
        self.items.get(self.position(id)?)
    }

    fn get_mut(&mut self, id: &ExchangeId) -> Option<&mut HttpExchange> {
        let ix = self.position(id)?;
        self.items.get_mut(ix)
    }

    fn clear(&mut self) {
        self.base += self.items.len() as u64;
        self.items.clear();
        self.index.clear();
    }
}

/// Ring buffer: фиксированная ёмкость (`capture.buffer_len` из конфига),
/// при переполнении вытесняются самые старые обмены.
pub struct InMemoryExchangeRepository {
    exchanges: RwLock<Ring>,
    capacity: usize,
    /// Id тел вытесненных обменов — на выдачу `drain_evicted_bodies`.
    evicted_bodies: Mutex<VecDeque<BodyId>>,
    /// Сообщения WebSocket — отдельно от обменов: `get` (и все, кто
    /// читает обмен целиком) не копирует мегабайты сообщений.
    websocket: Mutex<HashMap<ExchangeId, WsLog>>,
}

/// Сообщения одного WebSocket-обмена.
#[derive(Default)]
struct WsLog {
    messages: VecDeque<WebSocketMessage>,
    /// Байт содержимого в `messages`.
    bytes: usize,
    /// Сколько пришло всего (сквозной номер следующего).
    total: usize,
}

impl WsLog {
    fn push(&mut self, mut message: WebSocketMessage) {
        if self.bytes + message.payload.len() > WS_BYTES_CAP {
            message.payload = Vec::new();
        }
        if self.messages.len() >= WS_MESSAGES_CAP
            && let Some(old) = self.messages.pop_front()
        {
            self.bytes -= old.payload.len();
        }
        self.bytes += message.payload.len();
        self.messages.push_back(message);
        self.total += 1;
    }

    fn page(&self, from: usize) -> WsMessagesPage {
        let first = self.total - self.messages.len();
        let skip = from.saturating_sub(first);
        WsMessagesPage {
            messages: self.messages.iter().skip(skip).cloned().collect(),
            next: self.total,
        }
    }
}

impl InMemoryExchangeRepository {
    /// `capacity == 0` интерпретируется как «не хранить ничего».
    pub fn new(capacity: usize) -> Self {
        Self {
            exchanges: RwLock::new(Ring {
                items: VecDeque::with_capacity(capacity.min(1024)),
                ..Ring::default()
            }),
            capacity,
            evicted_bodies: Mutex::new(VecDeque::new()),
            websocket: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl ExchangeRepository for InMemoryExchangeRepository {
    async fn append(&self, exchange: HttpExchange) -> Result<(), DomainError> {
        let mut guard = self.exchanges.write().await;
        if self.capacity == 0 {
            return Ok(());
        }
        let mut evicted_ids = Vec::new();
        while guard.items.len() >= self.capacity {
            let Some(evicted) = guard.pop_front() else {
                break;
            };
            if let Some(b) = &evicted.request.body {
                evicted_ids.push(b.id);
            }
            if let Some(b) = evicted
                .client_request
                .as_ref()
                .and_then(|c| c.body.as_ref())
            {
                evicted_ids.push(b.id);
            }
            self.websocket
                .lock()
                .expect("websocket lock")
                .remove(&evicted.id);
            if let Some(b) = evicted.response.as_ref().and_then(|r| r.body.as_ref()) {
                evicted_ids.push(b.id);
            }
        }
        guard.push(exchange);
        drop(guard);
        if !evicted_ids.is_empty() {
            self.evicted_bodies
                .lock()
                .expect("evicted_bodies poisoned")
                .extend(evicted_ids);
        }
        Ok(())
    }

    async fn drain_evicted_bodies(&self) -> Result<Vec<BodyId>, DomainError> {
        let mut queue = self.evicted_bodies.lock().expect("evicted_bodies poisoned");
        Ok(queue.drain(..).collect())
    }

    async fn update_response(
        &self,
        id: &ExchangeId,
        response: HttpResponse,
        timing: TimingPhases,
    ) -> Result<(), DomainError> {
        let mut guard = self.exchanges.write().await;
        let exchange = find_mut(&mut guard, id)?;
        exchange.response = Some(response);
        exchange.timing = timing;
        Ok(())
    }

    async fn update_request_body(
        &self,
        id: &ExchangeId,
        body: Option<BodyRef>,
        raw_size: u64,
    ) -> Result<(), DomainError> {
        let mut guard = self.exchanges.write().await;
        let exchange = find_mut(&mut guard, id)?;
        exchange.request.body = body;
        exchange.request.raw_size = raw_size;
        Ok(())
    }

    async fn update_request_head(
        &self,
        id: &ExchangeId,
        head: RequestHead,
        client_body: ClientBody,
    ) -> Result<(), DomainError> {
        let mut guard = self.exchanges.write().await;
        let exchange = find_mut(&mut guard, id)?;
        // Исходный запрос клиента — один раз (повторные правки его не трогают).
        if exchange.client_request.is_none() {
            exchange.client_request = Some(RequestHead {
                method: exchange.request.method.clone(),
                uri: exchange.request.uri.clone(),
                headers: exchange.request.headers.clone(),
                body_lost: client_body == ClientBody::Lost,
                body: match client_body {
                    ClientBody::Stored(body) => Some(body),
                    ClientBody::Unchanged | ClientBody::Lost => None,
                },
            });
        }
        exchange.request.method = head.method;
        exchange.request.uri = head.uri;
        exchange.request.headers = head.headers;
        Ok(())
    }

    async fn append_ws_message(
        &self,
        id: &ExchangeId,
        message: domain::entities::websocket::WebSocketMessage,
    ) -> Result<(), DomainError> {
        if self.exchanges.read().await.get(id).is_none() {
            return Err(DomainError::NotFound(format!("exchange {id}")));
        }
        self.websocket
            .lock()
            .expect("websocket lock")
            .entry(*id)
            .or_default()
            .push(message);
        Ok(())
    }

    async fn ws_messages(
        &self,
        id: &ExchangeId,
        from: usize,
    ) -> Result<WsMessagesPage, DomainError> {
        let logs = self.websocket.lock().expect("websocket lock");
        Ok(logs.get(id).map_or(
            WsMessagesPage {
                messages: Vec::new(),
                next: from,
            },
            |log| log.page(from),
        ))
    }

    async fn update_status(
        &self,
        id: &ExchangeId,
        status: ExchangeStatus,
    ) -> Result<(), DomainError> {
        let mut guard = self.exchanges.write().await;
        find_mut(&mut guard, id)?.status = status;
        Ok(())
    }

    async fn get(&self, id: &ExchangeId) -> Result<Option<HttpExchange>, DomainError> {
        let guard = self.exchanges.read().await;
        Ok(guard.get(id).cloned())
    }

    async fn list(
        &self,
        filter: FilterQuery,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<HttpExchange>, DomainError> {
        let guard = self.exchanges.read().await;
        // Свежие сначала — таблица показывает последнее сверху.
        Ok(guard
            .items
            .iter()
            .rev()
            .filter(|e| filter.matches(e))
            .skip(offset)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn tag(&self, id: &ExchangeId, tag: String) -> Result<(), DomainError> {
        let mut guard = self.exchanges.write().await;
        let exchange = find_mut(&mut guard, id)?;
        if !exchange.tags.contains(&tag) {
            exchange.tags.push(tag);
        }
        Ok(())
    }

    async fn set_note(&self, id: &ExchangeId, note: String) -> Result<(), DomainError> {
        let mut guard = self.exchanges.write().await;
        find_mut(&mut guard, id)?.note = Some(note);
        Ok(())
    }

    async fn clear(&self) -> Result<(), DomainError> {
        self.exchanges.write().await.clear();
        self.websocket.lock().expect("websocket lock").clear();
        Ok(())
    }

    async fn stats(&self) -> Result<TrafficStats, DomainError> {
        let guard = self.exchanges.read().await;
        let mut by_status: HashMap<u16, u64> = HashMap::new();
        let mut total_bytes = 0_u64;
        for exchange in guard.items.iter() {
            total_bytes += exchange.request.raw_size
                + exchange.response.as_ref().map(|r| r.raw_size).unwrap_or(0);
            if let Some(resp) = &exchange.response {
                *by_status.entry(resp.status).or_default() += 1;
            }
        }
        Ok(TrafficStats {
            total_exchanges: guard.items.len() as u64,
            total_bytes,
            by_status,
        })
    }

    async fn count(&self) -> Result<u64, DomainError> {
        Ok(self.exchanges.read().await.items.len() as u64)
    }
}

fn find_mut<'a>(guard: &'a mut Ring, id: &ExchangeId) -> Result<&'a mut HttpExchange, DomainError> {
    guard
        .get_mut(id)
        .ok_or_else(|| DomainError::NotFound(format!("exchange {id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use domain::entities::exchange::HttpRequest;
    use domain::value_objects::http_vo::{HttpVersionVo, Method, Protocol};
    use domain::value_objects::{ConnectionId, Host};

    fn exchange(host: &str, uri: &str) -> HttpExchange {
        HttpExchange::new_pending(
            ConnectionId::new(),
            Host::new(host).expect("test host"),
            HttpRequest {
                method: Method::new("GET"),
                uri: uri.to_string(),
                version: HttpVersionVo::Http11,
                headers: Vec::new(),
                body: None,
                raw_size: 10,
                origin: Default::default(),
            },
            Protocol::Http1,
            Utc::now(),
        )
    }

    #[tokio::test]
    async fn eviction_collects_body_ids_for_gc() {
        use domain::entities::body::BodyStorage;

        let repo = InMemoryExchangeRepository::new(1);
        let req_body = BodyId::new();
        let resp_body = BodyId::new();
        let client_body = BodyId::new();
        let body_ref = |id: BodyId| BodyRef {
            id,
            size: 3,
            content_type: None,
            encoding: None,
            storage: BodyStorage::Memory,
        };

        let first = exchange("a.com", "http://a.com/1");
        let id = first.id;
        repo.append(first).await.unwrap();
        repo.update_request_body(
            &id,
            Some(BodyRef {
                id: req_body,
                size: 5,
                content_type: None,
                encoding: None,
                storage: BodyStorage::Memory,
            }),
            5,
        )
        .await
        .unwrap();
        repo.update_response(
            &id,
            HttpResponse {
                status: 200,
                reason: None,
                headers: Vec::new(),
                body: Some(BodyRef {
                    id: resp_body,
                    size: 7,
                    content_type: None,
                    encoding: None,
                    storage: BodyStorage::Memory,
                }),
                raw_size: 7,
                version: HttpVersionVo::Http11,
            },
            TimingPhases::default(),
        )
        .await
        .unwrap();
        repo.update_request_head(
            &id,
            RequestHead {
                method: Method::new("POST"),
                uri: "http://a.com/edited".into(),
                headers: Vec::new(),
                body: None,
                body_lost: false,
            },
            ClientBody::Stored(body_ref(client_body)),
        )
        .await
        .unwrap();
        // Пустая очередь до вытеснения.
        assert!(repo.drain_evicted_bodies().await.unwrap().is_empty());

        // Вытеснение «a.com» → все его тела (и сохранённое тело клиента) в GC.
        repo.append(exchange("b.com", "http://b.com/2"))
            .await
            .unwrap();
        let drained = repo.drain_evicted_bodies().await.unwrap();
        assert_eq!(drained, vec![req_body, client_body, resp_body]);
        // Повторный drain — пусто (однократная выдача).
        assert!(repo.drain_evicted_bodies().await.unwrap().is_empty());
    }

    fn ws(payload: usize) -> WebSocketMessage {
        WebSocketMessage {
            direction: domain::entities::websocket::WsDirection::Sent,
            opcode: domain::entities::websocket::WsOpcode::Binary,
            payload: vec![0; payload],
            size: payload as u64,
            at: Utc::now(),
            injected: false,
        }
    }

    #[tokio::test]
    async fn websocket_messages_are_paged_capped_and_evicted() {
        let repo = InMemoryExchangeRepository::new(1);
        let first = exchange("a.test", "http://a.test/ws");
        let id = first.id;
        repo.append(first).await.unwrap();
        for _ in 0..WS_MESSAGES_CAP + 5 {
            repo.append_ws_message(&id, ws(1)).await.unwrap();
        }
        let page = repo.ws_messages(&id, 0).await.unwrap();
        assert_eq!(
            (page.messages.len(), page.next),
            (WS_MESSAGES_CAP, WS_MESSAGES_CAP + 5)
        );
        let tail = repo.ws_messages(&id, WS_MESSAGES_CAP + 3).await.unwrap();
        assert_eq!(tail.messages.len(), 2);
        assert!(repo.get(&id).await.unwrap().is_some());

        // Байтовый потолок: содержимое сверх него не хранится.
        repo.append_ws_message(&id, ws(WS_BYTES_CAP)).await.unwrap();
        let last = repo.ws_messages(&id, WS_MESSAGES_CAP + 5).await.unwrap();
        assert!(
            last.messages[0].payload.is_empty() && last.messages[0].size == WS_BYTES_CAP as u64
        );

        // Вытеснение обмена из кольца забирает и его сообщения.
        repo.append(exchange("b.test", "http://b.test/"))
            .await
            .unwrap();
        assert!(repo.ws_messages(&id, 0).await.unwrap().messages.is_empty());
        assert!(repo.append_ws_message(&id, ws(1)).await.is_err());
    }

    #[tokio::test]
    async fn ring_buffer_evicts_oldest() {
        let repo = InMemoryExchangeRepository::new(2);
        repo.append(exchange("a.com", "http://a.com/1"))
            .await
            .unwrap();
        repo.append(exchange("b.com", "http://b.com/2"))
            .await
            .unwrap();
        repo.append(exchange("c.com", "http://c.com/3"))
            .await
            .unwrap();

        assert_eq!(repo.count().await.unwrap(), 2);
        let listed = repo.list(FilterQuery::default(), 0, 10).await.unwrap();
        // Свежие сначала, «a.com» вытеснен.
        assert_eq!(listed[0].host.as_str(), "c.com");
        assert_eq!(listed[1].host.as_str(), "b.com");
    }

    #[tokio::test]
    async fn updates_and_tags_apply_in_place() {
        let repo = InMemoryExchangeRepository::new(10);
        let mut ex = exchange("x.com", "http://x.com/api");
        let id = ExchangeId::new();
        ex.id = id;
        repo.append(ex).await.unwrap();

        repo.update_status(&id, ExchangeStatus::Completed)
            .await
            .unwrap();
        repo.tag(&id, "map-local".into()).await.unwrap();
        repo.tag(&id, "map-local".into()).await.unwrap(); // идемпотентно
        repo.set_note(&id, "проверить".into()).await.unwrap();
        repo.update_response(
            &id,
            HttpResponse {
                status: 404,
                reason: Some("Not Found".into()),
                headers: Vec::new(),
                body: None,
                raw_size: 20,
                version: HttpVersionVo::Http11,
            },
            TimingPhases {
                total_ms: 5.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let stored = repo.get(&id).await.unwrap().unwrap();
        assert_eq!(stored.status, ExchangeStatus::Completed);
        assert_eq!(stored.tags, vec!["map-local".to_string()]);
        assert_eq!(stored.response.as_ref().unwrap().status, 404);
        assert_eq!(stored.timing.total_ms, 5.0);
    }

    #[tokio::test]
    async fn update_missing_exchange_returns_not_found() {
        let repo = InMemoryExchangeRepository::new(10);
        let err = repo
            .update_status(&ExchangeId::new(), ExchangeStatus::Failed)
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::NotFound(_)));
    }

    #[tokio::test]
    async fn stats_aggregate_sizes_and_statuses() {
        let repo = InMemoryExchangeRepository::new(10);
        let mut ex = exchange("y.com", "http://y.com/");
        let id = ExchangeId::new();
        ex.id = id;
        repo.append(ex).await.unwrap();
        repo.update_response(
            &id,
            HttpResponse {
                status: 200,
                reason: None,
                headers: Vec::new(),
                body: None,
                raw_size: 90,
                version: HttpVersionVo::Http11,
            },
            TimingPhases::default(),
        )
        .await
        .unwrap();

        let stats = repo.stats().await.unwrap();
        assert_eq!(stats.total_exchanges, 1);
        assert_eq!(stats.total_bytes, 10 + 90);
        assert_eq!(stats.by_status.get(&200), Some(&1));
    }

    #[tokio::test]
    async fn lookups_by_id_survive_eviction_and_clear() {
        let repo = InMemoryExchangeRepository::new(3);
        let mut ids = Vec::new();
        for i in 0..5 {
            let ex = exchange("r.com", &format!("http://r.com/{i}"));
            ids.push(ex.id);
            repo.append(ex).await.unwrap();
        }
        assert!(repo.get(&ids[0]).await.unwrap().is_none(), "evicted");
        assert!(repo.get(&ids[1]).await.unwrap().is_none(), "evicted");
        for id in &ids[2..] {
            repo.update_status(id, ExchangeStatus::Completed)
                .await
                .unwrap();
            assert_eq!(
                repo.get(id).await.unwrap().unwrap().status,
                ExchangeStatus::Completed
            );
        }
        repo.clear().await.unwrap();
        assert!(repo.get(&ids[4]).await.unwrap().is_none());
        let fresh = exchange("r.com", "http://r.com/after");
        let fresh_id = fresh.id;
        repo.append(fresh).await.unwrap();
        assert!(
            repo.get(&fresh_id).await.unwrap().is_some(),
            "index valid after clear"
        );
    }

    #[tokio::test]
    async fn re_appended_id_is_not_dropped_by_evicting_its_old_copy() {
        let repo = InMemoryExchangeRepository::new(2);
        let ex = exchange("d.com", "http://d.com/1");
        let id = ex.id;
        repo.append(ex.clone()).await.unwrap();
        repo.append(ex).await.unwrap();
        repo.append(exchange("d.com", "http://d.com/2"))
            .await
            .unwrap(); // вытесняет первую копию
        assert!(repo.get(&id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn list_applies_filter_offset_limit() {
        let repo = InMemoryExchangeRepository::new(10);
        for i in 0..5 {
            repo.append(exchange(
                "api.example.com",
                &format!("http://api.example.com/item/{i}"),
            ))
            .await
            .unwrap();
        }
        let filtered = repo
            .list(
                FilterQuery {
                    text: Some("item/4".into()),
                    ..Default::default()
                },
                0,
                10,
            )
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);

        let page = repo.list(FilterQuery::default(), 1, 2).await.unwrap();
        assert_eq!(page.len(), 2);
    }
}
