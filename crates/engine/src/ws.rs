//! WebSocket через прокси (Phase 11b): апгрейд-запрос идёт в origin
//! HTTP/1.1-клиентом с `Upgrade`/`Connection`; на ответ 101 обе стороны
//! апгрейдятся и байты пересылаются как есть, а парсер фреймов
//! ([`crate::ws_frames`]) публикует сообщения (`ProxyEvent::WebSocketMessage`).
//! Обмен завершается, когда закрываются оба направления.
//!
//! `Sec-WebSocket-Extensions` снимается с запроса: без permessage-deflate
//! сообщения читаемы (сжатые фреймы показать было бы нечем).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use application::dto::capture::ProxyEvent;
use application::services::EventPublisher;
use chrono::Utc;
use domain::entities::websocket::{WS_PAYLOAD_CAP, WebSocketMessage, WsDirection, WsOpcode};
use domain::value_objects::ExchangeId;
use http::header::{CONNECTION, HeaderMap, HeaderValue, UPGRADE};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;

use crate::commands::ConnGuard;
use crate::exchange_timing::ExchangeTiming;
use crate::ws_frames::{WsParser, encode_frame};
use crate::ws_inject::{Injection, QUEUE, Queues, WsSockets};

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
    /// Регистр открытых сокетов (отправка сообщений из UI).
    pub sockets: Arc<WsSockets>,
}

/// Сколько сообщение из UI ждёт границы кадра, прежде чем получить отказ.
const INJECTION_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Задачу прервали (Stop движка) — обмен не должен остаться pending, а
/// сокет — в регистре.
impl Drop for WsExchange {
    fn drop(&mut self) {
        self.sockets.remove(&self.id);
        finish(self, Some("websocket aborted (proxy stopped)".into()), 0);
    }
}

/// Апгрейднуть обе стороны и пересылать до закрытия. `queues` — сокет уже
/// в регистре (до ответа 101: сообщение из UI сразу после открытия не
/// должно получить «closed»).
pub(crate) async fn relay(
    exchange: WsExchange,
    queues: Queues,
    client: OnUpgrade,
    upstream: OnUpgrade,
) {
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
    let sink = Sink {
        id: exchange.id,
        events: exchange.events.clone(),
    };
    let sent = pump(
        Stream {
            reader: client_read,
            writer: upstream_write,
            direction: WsDirection::Sent,
            injections: queues.to_server,
        },
        &sink,
        &link,
    );
    let got = pump(
        Stream {
            reader: upstream_read,
            writer: client_write,
            direction: WsDirection::Received,
            injections: queues.to_client,
        },
        &sink,
        &link,
    );
    let (sent, got) = tokio::join!(sent, got);
    exchange.sockets.remove(&exchange.id);
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

/// Одно направление: откуда читать, куда писать, что вставлять.
struct Stream<R, W> {
    reader: R,
    writer: W,
    direction: WsDirection,
    /// Сообщения из UI для этого направления.
    injections: mpsc::Receiver<Injection>,
}

/// Куда публиковать сообщения направления (id обмена и издатель).
struct Sink {
    id: ExchangeId,
    events: Arc<dyn EventPublisher>,
}

/// Сообщение из UI, ждущее границы кадра.
struct Waiting {
    injection: Injection,
    since: tokio::time::Instant,
}

/// Одно направление: читать → писать дальше → разобрать фреймы; сообщения
/// из UI — в поток на границе сообщения (кадр клиента не разрезать). Чем бы
/// направление ни кончилось, каждое неотправленное сообщение получает
/// `WebSocketSendFailed`.
async fn pump<R, W>(mut stream: Stream<R, W>, sink: &Sink, link: &Link) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut waiting: VecDeque<Waiting> = VecDeque::new();
    let result = relay_direction(&mut stream, &mut waiting, sink, link).await;
    let reason = match &result {
        Ok(()) => "the WebSocket is closed".to_string(),
        Err(error) => format!("the WebSocket failed: {error}"),
    };
    reject_all(&mut waiting, sink, &reason);
    stream.injections.close();
    while stream.injections.try_recv().is_ok() {
        reject(sink, &reason);
    }
    result
}

async fn relay_direction<R, W>(
    stream: &mut Stream<R, W>,
    waiting: &mut VecDeque<Waiting>,
    sink: &Sink,
    link: &Link,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut parser = WsParser::default();
    let mut buf = vec![0u8; 16 * 1024];
    let mut injections_open = true;
    loop {
        let deadline = waiting.front().map(|w| w.since + INJECTION_WAIT_LIMIT);
        tokio::select! {
            read = stream.reader.read(&mut buf) => {
                let n = read?;
                if n == 0 {
                    let _ = stream.writer.shutdown().await;
                    return Ok(());
                }
                stream.writer.write_all(&buf[..n]).await?;
                link.bytes.fetch_add(n as u64, Ordering::Relaxed);
                for message in parser.feed(&buf[..n]) {
                    if message.opcode == WsOpcode::Close {
                        link.closed.store(true, Ordering::Relaxed);
                    }
                    publish(sink, stream.direction, message.opcode, message.payload, message.size, false);
                }
            }
            // Полная очередь не опустошается: канал заполнится, и UI
            // получит «too many messages», а не бесконечный буфер.
            injection = stream.injections.recv(), if injections_open && waiting.len() < QUEUE => {
                match injection {
                    Some(injection) => waiting.push_back(Waiting {
                        injection,
                        since: tokio::time::Instant::now(),
                    }),
                    None => injections_open = false,
                }
            }
            () = tokio::time::sleep_until(deadline.unwrap_or_else(tokio::time::Instant::now)), if deadline.is_some() => {
                reject_expired(waiting, sink);
            }
        }
        flush_injections(stream, waiting, &parser, sink, link).await?;
    }
}

/// Сообщения, прождавшие границу кадра дольше [`INJECTION_WAIT_LIMIT`]:
/// поздно отправленное сообщение хуже ошибки.
fn reject_expired(waiting: &mut VecDeque<Waiting>, sink: &Sink) {
    let now = tokio::time::Instant::now();
    while waiting
        .front()
        .is_some_and(|w| w.since + INJECTION_WAIT_LIMIT <= now)
    {
        if waiting.pop_front().is_some() {
            reject(
                sink,
                "the other side is in the middle of a message for too long — try again",
            );
        }
    }
}

/// Записать ждущие сообщения, если поток стоит на границе сообщения.
async fn flush_injections<R, W>(
    stream: &mut Stream<R, W>,
    waiting: &mut VecDeque<Waiting>,
    parser: &WsParser,
    sink: &Sink,
    link: &Link,
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    if waiting.is_empty() {
        return Ok(());
    }
    if link.closed.load(Ordering::Relaxed) {
        reject_all(waiting, sink, "the WebSocket is closing");
        return Ok(());
    }
    if parser.is_broken() {
        reject_all(
            waiting,
            sink,
            "the stream is not WebSocket framing crabtrap can read",
        );
        return Ok(());
    }
    if !parser.at_message_boundary() {
        return Ok(());
    }
    while let Some(Waiting { injection, .. }) = waiting.pop_front() {
        // К серверу — с маской (RFC 6455 §5.3: непредсказуемый ключ), к
        // клиенту — без.
        let mask = (stream.direction == WsDirection::Sent).then(mask_key);
        let frame = encode_frame(injection.opcode, &injection.payload, mask);
        if let Err(error) = stream.writer.write_all(&frame).await {
            reject(sink, &format!("the WebSocket failed: {error}"));
            return Err(error);
        }
        link.bytes.fetch_add(frame.len() as u64, Ordering::Relaxed);
        let size = injection.payload.len() as u64;
        let mut payload = injection.payload;
        payload.truncate(WS_PAYLOAD_CAP);
        publish(
            sink,
            stream.direction,
            injection.opcode,
            payload,
            size,
            true,
        );
    }
    Ok(())
}

fn reject_all(waiting: &mut VecDeque<Waiting>, sink: &Sink, error: &str) {
    for _ in waiting.drain(..) {
        reject(sink, error);
    }
}

fn reject(sink: &Sink, error: &str) {
    sink.events.publish(ProxyEvent::WebSocketSendFailed {
        id: sink.id,
        error: error.to_string(),
    });
}

/// Ключ маски кадра к серверу из системного CSPRNG.
fn mask_key() -> [u8; 4] {
    use ring::rand::SecureRandom as _;
    let mut key = [0u8; 4];
    if ring::rand::SystemRandom::new().fill(&mut key).is_err() {
        // CSPRNG ОС недоступен — крайне маловероятно; ключ всё равно
        // ненулевой и меняется от кадра к кадру.
        key = fastrand::u32(1..).to_be_bytes();
    }
    key
}

fn publish(
    sink: &Sink,
    direction: WsDirection,
    opcode: WsOpcode,
    payload: Vec<u8>,
    size: u64,
    injected: bool,
) {
    sink.events.publish(ProxyEvent::WebSocketMessage {
        id: sink.id,
        message: WebSocketMessage {
            direction,
            opcode,
            payload,
            size,
            at: Utc::now(),
            injected,
        },
    });
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

    /// Издатель для тестов: события — в вектор.
    #[derive(Default)]
    struct Recorded(std::sync::Mutex<Vec<ProxyEvent>>);

    impl EventPublisher for Recorded {
        fn publish(&self, event: ProxyEvent) {
            self.0.lock().expect("events").push(event);
        }
    }

    fn server_frame(payload: &[u8]) -> Vec<u8> {
        encode_frame(WsOpcode::Text, payload, None)
    }

    /// Прочитать из `out` ровно `len` байт (с таймаутом).
    async fn read_out(out: &mut tokio::io::DuplexStream, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        tokio::time::timeout(std::time::Duration::from_secs(5), out.read_exact(&mut buf))
            .await
            .expect("bytes in time")
            .expect("read");
        buf
    }

    fn failures(events: &Recorded) -> Vec<String> {
        events
            .0
            .lock()
            .expect("events")
            .iter()
            .filter_map(|e| match e {
                ProxyEvent::WebSocketSendFailed { error, .. } => Some(error.clone()),
                _ => None,
            })
            .collect()
    }

    /// Направление «к клиенту» посреди кадра сервера, вставка уже у pump.
    async fn stuck_mid_frame(
        events: Arc<Recorded>,
    ) -> (
        tokio::io::DuplexStream,
        tokio::io::DuplexStream,
        mpsc::Sender<Injection>,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ) {
        let (mut peer, reader) = tokio::io::duplex(1024);
        let (writer, mut out) = tokio::io::duplex(1024);
        let (tx, injections) = mpsc::channel(4);
        let sink = Sink {
            id: ExchangeId::new(),
            events,
        };
        let stream = Stream {
            reader,
            writer,
            direction: WsDirection::Received,
            injections,
        };
        let pump = tokio::spawn(async move { pump(stream, &sink, &Link::default()).await });
        // Половина кадра прошла насквозь — парсер посреди кадра.
        let half = &server_frame(b"hello")[..3];
        peer.write_all(half).await.expect("half a frame");
        assert_eq!(read_out(&mut out, half.len()).await, half);
        tx.send(Injection {
            opcode: WsOpcode::Text,
            payload: b"mine".to_vec(),
        })
        .await
        .expect("inject");
        while tx.capacity() < tx.max_capacity() {
            tokio::task::yield_now().await;
        }
        (peer, out, tx, pump)
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_waiting_too_long_is_refused() {
        let events = Arc::new(Recorded::default());
        let (_peer, _out, _tx, pump) = stuck_mid_frame(events.clone()).await;

        tokio::time::sleep(INJECTION_WAIT_LIMIT + std::time::Duration::from_secs(1)).await;

        let failed = failures(&events);
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0].contains("too long"), "{failed:?}");
        pump.abort();
    }

    #[tokio::test]
    async fn waiting_messages_are_refused_when_the_stream_ends() {
        let events = Arc::new(Recorded::default());
        let (peer, _out, _tx, pump) = stuck_mid_frame(events.clone()).await;

        // Конец потока посреди кадра: ждущая вставка получает отказ, а не
        // пропадает.
        drop(peer);
        pump.await.expect("join").expect("pump");

        let failed = failures(&events);
        assert_eq!(failed, vec!["the WebSocket is closed".to_string()]);
    }

    #[tokio::test]
    async fn an_injected_message_waits_for_the_frame_in_flight() {
        let (mut peer, reader) = tokio::io::duplex(1024);
        let (writer, mut out) = tokio::io::duplex(1024);
        let (tx, injections) = mpsc::channel(4);
        let events = Arc::new(Recorded::default());
        let sink = Sink {
            id: ExchangeId::new(),
            events: events.clone(),
        };
        let link = Link::default();
        let stream = Stream {
            reader,
            writer,
            direction: WsDirection::Received,
            injections,
        };
        let pump = pump(stream, &sink, &link);
        let script = async {
            let frame = server_frame(b"hello");
            peer.write_all(&frame[..3]).await.expect("half");
            assert_eq!(read_out(&mut out, 3).await, frame[..3]);
            tx.send(Injection {
                opcode: WsOpcode::Text,
                payload: b"mine".to_vec(),
            })
            .await
            .expect("inject");
            // Дождаться, пока pump заберёт вставку из канала.
            while tx.capacity() < tx.max_capacity() {
                tokio::task::yield_now().await;
            }
            // Вставка получена посреди кадра — в выход ничего не ушло.
            let mut probe = [0u8; 1];
            let early =
                tokio::time::timeout(std::time::Duration::from_millis(100), out.read(&mut probe))
                    .await;
            assert!(early.is_err(), "nothing may be written mid-frame");
            peer.write_all(&frame[3..]).await.expect("rest");
            // Сначала конец чужого кадра, потом свой — целиком.
            assert_eq!(read_out(&mut out, frame.len() - 3).await, frame[3..]);
            let mine = server_frame(b"mine");
            assert_eq!(read_out(&mut out, mine.len()).await, mine);
            drop(peer);
        };
        let (result, ()) = tokio::join!(pump, script);
        result.expect("pump");
        let injected = events
            .0
            .lock()
            .expect("events")
            .iter()
            .filter(
                |e| matches!(e, ProxyEvent::WebSocketMessage { message, .. } if message.injected),
            )
            .count();
        assert_eq!(injected, 1);
    }

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
