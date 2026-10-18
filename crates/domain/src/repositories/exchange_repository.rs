//! Хранилище захваченных обменов (ring buffer в RAM).

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::entities::HttpExchange;
use crate::entities::body::BodyRef;
use crate::entities::exchange::{ExchangeStatus, HttpResponse};
use crate::errors::DomainError;
use crate::value_objects::BodyId;
use crate::value_objects::ExchangeId;
use crate::value_objects::filter_query::FilterQuery;
use crate::value_objects::timing::TimingPhases;

/// Агрегированная статистика трафика (статус-бар / шапка таблицы).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TrafficStats {
    pub total_exchanges: u64,
    /// Сумма req+resp raw_size по всем обменам в буфере.
    pub total_bytes: u64,
    /// Распределение по кодам ответа (только завершённые с ответом).
    pub by_status: HashMap<u16, u64>,
}

pub use crate::entities::exchange::RequestHead;

#[async_trait]
pub trait ExchangeRepository: Send + Sync {
    /// Добавить новый обмен (движок — при ExchangeStarted).
    async fn append(&self, exchange: HttpExchange) -> Result<(), DomainError>;

    /// Записать/заменить ответ (статус обмена не меняет — см. `update_status`).
    async fn update_response(
        &self,
        id: &ExchangeId,
        response: HttpResponse,
        timing: TimingPhases,
    ) -> Result<(), DomainError>;

    /// Прикрепить тело запроса (RequestBodyDone).
    async fn update_request_body(
        &self,
        id: &ExchangeId,
        body: Option<BodyRef>,
        raw_size: u64,
    ) -> Result<(), DomainError>;

    /// Заменить стартовую строку и заголовки запроса тем, что реально ушло
    /// дальше (пайплайн перехвата изменил запрос: скрипт, rewrite).
    async fn update_request_head(
        &self,
        id: &ExchangeId,
        head: RequestHead,
    ) -> Result<(), DomainError>;

    /// Добавить сообщение WebSocket-обмена (Phase 11b); старые сверх
    /// потолка вытесняются.
    async fn append_ws_message(
        &self,
        id: &ExchangeId,
        message: crate::entities::websocket::WebSocketMessage,
    ) -> Result<(), DomainError>;

    /// Сообщения WebSocket-обмена, начиная с курсора `from` (сквозной
    /// номер; вытесненные потолком — пропускаются). Реализации без
    /// WebSocket — пусто.
    async fn ws_messages(
        &self,
        _id: &ExchangeId,
        from: usize,
    ) -> Result<crate::entities::websocket::WsMessagesPage, DomainError> {
        Ok(crate::entities::websocket::WsMessagesPage {
            messages: Vec::new(),
            next: from,
        })
    }

    /// Сменить статус обмена (Completed/Failed/Aborted/...).
    async fn update_status(
        &self,
        id: &ExchangeId,
        status: ExchangeStatus,
    ) -> Result<(), DomainError>;

    async fn get(&self, id: &ExchangeId) -> Result<Option<HttpExchange>, DomainError>;

    /// Отфильтрованный список (свежие сначала), offset+limit — пагинация.
    async fn list(
        &self,
        filter: FilterQuery,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<HttpExchange>, DomainError>;

    /// Добавить тег (идемпотентно).
    async fn tag(&self, id: &ExchangeId, tag: String) -> Result<(), DomainError>;

    async fn set_note(&self, id: &ExchangeId, note: String) -> Result<(), DomainError>;

    /// Полностью очистить буфер (кнопка Clear).
    async fn clear(&self) -> Result<(), DomainError>;

    async fn stats(&self) -> Result<TrafficStats, DomainError>;

    async fn count(&self) -> Result<u64, DomainError>;

    /// Id тел, чьи обмены вытеснены из ring buffer'а (GC тел).
    ///
    /// Реализации с вытеснением (in-memory ring) накапливают id при
    /// вытеснении и отдают их однократно; потребитель удаляет тела из
    /// `BodyRepository`. Default — пусто (вытеснения нет).
    async fn drain_evicted_bodies(&self) -> Result<Vec<BodyId>, DomainError> {
        Ok(Vec::new())
    }
}
