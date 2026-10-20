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

/// Стартовая строка и заголовки запроса (и, где известно, его тело).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestHead {
    pub method: Method,
    pub uri: String,
    pub headers: Vec<HeaderEntry>,
    /// Тело этого запроса; `None` — то же, что у отправленного
    /// (`HttpExchange.request.body`): пайплайн тело не менял.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<BodyRef>,
    /// Пайплайн тело изменил, а исходное не сохранено (не было загружено,
    /// запись тел выключена, сбой хранилища): тела клиента нет — и
    /// подставлять отправленное нельзя.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub body_lost: bool,
}

/// Тело клиента при правке запроса пайплайном.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum ClientBody {
    /// Тело не менялось — тело клиента и есть отправленное.
    #[default]
    Unchanged,
    /// Тело изменено; исходное сохранено.
    Stored(BodyRef),
    /// Тело изменено; исходное не сохранено.
    Lost,
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

    /// Запрос для повтора — каким его прислал клиент (правила применятся
    /// заново, а не поверх уже применённых): голова и тело клиента, если
    /// пайплайн их менял, иначе отправленные. `body` — всегда заполнено,
    /// если тело записано.
    pub fn replay_head(&self) -> RequestHead {
        let head = self.client_request.clone().unwrap_or_else(|| RequestHead {
            method: self.request.method.clone(),
            uri: self.request.uri.clone(),
            headers: self.request.headers.clone(),
            body: None,
            body_lost: false,
        });
        let body = match head.body_lost {
            true => None,
            false => head.body.or_else(|| self.request.body.clone()),
        };
        RequestHead { body, ..head }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value_objects::BodyId;

    fn body(size: u64) -> BodyRef {
        BodyRef {
            id: BodyId::new(),
            size,
            content_type: None,
            encoding: None,
            storage: Default::default(),
        }
    }

    fn exchange() -> HttpExchange {
        let mut exchange = HttpExchange::new_pending(
            ConnectionId::new(),
            Host::new("api.test").expect("host"),
            HttpRequest {
                method: Method::new("POST"),
                uri: "https://api.test/sent".into(),
                version: HttpVersionVo::Http11,
                headers: vec![HeaderEntry::new("X-Sent", "1")],
                body: Some(body(5)),
                raw_size: 5,
                origin: RequestOrigin::default(),
            },
            Protocol::Http1,
            Utc::now(),
        );
        exchange.request.body = Some(body(5));
        exchange
    }

    #[test]
    fn replay_uses_the_sent_request_when_nothing_was_edited() {
        let exchange = exchange();
        let head = exchange.replay_head();
        assert_eq!(head.uri, "https://api.test/sent");
        assert_eq!(head.body, exchange.request.body);
    }

    #[test]
    fn replay_uses_client_head_and_client_body_after_edits() {
        let mut edited = exchange();
        let client_body = body(9);
        edited.client_request = Some(RequestHead {
            method: Method::new("POST"),
            uri: "https://api.test/original".into(),
            headers: vec![HeaderEntry::new("X-Client", "1")],
            body: Some(client_body.clone()),
            body_lost: false,
        });
        let head = edited.replay_head();
        assert_eq!(head.uri, "https://api.test/original");
        assert_eq!(
            head.body,
            Some(client_body),
            "the client's body, not the rewritten one"
        );

        let mut head_only = exchange();
        head_only.client_request = Some(RequestHead {
            body: None,
            ..edited.client_request.clone().expect("client")
        });
        assert_eq!(
            head_only.replay_head().body,
            head_only.request.body,
            "body untouched by the pipeline — the recorded one is the client's"
        );

        let mut lost = exchange();
        lost.client_request = Some(RequestHead {
            body: None,
            body_lost: true,
            ..edited.client_request.clone().expect("client")
        });
        assert_eq!(
            lost.replay_head().body,
            None,
            "an edited body is never passed off as the client's"
        );
    }
}
