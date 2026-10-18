//! Тела сообщений для пайплайнов, меняющих содержимое (скрипты, Rewrite):
//! распаковка по Content-Encoding перед правкой и сборка результата.
//!
//! Правило одно для всех: не тронутое тело уходит как было (сжатым, со
//! своим Content-Encoding), заменённое — новыми байтами без
//! Content-Encoding (движок выставит Content-Length).

use std::sync::Arc;

use application::services::{BodyDecoderService, InterceptableMessage};
use domain::DomainError;
use domain::value_objects::http_vo::HeaderEntry;

/// Распаковка тел сообщений.
#[derive(Clone)]
pub struct MessageBodies {
    decoder: Arc<dyn BodyDecoderService>,
}

impl MessageBodies {
    pub fn new(decoder: Arc<dyn BodyDecoderService>) -> Self {
        Self { decoder }
    }

    /// Тело без Content-Encoding (`None` — не загружено или не
    /// распаковалось).
    pub async fn plain(&self, message: &InterceptableMessage) -> Option<Vec<u8>> {
        let body = message.body.as_deref()?;
        match content_encoding(&message.headers) {
            None => Some(body.to_vec()),
            Some(encoding) => match self.decode(body, encoding).await {
                Ok(plain) => Some(plain),
                Err(error) => {
                    tracing::debug!(%error, encoding, "body left as is: cannot decode");
                    None
                }
            },
        }
    }

    /// Распаковка — CPU-работа до десятков МБ: вне async-потоков tokio.
    async fn decode(&self, body: &[u8], encoding: &str) -> Result<Vec<u8>, DomainError> {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return self.decoder.decode(body, encoding).await;
        };
        let (decoder, body, encoding) = (
            Arc::clone(&self.decoder),
            body.to_vec(),
            encoding.to_string(),
        );
        runtime
            .spawn_blocking(move || futures::executor::block_on(decoder.decode(&body, &encoding)))
            .await
            .map_err(|e| DomainError::InfrastructureError(format!("decode task failed: {e}")))?
    }
}

/// Значимый Content-Encoding (не `identity`).
pub fn content_encoding(headers: &[HeaderEntry]) -> Option<&str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-encoding"))
        .map(|h| h.value.trim())
        .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("identity"))
}

/// Заголовки и тело после правки. `before` — распакованное тело, которое
/// видела правка, `after` — что она вернула.
pub fn finish_body(
    original: &InterceptableMessage,
    before: Option<&[u8]>,
    after: Option<Vec<u8>>,
    headers: Vec<HeaderEntry>,
) -> (Vec<HeaderEntry>, Option<Vec<u8>>) {
    if after.as_deref() == before {
        // Тело уходит как было (сжатым) — его Content-Encoding обязателен,
        // даже если правка удалила заголовок.
        let encoding = original
            .headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("content-encoding"))
            .filter(|_| {
                !headers
                    .iter()
                    .any(|h| h.name.eq_ignore_ascii_case("content-encoding"))
            });
        let headers = headers.into_iter().chain(encoding.cloned()).collect();
        return (headers, original.body.clone());
    }
    let headers = headers
        .into_iter()
        .filter(|h| !h.name.eq_ignore_ascii_case("content-encoding"))
        .collect();
    (headers, after)
}
