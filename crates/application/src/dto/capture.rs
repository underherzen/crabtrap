//! DTO захвата трафика: строка таблицы, статистика и события движка.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use domain::entities::BodyRef;
use domain::entities::exchange::HttpExchange;
use domain::value_objects::http_vo::{HeaderEntry, Protocol};
use domain::value_objects::{ConnectionId, ExchangeId};

/// Направление потока байт.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Request,
    Response,
}

/// Параметры TLS-сессии обмена (событие ExchangeStarted; доменный
/// `TlsInfo` без служебных полей сертификата).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsInfoDto {
    /// SNI, отправленный клиентом.
    pub sni: Option<String>,
    /// Выбранный ALPN-протокол.
    pub alpn: Option<String>,
    /// Версия TLS (например, `TLSv1.3`).
    pub version: Option<String>,
    /// Suite-имя шифра.
    pub cipher_suite: Option<String>,
    /// Соединение расшифровано MITM-прокси.
    pub mitm: bool,
}

impl From<domain::value_objects::tls::TlsInfo> for TlsInfoDto {
    fn from(t: domain::value_objects::tls::TlsInfo) -> Self {
        Self {
            sni: t.sni,
            alpn: t.alpn,
            version: t.version,
            cipher_suite: t.cipher_suite,
            mitm: t.mitm,
        }
    }
}

impl From<TlsInfoDto> for domain::value_objects::tls::TlsInfo {
    fn from(t: TlsInfoDto) -> Self {
        Self {
            sni: t.sni,
            alpn: t.alpn,
            version: t.version,
            cipher_suite: t.cipher_suite,
            mitm: t.mitm,
            leaf_not_after: None,
            peer_subject: None,
        }
    }
}

/// Заголовок в событиях (упрощённый HeaderEntry без доменной логики).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderEntryDto {
    pub name: String,
    pub value: String,
}

impl From<HeaderEntry> for HeaderEntryDto {
    fn from(h: HeaderEntry) -> Self {
        Self {
            name: h.name,
            value: h.value,
        }
    }
}

impl From<HeaderEntryDto> for HeaderEntry {
    fn from(h: HeaderEntryDto) -> Self {
        Self {
            name: h.name,
            value: h.value,
        }
    }
}

/// Строка таблицы трафика.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExchangeSummaryDto {
    pub id: ExchangeId,
    /// Нужен для воссоздания HttpExchange по событию (в UI не показывается).
    pub connection_id: ConnectionId,
    pub started_at: DateTime<Utc>,
    pub method: String,
    pub scheme: String,
    pub host: String,
    pub path: String,
    pub status: Option<u16>,
    pub req_size: u64,
    pub resp_size: u64,
    pub duration_ms: f64,
    pub protocol: Protocol,
    pub tls_mitm: Option<bool>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl ExchangeSummaryDto {
    /// Проекция доменной сущности для UI-таблицы.
    pub fn from_exchange(exchange: &HttpExchange) -> Self {
        let (scheme, path) = split_uri(&exchange.request.uri);
        Self {
            id: exchange.id,
            connection_id: exchange.connection_id,
            started_at: exchange.started_at,
            method: exchange.request.method.as_str().to_string(),
            scheme,
            host: exchange.host.to_string(),
            path,
            status: exchange.response.as_ref().map(|r| r.status),
            req_size: exchange.request.raw_size,
            resp_size: exchange.response.as_ref().map(|r| r.raw_size).unwrap_or(0),
            duration_ms: exchange.timing.total_ms,
            protocol: exchange.protocol,
            tls_mitm: exchange.tls.as_ref().map(|t| t.mitm),
            tags: exchange.tags.clone(),
        }
    }
}

fn split_uri(uri: &str) -> (String, String) {
    match uri.split_once("://") {
        Some((scheme, rest)) => match rest.find('/') {
            Some(idx) => (scheme.to_string(), rest[idx..].to_string()),
            None => (scheme.to_string(), "/".to_string()),
        },
        // origin-form (например, после CONNECT-ветки): считаем http.
        None => ("http".to_string(), uri.to_string()),
    }
}

/// Статистика захвата (статус-бар).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TrafficStatsDto {
    pub total_exchanges: u64,
    pub total_bytes: u64,
    pub by_status: HashMap<u16, u64>,
}

/// Событие движка → UI. Единственный транспорт событий (через
/// `async-channel`), никакого прямого вызова UI из engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProxyEvent {
    ConnectionOpened {
        id: ConnectionId,
        /// `ip:port` клиента.
        peer: String,
        ts: DateTime<Utc>,
    },
    ConnectionClosed {
        id: ConnectionId,
    },
    /// Запрос получен (заголовки); строка уже видна в таблице.
    ExchangeStarted {
        exchange: ExchangeSummaryDto,
        /// Параметры TLS-сессии (Some — расшифрованный MITM-exchange;
        /// Phase 4; serde default — совместимость со старыми потоками).
        #[serde(default)]
        tls: Option<TlsInfoDto>,
        /// Заголовки запроса в том виде, как их прислал клиент (до чистки
        /// hop-by-hop). Без них инспектор и поиск не видят request headers.
        #[serde(default)]
        request_headers: Vec<HeaderEntryDto>,
        /// Версия HTTP запроса клиента.
        #[serde(default)]
        request_version: domain::value_objects::http_vo::HttpVersionVo,
    },
    /// Пайплайн перехвата изменил запрос (скрипт, rewrite): что реально
    /// ушло дальше. Хост строки трафика остаётся исходным (куда обращался
    /// клиент), инспектор показывает итоговый запрос.
    RequestEdited {
        id: ExchangeId,
        method: String,
        /// Итоговый absolute URI.
        uri: String,
        /// Path + query итогового URI (колонка Path).
        path: String,
        headers: Vec<HeaderEntryDto>,
        /// Тело, каким его прислал клиент (для Repeat): не менялось,
        /// сохранено или потеряно.
        #[serde(default)]
        client_body: domain::entities::ClientBody,
    },
    /// Тело запроса догрузилось. `body = None` — захват выключен
    /// (record_bodies), тело пусто, превышен cap (сохраняется обрезанный
    /// префикс — тогда Some) или сохранение не удалось.
    RequestBodyDone {
        id: ExchangeId,
        body: Option<BodyRef>,
        raw_size: u64,
    },
    /// Заголовки ответа получены. Версия — с провода (Phase 2+); reason-фраза
    /// в http-крейте не сохраняется, домен достроит canonical при надобности.
    ResponseHeaders {
        id: ExchangeId,
        status: u16,
        #[serde(default)]
        version: domain::value_objects::http_vo::HttpVersionVo,
        headers: Vec<HeaderEntryDto>,
        ts: DateTime<Utc>,
    },
    /// Прогресс тела (потоковая семантика, шлётся периодически).
    BodyProgress {
        id: ExchangeId,
        direction: Direction,
        bytes_so_far: u64,
    },
    /// Обмен завершён (финальные timing + тело).
    ExchangeCompleted {
        id: ExchangeId,
        timing: domain::value_objects::TimingPhases,
        body: Option<BodyRef>,
        raw_size: u64,
    },
    ExchangeFailed {
        id: ExchangeId,
        error: String,
    },
    /// Сообщение WebSocket-соединения (Phase 11b).
    WebSocketMessage {
        id: ExchangeId,
        message: domain::entities::websocket::WebSocketMessage,
    },
    /// Обмену назначен тег (маршрут правил: `blocked`, `map-local`,
    /// `map-remote`, `throttled`, …).
    ExchangeTagged {
        id: ExchangeId,
        tag: String,
    },
    /// Обмен припаркован на брейкпоинте (Phase 7): что показать и дать
    /// отредактировать в окне брейкпоинта.
    InterceptWaiting {
        preview: crate::dto::rules::InterceptPreviewDto,
        ts: DateTime<Utc>,
    },
    InterceptResolved {
        id: domain::value_objects::InterceptId,
    },
    /// Запись консоли скрипта (`console.log`, ошибка выполнения) — панель
    /// Console окна Script Editor (Track S).
    ScriptLog {
        rule_id: domain::value_objects::RuleId,
        /// `None` — пробный запуск из редактора.
        exchange_id: Option<ExchangeId>,
        level: crate::dto::scripting::ScriptLogLevel,
        message: String,
        ts: DateTime<Utc>,
    },
    /// Изменение состояния движка (листенер встал/упал и т.п.).
    EngineStatus {
        listening: Option<String>,
        active_conns: usize,
        error: Option<String>,
    },
}
