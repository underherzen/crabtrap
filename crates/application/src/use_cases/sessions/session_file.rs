//! Save Session / Open Session: весь записанный трафик — в файл `.crabtrap`
//! и обратно. Открытая сессия добавляется к текущему трафику (ничего не
//! стирается): обмены получают новые id и тег `imported`, тела — заново в
//! хранилище, незавершённые обмены — Aborted.

use std::collections::HashMap;
use std::io::BufWriter;
use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use domain::entities::body::{BodyDescriptor, BodyRef};
use domain::entities::exchange::{ExchangeStatus, HttpExchange};
use domain::entities::websocket::WebSocketMessage;
use domain::errors::DomainError;
use domain::repositories::{BodyRepository, ExchangeRepository};
use domain::value_objects::filter_query::FilterQuery;
use domain::value_objects::{BodyId, ExchangeId};

use super::format::{
    FORMAT, SessionHeader, SessionReader, SessionRecord, SessionWriter, StoredBody, VERSION,
};
use super::har_import::parse_har;
use crate::dto::capture::ExchangeSummaryDto;
use crate::services::{ExportFileWriter, ImportFileReader};

/// Тег открытых из файла обменов.
pub const IMPORTED_TAG: &str = "imported";

/// Открытый обмен для таблицы трафика.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedRow {
    pub summary: ExchangeSummaryDto,
    /// Content-Type ответа (фильтр `type:`, колонка Type).
    pub content_type: Option<String>,
    /// Обмен упал или оборвался — строка красится как ошибка.
    pub failed: Option<String>,
    /// Сколько сообщений WebSocket открыто вместе с обменом.
    pub ws_messages: usize,
}

impl ImportedRow {
    pub fn of(exchange: &HttpExchange, ws_messages: usize) -> Self {
        Self {
            summary: ExchangeSummaryDto::from_exchange(exchange),
            content_type: exchange.response.as_ref().and_then(|r| {
                r.headers
                    .iter()
                    .find(|h| h.name.eq_ignore_ascii_case("content-type"))
                    .map(|h| h.value.clone())
            }),
            failed: match exchange.status {
                ExchangeStatus::Failed => Some("failed (imported)".into()),
                ExchangeStatus::Aborted => Some("aborted (imported)".into()),
                ExchangeStatus::Pending
                | ExchangeStatus::Completed
                | ExchangeStatus::Replayed
                | ExchangeStatus::Composed => None,
            },
            ws_messages,
        }
    }
}

/// Итог открытия: что попало в трафик и почему остановились (битый хвост
/// файла не отменяет уже добавленные обмены).
#[derive(Debug, Default)]
pub struct ImportOutcome {
    pub imported: Vec<ImportedRow>,
    /// Записи HAR, которые не удалось разобрать (хост IPv6, нет URL…).
    pub skipped: usize,
    pub error: Option<DomainError>,
}

pub struct SessionFileUseCase {
    exchanges: Arc<dyn ExchangeRepository>,
    bodies: Arc<dyn BodyRepository>,
    writer: Arc<dyn ExportFileWriter>,
    reader: Arc<dyn ImportFileReader>,
}

impl SessionFileUseCase {
    pub fn new(
        exchanges: Arc<dyn ExchangeRepository>,
        bodies: Arc<dyn BodyRepository>,
        writer: Arc<dyn ExportFileWriter>,
        reader: Arc<dyn ImportFileReader>,
    ) -> Self {
        Self {
            exchanges,
            bodies,
            writer,
            reader,
        }
    }

    /// Сохранить весь трафик в `path`; возврат — сколько обменов.
    pub async fn save(&self, path: &Path) -> Result<usize, DomainError> {
        let mut exchanges = self
            .exchanges
            .list(FilterQuery::default(), 0, usize::MAX)
            .await?;
        if exchanges.is_empty() {
            return Err(DomainError::ValidationError(
                "nothing to save — the traffic list is empty".into(),
            ));
        }
        exchanges.sort_by_key(|e| e.started_at);
        let header = SessionHeader {
            format: FORMAT.into(),
            version: VERSION,
            created_at: Utc::now(),
            exchange_count: exchanges.len() as u64,
        };
        let file = self.writer.create(path)?;
        let mut writer = SessionWriter::new(BufWriter::new(file), &header)?;
        for exchange in &exchanges {
            let record = SessionRecord {
                bodies: self.bodies_of(exchange).await?,
                ws_messages: self.ws_messages_of(&exchange.id).await?,
                exchange: exchange.clone(),
            };
            writer.push(&record)?;
        }
        writer
            .finish()?
            .into_inner()
            .map_err(|e| DomainError::InfrastructureError(format!("write session: {}", e.error())))?
            .commit()?;
        Ok(exchanges.len())
    }

    /// Открыть сессию из `path` и добавить её обмены в трафик.
    pub async fn open(&self, path: &Path) -> ImportOutcome {
        let mut outcome = ImportOutcome::default();
        let mut reader = match self.reader.open(path).and_then(SessionReader::open) {
            Ok(reader) => reader,
            Err(error) => {
                outcome.error = Some(error);
                return outcome;
            }
        };
        loop {
            let record = match reader.next_record() {
                Ok(Some(record)) => record,
                Ok(None) => return outcome,
                Err(error) => {
                    outcome.error = Some(error);
                    return outcome;
                }
            };
            match self.import(record).await {
                Ok(summary) => outcome.imported.push(summary),
                Err(error) => {
                    outcome.error = Some(error);
                    return outcome;
                }
            }
        }
    }

    /// Импорт HAR из `path`: записи — как открытая сессия.
    pub async fn import_har(&self, path: &Path) -> ImportOutcome {
        let parsed = match self.reader.open(path).and_then(parse_har) {
            Ok(parsed) => parsed,
            Err(error) => {
                return ImportOutcome {
                    error: Some(error),
                    ..ImportOutcome::default()
                };
            }
        };
        let mut outcome = ImportOutcome {
            skipped: parsed.skipped,
            ..ImportOutcome::default()
        };
        for record in parsed.records {
            match self.import(record).await {
                Ok(row) => outcome.imported.push(row),
                Err(error) => {
                    outcome.error = Some(error);
                    break;
                }
            }
        }
        outcome
    }

    async fn import(&self, record: SessionRecord) -> Result<ImportedRow, DomainError> {
        let mut stored = HashMap::new();
        for body in record.bodies {
            // Тело, на которое обмен не ссылается, не сохраняется: его
            // никто не освободил бы.
            let Some(old) = find_ref(&record.exchange, body.id) else {
                continue;
            };
            let old = Some(old);
            let descriptor = BodyDescriptor {
                size: body.data.len() as u64,
                content_type: old.and_then(|r| r.content_type.clone()),
                encoding: old.and_then(|r| r.encoding.clone()),
            };
            let new_ref = self
                .bodies
                .put(BodyId::new(), body.data, descriptor)
                .await?;
            stored.insert(body.id, new_ref);
        }
        let exchange = imported(record.exchange, &stored);
        let id = exchange.id;
        let mut row = ImportedRow::of(&exchange, 0);
        self.exchanges.append(exchange).await?;
        self.release_evicted_bodies().await;
        for message in record.ws_messages {
            // Сообщение не легло (обмен уже вытеснен) — обмен всё равно
            // открыт; остальные сообщения пробуем дальше.
            match self.exchanges.append_ws_message(&id, message).await {
                Ok(()) => row.ws_messages += 1,
                Err(error) => tracing::debug!(%id, %error, "imported websocket message dropped"),
            }
        }
        Ok(row)
    }

    /// Тела обменов, вытесненных кольцевым буфером при добавлении, —
    /// освободить (как при живой записи).
    async fn release_evicted_bodies(&self) {
        match self.exchanges.drain_evicted_bodies().await {
            Ok(ids) => {
                for id in ids {
                    let _ = self.bodies.delete(id).await;
                }
            }
            Err(error) => tracing::warn!(%error, "evicted bodies not released after import"),
        }
    }

    /// Байты всех тел обмена (что не сохранено — пропускается).
    async fn bodies_of(&self, exchange: &HttpExchange) -> Result<Vec<StoredBody>, DomainError> {
        let mut out = Vec::new();
        for body_ref in body_refs(exchange) {
            if let Some(data) = self.bodies.get(body_ref).await? {
                out.push(StoredBody {
                    id: body_ref.id,
                    data,
                });
            }
        }
        Ok(out)
    }

    async fn ws_messages_of(&self, id: &ExchangeId) -> Result<Vec<WebSocketMessage>, DomainError> {
        let mut out = Vec::new();
        let mut from = 0;
        loop {
            let page = self.exchanges.ws_messages(id, from).await?;
            if page.messages.is_empty() || page.next == from {
                return Ok(out);
            }
            out.extend(page.messages);
            from = page.next;
        }
    }
}

/// Ссылки на тела обмена: запрос, ответ, исходный запрос клиента.
fn body_refs(exchange: &HttpExchange) -> Vec<&BodyRef> {
    [
        exchange.request.body.as_ref(),
        exchange.response.as_ref().and_then(|r| r.body.as_ref()),
        exchange
            .client_request
            .as_ref()
            .and_then(|r| r.body.as_ref()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn find_ref(exchange: &HttpExchange, id: BodyId) -> Option<&BodyRef> {
    body_refs(exchange).into_iter().find(|r| r.id == id)
}

/// Обмен из файла → обмен трафика: новый id, перепривязанные тела (тела,
/// которых в файле нет, — без ссылки), тег, Pending → Aborted.
fn imported(exchange: HttpExchange, stored: &HashMap<BodyId, BodyRef>) -> HttpExchange {
    let relink = |body: Option<BodyRef>| body.and_then(|old| stored.get(&old.id).cloned());
    let mut tags = exchange.tags;
    if !tags.iter().any(|t| t == IMPORTED_TAG) {
        tags.push(IMPORTED_TAG.to_string());
    }
    HttpExchange {
        id: ExchangeId::new(),
        status: match exchange.status {
            ExchangeStatus::Pending => ExchangeStatus::Aborted,
            status => status,
        },
        request: domain::entities::exchange::HttpRequest {
            body: relink(exchange.request.body),
            ..exchange.request
        },
        response: exchange
            .response
            .map(|r| domain::entities::exchange::HttpResponse {
                body: relink(r.body),
                ..r
            }),
        client_request: exchange.client_request.map(|head| {
            let body = relink(head.body.clone());
            // Исходного тела клиента нет в файле — Repeat должен отказать, а
            // не отправить изменённое правилом тело.
            let body_lost = head.body_lost || (head.body.is_some() && body.is_none());
            domain::entities::exchange::RequestHead {
                body,
                body_lost,
                ..head
            }
        }),
        tags,
        ..exchange
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::entities::exchange::{HttpRequest, HttpResponse};
    use domain::value_objects::http_vo::{HttpVersionVo, Method, Protocol};
    use domain::value_objects::{ConnectionId, Host};

    fn body_ref() -> BodyRef {
        BodyRef {
            id: BodyId::new(),
            size: 3,
            content_type: Some("text/plain".into()),
            encoding: None,
            storage: Default::default(),
        }
    }

    #[test]
    fn imported_exchanges_get_new_ids_relinked_bodies_and_a_tag() {
        let (request_body, response_body) = (body_ref(), body_ref());
        let original = HttpExchange {
            response: Some(HttpResponse {
                body: Some(response_body.clone()),
                ..HttpResponse::empty()
            }),
            ..HttpExchange::new_pending(
                ConnectionId::new(),
                Host::new("api.test").expect("host"),
                HttpRequest {
                    method: Method::new("POST"),
                    uri: "https://api.test/x".into(),
                    version: HttpVersionVo::Http11,
                    headers: Vec::new(),
                    body: Some(request_body.clone()),
                    raw_size: 0,
                    origin: Default::default(),
                },
                Protocol::Http1,
                Utc::now(),
            )
        };
        let new_request = body_ref();
        let stored = HashMap::from([(request_body.id, new_request.clone())]);

        let copy = imported(original.clone(), &stored);

        assert_ne!(copy.id, original.id);
        assert_eq!(copy.request.body, Some(new_request));
        assert_eq!(
            copy.response.and_then(|r| r.body),
            None,
            "a body missing from the file is not linked"
        );
        assert_eq!(copy.status, ExchangeStatus::Aborted, "pending can't finish");
        assert_eq!(copy.tags, vec![IMPORTED_TAG.to_string()]);
        let again = imported(
            HttpExchange {
                tags: vec![IMPORTED_TAG.into()],
                ..original
            },
            &stored,
        );
        assert_eq!(again.tags.len(), 1, "the tag is not doubled");
    }

    #[test]
    fn a_client_body_missing_from_the_file_is_marked_lost() {
        let original = HttpExchange {
            client_request: Some(domain::entities::exchange::RequestHead {
                method: Method::new("POST"),
                uri: "https://api.test/x".into(),
                headers: Vec::new(),
                body: Some(body_ref()),
                body_lost: false,
            }),
            ..HttpExchange::new_pending(
                ConnectionId::new(),
                Host::new("api.test").expect("host"),
                HttpRequest {
                    method: Method::new("POST"),
                    uri: "https://api.test/x".into(),
                    version: HttpVersionVo::Http11,
                    headers: Vec::new(),
                    body: None,
                    raw_size: 0,
                    origin: Default::default(),
                },
                Protocol::Http1,
                Utc::now(),
            )
        };

        let copy = imported(original, &HashMap::new());

        let head = copy.client_request.expect("client request");
        assert!(head.body.is_none());
        assert!(
            head.body_lost,
            "Repeat must refuse, not send the edited body"
        );
    }
}
