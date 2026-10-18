//! HttpExchange — центральная сущность: один request/response обмен.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::entities::body::BodyRef;
use crate::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
use crate::value_objects::timing::TimingPhases;
use crate::value_objects::tls::TlsInfo;
use crate::value_objects::{ConnectionId, ExchangeId, Host};

/// Жизненный цикл обмена.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeStatus {
    /// Запрос ушёл, ответа ещё нет.
    Pending,
    /// Ответ получен полностью.
    Completed,
    /// Ошибка обмена.
    Failed,
    /// Соединение оборвано до завершения.
    Aborted,
    /// Повтор ранее захваченного обмена (Repeat).
    Replayed,
    /// Отправлен из Compose/Edit&Resend.
    Composed,
}

/// Происхождение запроса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestOrigin {
    #[default]
    /// Обычный запрос клиента через прокси.
    Client,
    /// Повтор (Repeat).
    Repeat,
    /// Составлен/отредактирован вручную (Compose / Edit & Resend).
    Compose,
}

/// HTTP-запрос (wire-семантика: заголовки + ссылка на тело, не сами байты).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: Method,
    /// URI как пришёл от клиента (absolute-form для прокси-запросов).
    pub uri: String,
    pub version: HttpVersionVo,
    pub headers: Vec<HeaderEntry>,
    /// Тело — по ссылке (BodyRef), байты живут в BodyRepository.
    #[serde(default)]
    pub body: Option<BodyRef>,
    /// Размер запроса «по проводу» (заголовки + тело).
    #[serde(default)]
    pub raw_size: u64,
    #[serde(default)]
    pub origin: RequestOrigin,
}

/// Стартовая строка и заголовки запроса.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestHead {
    pub method: Method,
    pub uri: String,
    pub headers: Vec<HeaderEntry>,
}

/// HTTP-ответ.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub reason: Option<String>,
    pub headers: Vec<HeaderEntry>,
    #[serde(default)]
    pub body: Option<BodyRef>,
    #[serde(default)]
    pub raw_size: u64,
    #[serde(default)]
    pub version: HttpVersionVo,
}

impl HttpResponse {
    /// «Пустой» ответ-заготовка (заполняется по мере поступления событий).
    pub fn empty() -> Self {
        Self {
            status: 0,
            reason: None,
            headers: Vec::new(),
            body: None,
            raw_size: 0,
            version: HttpVersionVo::default(),
        }
    }
}

/// Один HTTP-обмен (строка таблицы трафика).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpExchange {
    pub id: ExchangeId,
    pub connection_id: ConnectionId,
    pub started_at: DateTime<Utc>,
    pub request: HttpRequest,
    #[serde(default)]
    pub response: Option<HttpResponse>,
    pub status: ExchangeStatus,
    #[serde(default)]
    pub timing: TimingPhases,
    #[serde(default)]
    pub tls: Option<TlsInfo>,
    pub protocol: Protocol,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Хост, к которому шёл обмен (для фильтров и UI).
    pub host: Host,
    #[serde(default)]
    pub note: Option<String>,
    /// Запрос, каким его прислал клиент, если пайплайн (скрипт, Rewrite,
    /// брейкпоинт) его изменил; `request` — что ушло дальше. Repeat
    /// повторяет исходный — правила применятся заново, а не дважды.
    #[serde(default)]
    pub client_request: Option<RequestHead>,
}

impl HttpExchange {
    /// Новый Pending-обмен.
    pub fn new_pending(
        connection_id: ConnectionId,
        host: Host,
        request: HttpRequest,
        protocol: Protocol,
        started_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: ExchangeId::new(),
            connection_id,
            started_at,
            request,
            response: None,
            status: ExchangeStatus::Pending,
            timing: TimingPhases::default(),
            tls: None,
            protocol,
            tags: Vec::new(),
            host,
            note: None,
            client_request: None,
        }
    }

    /// Голова запроса для повтора: исходная клиентская, если пайплайн её
    /// менял.
    pub fn replay_head(&self) -> RequestHead {
        self.client_request.clone().unwrap_or_else(|| RequestHead {
            method: self.request.method.clone(),
            uri: self.request.uri.clone(),
            headers: self.request.headers.clone(),
        })
    }

    /// Path+query из URI (для матчинга правил и колонки Path).
    pub fn request_path(&self) -> String {
        let uri = &self.request.uri;
        if let Some((_scheme, rest)) = uri.split_once("://") {
            match rest.find('/') {
                Some(idx) => rest[idx..].to_string(),
                None => "/".to_string(),
            }
        } else {
            uri.clone()
        }
    }
}
