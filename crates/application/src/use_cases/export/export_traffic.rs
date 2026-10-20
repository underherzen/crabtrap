//! Сценарии экспорта: «Copy as cURL» для обмена и HAR-файл для обмена или
//! всей записанной сессии.

use std::io::BufWriter;
use std::path::Path;
use std::sync::Arc;

use domain::entities::BodyRef;
use domain::entities::exchange::HttpExchange;
use domain::errors::DomainError;
use domain::repositories::{BodyRepository, ExchangeRepository};
use domain::value_objects::ExchangeId;
use domain::value_objects::filter_query::FilterQuery;

use super::curl::curl_command;
use super::har::{HarSource, HarWriter, har_entry};
use crate::services::{BodyDecoderService, ExportFileWriter};

/// Что экспортировать в HAR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarScope {
    Exchange(ExchangeId),
    /// Все записанные обмены (в порядке начала).
    Session,
}

pub struct ExportTrafficUseCase {
    exchanges: Arc<dyn ExchangeRepository>,
    bodies: Arc<dyn BodyRepository>,
    decoder: Arc<dyn BodyDecoderService>,
    writer: Arc<dyn ExportFileWriter>,
}

impl ExportTrafficUseCase {
    pub fn new(
        exchanges: Arc<dyn ExchangeRepository>,
        bodies: Arc<dyn BodyRepository>,
        decoder: Arc<dyn BodyDecoderService>,
        writer: Arc<dyn ExportFileWriter>,
    ) -> Self {
        Self {
            exchanges,
            bodies,
            decoder,
            writer,
        }
    }

    /// Команда curl: запрос, каким crabtrap отправил его дальше (после
    /// правок Rewrite / скриптов / брейкпоинта) — тело записано именно
    /// такое, и заголовки должны ему соответствовать.
    pub async fn curl(&self, id: &ExchangeId) -> Result<String, DomainError> {
        let exchange = self.exchange(id).await?;
        let request = &exchange.request;
        let body = self.raw_body(request.body.as_ref()).await?;
        Ok(curl_command(
            request.method.as_str(),
            &request.uri,
            &request.headers,
            body.as_deref(),
        ))
    }

    /// Записать HAR в `path`; возврат — сколько обменов попало в файл
    /// (без ответа — не попадают). `with_bodies = false` — только
    /// заголовки, размеры и тайминги. Пишется потоком: тела грузятся по
    /// одному обмену; ни одной записи — файл не создаётся.
    pub async fn har(
        &self,
        scope: HarScope,
        with_bodies: bool,
        path: &Path,
    ) -> Result<usize, DomainError> {
        let mut exchanges = match scope {
            HarScope::Exchange(id) => vec![self.exchange(&id).await?],
            HarScope::Session => {
                self.exchanges
                    .list(FilterQuery::default(), 0, usize::MAX)
                    .await?
            }
        };
        exchanges.sort_by_key(|e| e.started_at);
        if exchanges.iter().all(|e| e.response.is_none()) {
            return Err(DomainError::ValidationError(
                "nothing to export — no request has a response yet".into(),
            ));
        }
        let file = self.writer.create(path)?;
        let mut har = HarWriter::new(BufWriter::new(file)).map_err(write_error)?;
        for exchange in &exchanges {
            if exchange.response.is_none() {
                continue;
            }
            let (request_body, response_body) = match with_bodies {
                true => self.bodies_of(exchange).await?,
                false => (None, None),
            };
            let entry = har_entry(&HarSource {
                exchange,
                request_body: request_body.as_deref(),
                response_body: response_body.as_deref(),
            });
            if let Some(entry) = entry {
                har.push(&entry).map_err(write_error)?;
            }
        }
        let (out, count) = har.finish().map_err(write_error)?;
        out.into_inner()
            .map_err(|e| write_error(e.into_error()))?
            .commit()?;
        Ok(count)
    }

    async fn exchange(&self, id: &ExchangeId) -> Result<HttpExchange, DomainError> {
        self.exchanges
            .get(id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("exchange {id}")))
    }

    /// Тело запроса как записано и тело ответа без Content-Encoding (битое
    /// сжатие — тело ответа не экспортируется, а не экспорт падает).
    async fn bodies_of(
        &self,
        exchange: &HttpExchange,
    ) -> Result<(Option<Vec<u8>>, Option<Vec<u8>>), DomainError> {
        let request = self.raw_body(exchange.request.body.as_ref()).await?;
        let response_ref = exchange.response.as_ref().and_then(|r| r.body.as_ref());
        let response = match (self.raw_body(response_ref).await?, response_ref) {
            (
                Some(bytes),
                Some(BodyRef {
                    encoding: Some(encoding),
                    ..
                }),
            ) => match self.decoder.decode(&bytes, encoding).await {
                Ok(decoded) => Some(decoded),
                Err(error) => {
                    tracing::warn!(id = %exchange.id, %error, "HAR: response body not decoded, skipped");
                    None
                }
            },
            (bytes, _) => bytes,
        };
        Ok((request, response))
    }

    async fn raw_body(&self, body: Option<&BodyRef>) -> Result<Option<Vec<u8>>, DomainError> {
        match body {
            Some(body) => self.bodies.get(body).await,
            None => Ok(None),
        }
    }
}

fn write_error(error: std::io::Error) -> DomainError {
    DomainError::InfrastructureError(format!("write HAR: {error}"))
}
