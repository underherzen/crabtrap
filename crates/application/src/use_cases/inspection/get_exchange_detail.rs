//! Детали одного обмена для панели инспектора.

use std::sync::Arc;

use domain::errors::DomainError;
use domain::repositories::ExchangeRepository;
use domain::value_objects::ExchangeId;

use crate::dto::inspection::ExchangeDetailDto;

pub struct GetExchangeDetailUseCase {
    exchange_repository: Arc<dyn ExchangeRepository>,
}

impl GetExchangeDetailUseCase {
    pub fn new(exchange_repository: Arc<dyn ExchangeRepository>) -> Self {
        Self {
            exchange_repository,
        }
    }

    pub async fn execute(&self, id: &ExchangeId) -> Result<ExchangeDetailDto, DomainError> {
        let exchange = self
            .exchange_repository
            .get(id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("exchange {id}")))?;
        Ok(ExchangeDetailDto {
            summary: crate::dto::capture::ExchangeSummaryDto::from_exchange(&exchange),
            request: exchange.request.into(),
            response: exchange.response.map(Into::into),
            timing: exchange.timing,
            tls: exchange.tls,
            note: exchange.note,
        })
    }

    /// Сообщения WebSocket-обмена с курсора `from` (инспектор подгружает
    /// только новые).
    pub async fn websocket_messages(
        &self,
        id: &ExchangeId,
        from: usize,
    ) -> Result<domain::entities::websocket::WsMessagesPage, DomainError> {
        self.exchange_repository.ws_messages(id, from).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use domain::entities::BodyRef;
    use domain::entities::ExchangeStatus;
    use domain::entities::exchange::{HttpExchange, HttpRequest, HttpResponse};
    use domain::repositories::TrafficStats;
    use domain::value_objects::filter_query::FilterQuery;
    use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
    use domain::value_objects::timing::TimingPhases;
    use domain::value_objects::{ConnectionId, Host};

    struct MemRepo {
        exchanges: std::sync::Mutex<Vec<HttpExchange>>,
    }

    #[async_trait]
    impl ExchangeRepository for MemRepo {
        async fn append(&self, exchange: HttpExchange) -> Result<(), DomainError> {
            self.exchanges.lock().expect("lock").push(exchange);
            Ok(())
        }

        async fn update_response(
            &self,
            _id: &ExchangeId,
            _response: HttpResponse,
            _timing: TimingPhases,
        ) -> Result<(), DomainError> {
            Ok(())
        }

        async fn append_ws_message(
            &self,
            _id: &ExchangeId,
            _message: domain::entities::websocket::WebSocketMessage,
        ) -> Result<(), DomainError> {
            Ok(())
        }

        async fn update_request_head(
            &self,
            _id: &ExchangeId,
            _head: domain::repositories::RequestHead,
        ) -> Result<(), DomainError> {
            Ok(())
        }

        async fn update_request_body(
            &self,
            _id: &ExchangeId,
            _body: Option<BodyRef>,
            _raw_size: u64,
        ) -> Result<(), DomainError> {
            Ok(())
        }

        async fn update_status(
            &self,
            _id: &ExchangeId,
            _status: ExchangeStatus,
        ) -> Result<(), DomainError> {
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

        async fn set_note(&self, _id: &ExchangeId, _note: String) -> Result<(), DomainError> {
            Ok(())
        }

        async fn clear(&self) -> Result<(), DomainError> {
            self.exchanges.lock().expect("lock").clear();
            Ok(())
        }

        async fn stats(&self) -> Result<TrafficStats, DomainError> {
            Ok(TrafficStats::default())
        }

        async fn count(&self) -> Result<u64, DomainError> {
            Ok(self.exchanges.lock().expect("lock").len() as u64)
        }
    }

    #[test]
    fn returns_detail_or_not_found() {
        let repo = Arc::new(MemRepo {
            exchanges: std::sync::Mutex::new(vec![]),
        });
        repo.exchanges
            .lock()
            .unwrap()
            .push(HttpExchange::new_pending(
                ConnectionId::new(),
                Host::new("example.com").unwrap(),
                HttpRequest {
                    method: Method::new("GET"),
                    uri: "https://example.com/a".into(),
                    version: HttpVersionVo::Http11,
                    headers: vec![HeaderEntry::new("X-Trace", "1")],
                    body: None,
                    raw_size: 10,
                    origin: Default::default(),
                },
                Protocol::Http1,
                Utc::now(),
            ));
        let id = repo.exchanges.lock().unwrap()[0].id;

        let uc = GetExchangeDetailUseCase::new(repo.clone());
        let detail = futures::executor::block_on(uc.execute(&id)).expect("detail");
        assert_eq!(detail.summary.host, "example.com");
        assert_eq!(detail.request.headers.len(), 1);
        assert!(detail.response.is_none());

        let missing = futures::executor::block_on(uc.execute(&ExchangeId::new()));
        assert!(matches!(missing, Err(DomainError::NotFound(_))));
    }
}
