//! Запись CONNECT-туннеля в трафик (как в Charles): туннель без расшифровки,
//! отказ в CONNECT и неудачный дозвон — видимые обмены с методом CONNECT.
//!
//! Обмен проходит обычный жизненный цикл событий, поэтому capture-конвейер
//! и таблица не знают о туннелях ничего особого:
//! ExchangeStarted → ResponseHeaders(200) → RequestBodyDone(байты вверх) →
//! ExchangeCompleted(байты вниз) | ExchangeFailed(причина).
//!
//! MITM-туннели здесь не пишутся: их расшифрованные запросы и так видны.

use std::sync::Arc;
use std::time::Instant;

use application::dto::capture::{ExchangeSummaryDto, ProxyEvent};
use application::services::EventPublisher;
use chrono::Utc;
use domain::value_objects::http_vo::Protocol;
use domain::value_objects::{ConnectionId, ExchangeId, TimingPhases};

/// Тег строки таблицы: прозрачный туннель (байты не расшифровывались).
pub(crate) const TUNNEL_TAG: &str = "tunnel";

/// Обмен-запись одного CONNECT.
///
/// Drop без `failed`/`closed` (отмена future CONNECT, абортнутая на Stop
/// туннельная задача) публикует ExchangeFailed — строка не висит в pending.
pub(crate) struct TunnelRecord {
    events: Arc<dyn EventPublisher>,
    id: ExchangeId,
    started: Instant,
    finished: bool,
}

impl TunnelRecord {
    /// Опубликовать ExchangeStarted для `CONNECT host:port`.
    pub(crate) fn start(
        events: Arc<dyn EventPublisher>,
        connection_id: ConnectionId,
        host: &str,
        port: u16,
    ) -> Self {
        let id = ExchangeId::new();
        events.publish(ProxyEvent::ExchangeStarted {
            exchange: ExchangeSummaryDto {
                id,
                connection_id,
                started_at: Utc::now(),
                method: "CONNECT".to_string(),
                scheme: "https".to_string(),
                // Нестандартный порт — в authority (`host:8443`), как в Charles.
                host: if port == 443 {
                    host.to_string()
                } else {
                    format!("{host}:{port}")
                },
                path: String::new(),
                status: None,
                req_size: 0,
                resp_size: 0,
                duration_ms: 0.0,
                protocol: Protocol::Http1,
                tls_mitm: Some(false),
                tags: vec![TUNNEL_TAG.to_string()],
            },
            tls: None,
            request_headers: Vec::new(),
            request_version: Default::default(),
        });
        Self {
            events,
            id,
            started: Instant::now(),
            finished: false,
        }
    }

    /// Прокси ответил на CONNECT статусом (200 — туннель поднят, 403 — блок).
    pub(crate) fn responded(&self, status: u16) {
        self.events.publish(ProxyEvent::ResponseHeaders {
            id: self.id,
            status,
            version: Default::default(),
            headers: Vec::new(),
            ts: Utc::now(),
        });
    }

    /// CONNECT не удался (дозвон, выпуск сертификата, upgrade).
    pub(crate) fn failed(mut self, error: impl Into<String>) {
        self.finished = true;
        self.events.publish(ProxyEvent::ExchangeFailed {
            id: self.id,
            error: error.into(),
        });
    }

    /// Туннель закрыт: `up` — байты клиент → origin, `down` — обратно.
    pub(crate) fn closed(mut self, up: u64, down: u64) {
        self.finished = true;
        let total_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        self.events.publish(ProxyEvent::RequestBodyDone {
            id: self.id,
            body: None,
            raw_size: up,
        });
        self.events.publish(ProxyEvent::ExchangeCompleted {
            id: self.id,
            timing: TimingPhases {
                total_ms,
                ..Default::default()
            },
            body: None,
            raw_size: down,
        });
    }
}

impl Drop for TunnelRecord {
    fn drop(&mut self) {
        if !self.finished {
            self.events.publish(ProxyEvent::ExchangeFailed {
                id: self.id,
                error: "tunnel aborted".to_string(),
            });
        }
    }
}
