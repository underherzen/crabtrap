//! Repeat и подготовка Edit & Resend: захваченный запрос (с телом)
//! отправляется снова через движок или превращается в форму Compose.

use std::sync::Arc;

use domain::entities::exchange::{HttpExchange, RequestHead, RequestOrigin};
use domain::errors::DomainError;
use domain::repositories::{BodyRepository, ExchangeRepository};
use domain::value_objects::ExchangeId;

use super::form::ComposeForm;
use crate::dto::capture::HeaderEntryDto;
use crate::dto::compose::{ComposedRequestDto, SendResultDto};
use crate::services::{BodyDecoderService, DirectHttpService};

pub struct RepeatExchangeUseCase {
    exchanges: Arc<dyn ExchangeRepository>,
    bodies: Arc<dyn BodyRepository>,
    direct: Arc<dyn DirectHttpService>,
}

impl RepeatExchangeUseCase {
    pub fn new(
        exchanges: Arc<dyn ExchangeRepository>,
        bodies: Arc<dyn BodyRepository>,
        direct: Arc<dyn DirectHttpService>,
    ) -> Self {
        Self {
            exchanges,
            bodies,
            direct,
        }
    }

    /// Повторить обмен: запрос, каким его прислал клиент — голова и тело
    /// (правила, скрипты и брейкпоинты применятся заново, а не поверх уже
    /// применённых). Тело захвачено не полностью — отказ, а не урезанный
    /// повтор.
    pub async fn repeat(&self, id: &ExchangeId) -> Result<SendResultDto, DomainError> {
        let (exchange, head, body) = self.load(id).await?;
        match (client_body_edited(&exchange), &body) {
            (false, _) => check_body_complete(&exchange, body.as_deref())?,
            // Отправить вместо тела клиента изменённое — повторить правку дважды.
            (true, None) => {
                return Err(DomainError::ValidationError(
                    "a rule changed this request's body and the original isn't stored — use Edit & Resend".into(),
                ));
            }
            (true, Some(_)) => {}
        }
        let request = ComposedRequestDto {
            method: head.method.to_string(),
            url: head.uri,
            headers: head.headers.into_iter().map(HeaderEntryDto::from).collect(),
            body,
            origin: RequestOrigin::Repeat,
        };
        self.direct.send(request).await
    }

    /// Форма Compose из обмена (Edit & Resend): исходный запрос клиента.
    /// Тело клиента вытеснено из хранилища — форма с отправленным телом
    /// (его можно поправить руками).
    pub async fn edit_form(&self, id: &ExchangeId) -> Result<ComposeForm, DomainError> {
        let (exchange, head, body) = self.load(id).await?;
        let body = match (body, &exchange.request.body) {
            (Some(body), _) => Some(body),
            (None, Some(sent)) => self.bodies.get(sent).await?,
            (None, None) => None,
        };
        Ok(ComposeForm::from_head(&head, body.as_deref()))
    }

    /// Обмен, запрос клиента и тело этого запроса (`None` — нет или уже
    /// не хранится).
    async fn load(
        &self,
        id: &ExchangeId,
    ) -> Result<(HttpExchange, RequestHead, Option<Vec<u8>>), DomainError> {
        let exchange = self
            .exchanges
            .get(id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("exchange {id}")))?;
        let head = exchange.replay_head();
        let body = match &head.body {
            Some(body) => self.bodies.get(body).await?,
            None => None,
        };
        Ok((exchange, head, body))
    }
}

/// Пайплайн изменил тело — у клиента было своё (сохранённое или
/// потерянное), отправленное за него выдавать нельзя.
fn client_body_edited(exchange: &HttpExchange) -> bool {
    exchange
        .client_request
        .as_ref()
        .is_some_and(|client| client.body.is_some() || client.body_lost)
}

/// Захвачено ли тело запроса целиком: объявленная длина / размер по
/// проводу против сохранённых байт.
fn check_body_complete(exchange: &HttpExchange, body: Option<&[u8]>) -> Result<(), DomainError> {
    let header = |name: &str| {
        exchange
            .request
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.trim().to_string())
    };
    let declared = header("content-length").and_then(|v| v.parse::<u64>().ok());
    let chunked =
        header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let stored = body.map_or(0, |b| b.len() as u64);
    // raw_size после RequestBodyDone — байты тела, ушедшие дальше (после
    // правок пайплайна; объявленная длина клиента тогда устарела).
    let on_wire = exchange
        .request
        .body
        .as_ref()
        .map(|_| exchange.request.raw_size);
    let expected = on_wire.or(declared).unwrap_or(0);
    if body.is_none() && (expected > 0 || chunked) {
        return Err(DomainError::ValidationError(
            "the request body was not captured (body recording is off or it is still uploading) — use Edit & Resend".into(),
        ));
    }
    if stored < expected {
        return Err(DomainError::ValidationError(format!(
            "only {stored} of {expected} body bytes were captured — the request cannot be repeated exactly"
        )));
    }
    Ok(())
}

/// Отправить запрос из окна Compose. Тело ответа — распакованное по
/// Content-Encoding (для показа; в трафике — как пришло).
pub struct SendComposedRequestUseCase {
    direct: Arc<dyn DirectHttpService>,
    decoder: Arc<dyn BodyDecoderService>,
}

impl SendComposedRequestUseCase {
    pub fn new(direct: Arc<dyn DirectHttpService>, decoder: Arc<dyn BodyDecoderService>) -> Self {
        Self { direct, decoder }
    }

    pub async fn execute(&self, form: &ComposeForm) -> Result<SendResultDto, DomainError> {
        let request = form
            .to_request(RequestOrigin::Compose)
            .map_err(DomainError::ValidationError)?;
        let response = self.direct.send(request).await?;
        let encoding = response
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("content-encoding"))
            .map(|h| h.value.clone());
        let body = match (response.body.as_deref(), encoding) {
            (Some(body), Some(encoding)) => Some(
                self.decoder
                    .decode(body, &encoding)
                    .await
                    .unwrap_or_else(|_| body.to_vec()),
            ),
            (body, _) => body.map(<[u8]>::to_vec),
        };
        Ok(SendResultDto { body, ..response })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use domain::entities::BodyRef;
    use domain::entities::body::BodyStorage;
    use domain::entities::exchange::HttpRequest;
    use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
    use domain::value_objects::{BodyId, ConnectionId, Host};

    fn exchange(headers: Vec<HeaderEntry>, body: Option<u64>, raw_size: u64) -> HttpExchange {
        HttpExchange::new_pending(
            ConnectionId::new(),
            Host::new("a.test").expect("host"),
            HttpRequest {
                method: Method::new("POST"),
                uri: "https://a.test/".into(),
                version: HttpVersionVo::Http11,
                headers,
                body: body.map(|size| BodyRef {
                    id: BodyId::new(),
                    size,
                    content_type: None,
                    encoding: None,
                    storage: BodyStorage::Memory,
                }),
                raw_size,
                origin: Default::default(),
            },
            Protocol::Http1,
            Utc::now(),
        )
    }

    fn length(n: u64) -> Vec<HeaderEntry> {
        vec![HeaderEntry {
            name: "Content-Length".into(),
            value: n.to_string(),
        }]
    }

    #[test]
    fn body_completeness() {
        assert!(
            check_body_complete(&exchange(Vec::new(), None, 0), None).is_ok(),
            "GET without body"
        );
        assert!(
            check_body_complete(&exchange(length(5), None, 0), None).is_err(),
            "body not captured"
        );
        assert!(
            check_body_complete(&exchange(length(10), Some(4), 10), Some(b"abcd")).is_err(),
            "truncated"
        );
        assert!(check_body_complete(&exchange(length(4), Some(4), 4), Some(b"abcd")).is_ok());
        // Пайплайн заменил тело: объявленная длина клиента устарела.
        assert!(check_body_complete(&exchange(length(99), Some(4), 4), Some(b"abcd")).is_ok());
    }
}
