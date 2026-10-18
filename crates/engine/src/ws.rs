//! WebSocket через прокси (Phase 11b): апгрейд-запрос идёт в origin
//! HTTP/1.1-клиентом с `Upgrade`/`Connection`; на ответ 101 обе стороны
//! апгрейдятся и байты пересылаются как есть, а парсер фреймов
//! ([`crate::ws_frames`]) публикует сообщения (`ProxyEvent::WebSocketMessage`).
//! Обмен завершается, когда закрываются оба направления.
//!
//! `Sec-WebSocket-Extensions` снимается с запроса: без permessage-deflate
//! сообщения читаемы (сжатые фреймы показать было бы нечем).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use application::dto::capture::ProxyEvent;
use application::services::EventPublisher;
use chrono::Utc;
use domain::entities::websocket::{WebSocketMessage, WsDirection, WsOpcode};
use domain::value_objects::ExchangeId;
use http::header::{CONNECTION, HeaderMap, HeaderValue, UPGRADE};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::commands::ConnGuard;
use crate::exchange_timing::ExchangeTiming;
use crate::ws_frames::WsParser;

const SEC_WEBSOCKET_EXTENSIONS: &str = "sec-websocket-extensions";

/// Запрос — апгрейд на WebSocket (HTTP/1.1, `Upgrade: websocket`).
pub(crate) fn is_upgrade(
    method: &http::Method,
    version: http::Version,
    headers: &HeaderMap,
) -> bool {
    let has_token = |name: http::header::HeaderName, token: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    };
    method == http::Method::GET
        && version == http::Version::HTTP_11
        && has_token(UPGRADE, "websocket")
        && has_token(CONNECTION, "upgrade")
}

/// Заголовки апгрейда после чистки hop-by-hop: вернуть `Upgrade` /
/// `Connection` и снять сжатие.
pub(crate) fn restore_upgrade_headers(headers: &mut HeaderMap) {
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    headers.remove(SEC_WEBSOCKET_EXTENSIONS);
}

/// То же для ответа 101 клиенту.
pub(crate) fn restore_response_headers(headers: &mut HeaderMap) {
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
}

/// Всё, что нужно ретрансляции от обмена.
pub(crate) struct WsExchange {
    pub id: ExchangeId,
    pub events: Arc<dyn EventPublisher>,
    pub timing: Arc<ExchangeTiming>,
    pub settled: Arc<AtomicBool>,
    /// Клиентское соединение: пока сокет открыт, оно занимает слот лимита
    /// и не считается закрытым (гард закроется с последней ссылкой).
    pub _connection: Arc<ConnGuard>,
}

/// Задачу прервали (Stop движка) — обмен не должен остаться pending.
impl Drop for WsExchange {
    fn drop(&mut self) {
        finish(self, Some("websocket aborted (proxy stopped)".into()), 0);
    }
}

/// Апгрейднуть обе стороны и пересылать до закрытия.
pub(crate) async fn relay(exchange: WsExchange, client: OnUpgrade, upstream: OnUpgrade) {
    let (client, upstream) = match tokio::try_join!(client, upstream) {
        Ok(pair) => pair,
        Err(error) => {
            finish(
                &exchange,
                Some(format!("websocket upgrade failed: {error}")),
                0,
            );
            return;
        }
    };
    let (client_read, client_write) = tokio::io::split(TokioIo::new(client));
    let (upstream_read, upstream_write) = tokio::io::split(TokioIo::new(upstream));
    let link = Link::default();
    let sent = pump(
        client_read,
        upstream_write,
        WsDirection::Sent,
        &exchange,
        &link,
    );
    let got = pump(
        upstream_read,
        client_write,
        WsDirection::Received,
        &exchange,
        &link,
    );
    let (sent, got) = tokio::join!(sent, got);
    // Обрыв после Close-фрейма — штатное закрытие (многие пиры рвут TCP
    // сразу после рукопожатия закрытия).
    let error = match link.closed.load(Ordering::Relaxed) {
        true => None,
        false => sent
            .err()
            .or(got.err())
            .map(|e| format!("websocket closed with error: {e}")),
    };
    finish(&exchange, error, link.bytes.load(Ordering::Relaxed));
}

/// Общее состояние двух направлений.
#[derive(Default)]
struct Link {
    /// Байт в обе стороны (размер обмена).
    bytes: AtomicU64,
    /// Close-фрейм прошёл хотя бы в одну сторону.
    closed: AtomicBool,
}

/// Одно направление: читать → писать дальше → разобрать фреймы.
async fn pump<R, W>(
    mut reader: R,
    mut writer: W,
    direction: WsDirection,
    exchange: &WsExchange,
    link: &Link,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut parser = WsParser::default();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            let _ = writer.shutdown().await;
            return Ok(());
        }
        writer.write_all(&buf[..n]).await?;
        link.bytes.fetch_add(n as u64, Ordering::Relaxed);
        for message in parser.feed(&buf[..n]) {
            if message.opcode == WsOpcode::Close {
                link.closed.store(true, Ordering::Relaxed);
            }
            exchange.events.publish(ProxyEvent::WebSocketMessage {
                id: exchange.id,
                message: WebSocketMessage {
                    direction,
                    opcode: message.opcode,
                    payload: message.payload,
                    size: message.size,
                    at: Utc::now(),
                },
            });
        }
    }
}

/// Терминальное событие обмена (ровно одно — через `settled`).
fn finish(exchange: &WsExchange, error: Option<String>, received: u64) {
    if exchange.settled.swap(true, Ordering::SeqCst) {
        return;
    }
    let event = match error {
        Some(error) => ProxyEvent::ExchangeFailed {
            id: exchange.id,
            error,
        },
        None => ProxyEvent::ExchangeCompleted {
            id: exchange.id,
            timing: exchange.timing.finalize(),
            body: None,
            raw_size: received,
        },
    };
    exchange.events.publish(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn detects_websocket_upgrade() {
        let ws = headers(&[
            ("upgrade", "WebSocket"),
            ("connection", "keep-alive, Upgrade"),
        ]);
        assert!(is_upgrade(&http::Method::GET, http::Version::HTTP_11, &ws));
        assert!(!is_upgrade(
            &http::Method::POST,
            http::Version::HTTP_11,
            &ws
        ));
        assert!(!is_upgrade(&http::Method::GET, http::Version::HTTP_2, &ws));
        let h2c = headers(&[("upgrade", "h2c"), ("connection", "Upgrade")]);
        assert!(!is_upgrade(
            &http::Method::GET,
            http::Version::HTTP_11,
            &h2c
        ));
    }

    #[test]
    fn restored_headers_drop_compression() {
        let mut map = headers(&[("sec-websocket-extensions", "permessage-deflate")]);
        restore_upgrade_headers(&mut map);
        assert_eq!(map[UPGRADE], "websocket");
        assert!(map.get(SEC_WEBSOCKET_EXTENSIONS).is_none());
    }
}
