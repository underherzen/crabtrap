//! Список обменов для таблицы трафика (с фильтром и пагинацией).

use std::sync::Arc;

use domain::errors::DomainError;
use domain::repositories::ExchangeRepository;
use domain::value_objects::filter_query::FilterQuery;

use crate::dto::capture::ExchangeSummaryDto;

pub struct GetExchangeListUseCase {
    exchange_repository: Arc<dyn ExchangeRepository>,
}

impl GetExchangeListUseCase {
    pub fn new(exchange_repository: Arc<dyn ExchangeRepository>) -> Self {
        Self {
            exchange_repository,
        }
    }

    pub async fn execute(
        &self,
        filter: FilterQuery,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<ExchangeSummaryDto>, DomainError> {
        let exchanges = self.exchange_repository.list(filter, offset, limit).await?;
        Ok(exchanges
            .iter()
            .map(ExchangeSummaryDto::from_exchange)
            .collect())
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
    use domain::value_objects::http_vo::{HttpVersionVo, Method, Protocol};
    use domain::value_objects::timing::TimingPhases;
    use domain::value_objects::{ConnectionId, ExchangeId, Host};

    struct MemRepo(std::sync::Mutex<Vec<HttpExchange>>);

    #[async_trait]
    impl ExchangeRepository for MemRepo {
        async fn append(&self, exchange: HttpExchange) -> Result<(), DomainError> {
            self.0.lock().expect("lock").push(exchange);
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
            _client_body: domain::entities::ClientBody,
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
                .0
                .lock()
                .expect("lock")
                .iter()
                .find(|e| e.id == *id)
                .cloned())
        }

        async fn list(
            &self,
            filter: FilterQuery,
            offset: usize,
            limit: usize,
        ) -> Result<Vec<HttpExchange>, DomainError> {
            let guard = self.0.lock().expect("lock");
            Ok(guard
                .iter()
                .filter(|e| filter.matches(e))
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }

        async fn tag(&self, _id: &ExchangeId, _tag: String) -> Result<(), DomainError> {
            Ok(())
        }

        async fn set_note(&self, _id: &ExchangeId, _note: String) -> Result<(), DomainError> {
            Ok(())
        }

        async fn clear(&self) -> Result<(), DomainError> {
            self.0.lock().expect("lock").clear();
            Ok(())
        }

        async fn stats(&self) -> Result<TrafficStats, DomainError> {
            Ok(TrafficStats::default())
        }

        async fn count(&self) -> Result<u64, DomainError> {
            Ok(self.0.lock().expect("lock").len() as u64)
        }
    }

    fn exchange(host: &str, uri: &str, method: &str, status: Option<u16>) -> HttpExchange {
        let response = status.map(|status| HttpResponse {
            status,
            reason: None,
            headers: Vec::new(),
            body: None,
            raw_size: 100,
            version: HttpVersionVo::Http11,
        });
        HttpExchange {
            id: ExchangeId::new(),
            connection_id: ConnectionId::new(),
            started_at: Utc::now(),
            request: HttpRequest {
                method: Method::new(method),
                uri: uri.to_string(),
                version: HttpVersionVo::Http11,
                headers: Vec::new(),
                body: None,
                raw_size: 50,
                origin: Default::default(),
            },
            response,
            status: ExchangeStatus::Completed,
            timing: TimingPhases {
                total_ms: 10.0,
                ..Default::default()
            },
            tls: None,
            protocol: Protocol::Http1,
            tags: Vec::new(),
            host: Host::new(host).expect("test host"),
            note: None,
            client_request: None,
        }
    }

    #[test]
    fn lists_summaries_with_filter_and_pagination() {
        let repo = Arc::new(MemRepo(std::sync::Mutex::new(vec![
            exchange("a.example.com", "https://a.example.com/x", "GET", Some(200)),
            exchange(
                "b.example.com",
                "https://b.example.com/y",
                "POST",
                Some(500),
            ),
            exchange("a.example.com", "https://a.example.com/z", "GET", Some(200)),
        ])));
        let uc = GetExchangeListUseCase::new(repo);

        let all =
            futures::executor::block_on(uc.execute(FilterQuery::default(), 0, 10)).expect("list");
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].host, "a.example.com");
        assert_eq!(all[0].status, Some(200));
        assert_eq!(all[0].duration_ms, 10.0);

        let filtered = futures::executor::block_on(uc.execute(
            FilterQuery {
                methods: vec!["POST".into()],
                ..Default::default()
            },
            0,
            10,
        ))
        .expect("filtered");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].method, "POST");

        let page =
            futures::executor::block_on(uc.execute(FilterQuery::default(), 1, 1)).expect("page");
        assert_eq!(page.len(), 1);
    }
}
