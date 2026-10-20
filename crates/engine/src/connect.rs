//! CONNECT host:port → MITM-расшифровка или прозрачный туннель (Phase 3).
//!
//! Порядок «dial-first»: цель дозванивается ЕЩЁ ДО ответа `200 Connection
//! Established` (hyper позволяет await-ить connect внутри service-вызова).
//! Это даёт честную семантику ошибок — недоступный origin получает `502
//! Bad Gateway` от прокси, а не «туннель установился и сразу умер», что
//! невозможно после отправки 200 (HTTP-ответ нельзя заменить). Плата —
//! клиент ждёт коннекта до первого байта ответа; для отладочного прокси
//! это честнее.
//!
//! Решение Mitm|Passthrough: список SSL-Proxying (Charles-семантика —
//! пустой список = выключено) поверх пайплайна; `Block` пайплайна важнее.
//!
//! Ветка Mitm: dial-first (probe), выпуск листа ДО хендшейка, `200`,
//! upgrade → tokio-rustls server (ALPN только http/1.1) → hyper http1 с
//! ProxyService в MITM-контексте (origin-form запросы переписываются в
//! https absolute-form). Диалап-probe закрывается: реальный трафик идёт
//! через пул UpstreamClient (hyper-util не принимает чужие соединения);
//! известная плата — лишний TCP-хендшейк на CONNECT (Phase 4+ может
//! прогревать пул пробным соединением).
//!
//! Ветка Passthrough: байты туннеля не трогаем — upgraded IO ↔ TcpStream
//! через `copy_bidirectional`. В трафик туннель попадает обменом CONNECT
//! (см. [`crate::tunnel_record`]), как и отказы/неудачные дозвоны.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use std::time::Instant;

use application::services::{ConnectDecision, EventPublisher};
use domain::entities::rule::SslProxyEntry;
use http::{Request, Response};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use crate::body::{MonitoredBody, StreamTracker};
use crate::bypass::SilentPublisher;
use crate::client::CONNECT_TIMEOUT;
use crate::commands::{ConnGuard, EngineState};
use crate::http_proxy::{ProxyService, lenient_host, text_response};
use crate::tunnel_record::TunnelRecord;

/// Обработка CONNECT-запроса (вызывается из ProxyService).
pub(crate) async fn handle_connect(
    state: Arc<EngineState>,
    guard: Arc<ConnGuard>,
    req: Request<Incoming>,
) -> Response<MonitoredBody> {
    let uri = req.uri().clone();
    let host = uri.host().map(str::to_string);
    let port = uri.port_u16().unwrap_or(443);

    let Some(host_raw) = host else {
        return text_response(400, "crabtrap: CONNECT without host");
    };
    let host = lenient_host(&host_raw);
    tracing::debug!(host = %host, port, "CONNECT");

    match effective_decision(&state, &host, port).await {
        ConnectDecision::Block => {
            tracing::info!(host = %host, port, "CONNECT blocked by pipeline");
            let record =
                TunnelRecord::start(state.events.clone(), guard.conn_id(), host.as_str(), port);
            record.responded(403);
            record.closed(0, 0);
            text_response(403, "crabtrap: connection blocked")
        }
        ConnectDecision::Mitm => mitm(state, guard, req, &host, port).await,
        ConnectDecision::Passthrough => passthrough(state, guard, req, &host_raw, port, true).await,
        ConnectDecision::Bypass => passthrough(state, guard, req, &host_raw, port, false).await,
    }
}

/// Итоговое решение CONNECT: `Block` пайплайна важнее всего; дальше —
/// список SSL-Proxying: пустой → Passthrough (Charles: выключено по
/// умолчанию), совпадение хоста → Mitm, иначе Passthrough.
async fn effective_decision(
    state: &EngineState,
    host: &domain::value_objects::Host,
    port: u16,
) -> ConnectDecision {
    match state.pipeline.on_connect(host, port).await {
        decision @ (ConnectDecision::Block | ConnectDecision::Bypass) => return decision,
        ConnectDecision::Mitm | ConnectDecision::Passthrough => {}
    }
    let entries = state.ssl_entries.read().await;
    if ssl_mitm_enabled(&entries, host.as_str()) {
        ConnectDecision::Mitm
    } else {
        ConnectDecision::Passthrough
    }
}

/// Матчер ssl-списка: wildcard `*` (все), `*.example.com` (суффикс +
/// голый домен — семантика домена), exact; exclude-записи (домены с
/// пиннингом Apple и т.п.) перекрывают include. Чистая функция для тестов.
pub(crate) fn ssl_mitm_enabled(entries: &[SslProxyEntry], host: &str) -> bool {
    domain::entities::ssl_proxy::should_decrypt(entries, host)
}

/// Dial-first с учётом DNS-override'ов (общих с исходящим коннектором).
async fn dial_target(state: &EngineState, host: &str, port: u16) -> Result<TcpStream, DialError> {
    let override_ip = state
        .dns_overrides
        .read()
        .await
        .iter()
        .find(|o| o.host.as_str() == host)
        .map(|o| o.ip);
    let connect_fut = async {
        match override_ip {
            Some(ip) => TcpStream::connect(std::net::SocketAddr::new(ip, port)).await,
            None => TcpStream::connect((host, port)).await,
        }
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, connect_fut).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(e)) => Err(DialError::Failed(e)),
        Err(_) => Err(DialError::Timeout),
    }
}

enum DialError {
    Failed(std::io::Error),
    Timeout,
}

/// Ответ клиенту на неудачный дозвон + запись CONNECT-обмена с причиной.
fn dial_error_response(
    record: TunnelRecord,
    host: &str,
    port: u16,
    e: DialError,
) -> Response<MonitoredBody> {
    match e {
        DialError::Failed(err) => {
            tracing::warn!(host, port, error = %err, "CONNECT dial failed");
            record.failed(format!("cannot connect to {host}:{port}: {err}"));
            text_response(502, format!("crabtrap: cannot connect to {host}:{port}"))
        }
        DialError::Timeout => {
            tracing::warn!(host, port, "CONNECT dial timed out");
            record.failed(format!("connect to {host}:{port} timed out"));
            text_response(504, format!("crabtrap: connect to {host}:{port} timed out"))
        }
    }
}

/// MITM-ветка: probe dial → лист → 200 → TLS-терминация → http1 на
/// расшифрованном стриме (задача в tunnels, переживает CONNECT-соединение).
async fn mitm(
    state: Arc<EngineState>,
    guard: Arc<ConnGuard>,
    req: Request<Incoming>,
    host: &domain::value_objects::Host,
    port: u16,
) -> Response<MonitoredBody> {
    // Dial-first: недоступный origin → честный 502/504 ДО ответа 200.
    let probe = match dial_target(&state, host.as_str(), port).await {
        Ok(stream) => stream,
        Err(e) => {
            // Расшифрованные запросы видны сами; CONNECT пишем только при отказе.
            let record =
                TunnelRecord::start(state.events.clone(), guard.conn_id(), host.as_str(), port);
            return dial_error_response(record, host.as_str(), port, e);
        }
    };
    // Probe закрывается сразу: реальный трафик — через пул UpstreamClient
    // (см. комментарий модуля).
    drop(probe);

    // Лист выпускается ДО хендшейка: resolve() синхронен.
    if let Err(e) = state.certs.prepare(host.as_str()).await {
        tracing::error!(host = %host, error = %e, "MITM leaf issuance failed");
        TunnelRecord::start(state.events.clone(), guard.conn_id(), host.as_str(), port)
            .failed(format!("cannot issue certificate for {host}: {e}"));
        return text_response(500, "crabtrap: cannot issue certificate");
    }

    let mitm_host = host.as_str().to_string();
    let conn_id = guard.conn_id();
    let tunnel_state = state.clone();
    let tunnel_guard = guard.clone();
    let certs = Arc::clone(&state.certs);
    state.spawn_tunnel(async move {
        let upgraded = match hyper::upgrade::on(req).await {
            Ok(u) => u,
            Err(e) => {
                tracing::debug!(host = %mitm_host, error = %e, "MITM upgrade failed");
                tunnel_guard.close();
                return;
            }
        };

        let handshake_started = Instant::now();
        let acceptor = tokio_rustls::TlsAcceptor::from(certs.server_config(&mitm_host));
        let tls_stream = match acceptor.accept(TokioIo::new(upgraded)).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(host = %mitm_host, error = %e, "MITM TLS handshake failed");
                tunnel_guard.close();
                return;
            }
        };
        let tls_ms = handshake_started.elapsed().as_secs_f64() * 1000.0;
        // Параметры установленной сессии (Phase 4): версия/cipher/ALPN едут
        // в MitmConnInfo → событие ExchangeStarted.tls.
        let tls_info = {
            let conn = tls_stream.get_ref().1;
            domain::value_objects::tls::TlsInfo {
                sni: Some(mitm_host.clone()),
                alpn: conn
                    .alpn_protocol()
                    .map(|p| String::from_utf8_lossy(p).into_owned()),
                // Debug-имена rustls: ProtocolVersion "TLSv1_3" → "TLSv1.3".
                version: conn
                    .protocol_version()
                    .map(|v| format!("{v:?}").replace('_', ".")),
                cipher_suite: conn
                    .negotiated_cipher_suite()
                    .map(|c| format!("{:?}", c.suite())),
                mitm: true,
                leaf_not_after: None,
                peer_subject: None,
            }
        };
        tracing::debug!(host = %mitm_host, alpn = ?tls_info.alpn, tls_ms, "MITM TLS established");

        let svc = ProxyService::new(tunnel_state.clone(), conn_id, tunnel_guard.clone()).for_mitm(
            mitm_host.clone(),
            port,
            tls_ms,
            tls_info,
        );
        // HTTP/1.1 или h2 — по ALPN/преамбуле (Phase 11).
        let mut builder = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
        builder.http1().preserve_header_case(true);
        let res = builder
            .serve_connection_with_upgrades(TokioIo::new(tls_stream), svc)
            .await;
        if let Err(e) = res {
            tracing::debug!(host = %mitm_host, error = ?e, "MITM connection ended with error");
        }
        // Не close(): WebSocket из этого соединения (wss) держит гард, пока
        // сокет открыт; последняя ссылка закроет соединение сама.
        drop(tunnel_guard);
    })
    .await;
    // После spawn и без await дальше: отмена future не оставит
    // соединение помеченным как туннель без туннеля.
    guard.mark_upgraded();

    Response::builder()
        .status(200)
        .body(MonitoredBody::from_body(
            http_body_util::Full::<bytes::Bytes>::default(),
            StreamTracker::new(|_, _, _| {}),
        ))
        .expect("static CONNECT response builds")
}

/// Прозрачный туннель: dial-first, затем 200 + копирование байт.
async fn passthrough(
    state: Arc<EngineState>,
    guard: Arc<ConnGuard>,
    req: Request<Incoming>,
    host: &str,
    port: u16,
    recorded: bool,
) -> Response<MonitoredBody> {
    // Мимо записи (Allow List) — туннель тот же, строки в трафике нет.
    let events: Arc<dyn EventPublisher> = match recorded {
        true => state.events.clone(),
        false => Arc::new(SilentPublisher),
    };
    let record = TunnelRecord::start(events, guard.conn_id(), host, port);
    // ВАЖНО: дозвон ДО ответа — см. комментарий модуля.
    let target = match dial_target(&state, host, port).await {
        Ok(stream) => stream,
        Err(e) => return dial_error_response(record, host, port, e),
    };
    record.responded(200);
    let _ = target.set_nodelay(true);

    // hyper отдаст upgraded IO только после записи ответа клиенту.
    let tunnel_state = state.clone();
    let tunnel_guard = guard.clone();
    let host_log = host.to_string();
    state
        .spawn_tunnel(async move {
            let upgraded = match hyper::upgrade::on(req).await {
                Ok(u) => u,
                Err(e) => {
                    tracing::debug!(host = %host_log, error = %e, "tunnel upgrade failed");
                    record.failed(format!("tunnel upgrade failed: {e}"));
                    tunnel_guard.close();
                    return;
                }
            };

            // Счёт байт туннеля: уходит в запись CONNECT-обмена (up/down).
            let client_to_target = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let target_to_client = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let mut client = CountingIo::new(
                TokioIo::new(upgraded),
                client_to_target.clone(),
                target_to_client.clone(),
            );
            // Считаем только клиентский конец: каждый байт проходит его ровно
            // один раз (обёртка и upstream удвоила бы счёт).
            let mut upstream = target;

            let copied = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            match copied {
                Ok((up, down)) => tracing::debug!(
                    host = %host_log,
                    up,
                    down,
                    "tunnel closed"
                ),
                Err(e) => tracing::debug!(host = %host_log, error = %e, "tunnel ended with error"),
            }
            // Счётчики, а не результат copy: при ошибке он теряет переданное.
            record.closed(
                client_to_target.load(Ordering::Relaxed),
                target_to_client.load(Ordering::Relaxed),
            );
            // copy_bidirectional сам делает half-close обоих концов.
            // Туннельная задача живёт дольше http1-соединения, из которого выросла:
            // закрываем соединение здесь (идемпотентно с завершением conn-задачи).
            tunnel_guard.close();
            let _ = tunnel_state;
        })
        .await;
    // После spawn и без await дальше: отмена future не оставит
    // соединение помеченным как туннель без туннеля.
    guard.mark_upgraded();

    Response::builder()
        .status(200)
        .body(MonitoredBody::from_body(
            http_body_util::Full::<bytes::Bytes>::default(),
            StreamTracker::new(|_, _, _| {}),
        ))
        .expect("static CONNECT response builds")
}

/// IO-обёртка со счётчиком байт в обе стороны.
struct CountingIo<T> {
    inner: T,
    /// Байты, прочитанные из этого конца (пойдут «вниз» по туннелю).
    rx: Arc<std::sync::atomic::AtomicU64>,
    /// Байты, записанные в этот конец.
    tx: Arc<std::sync::atomic::AtomicU64>,
}

impl<T> CountingIo<T> {
    fn new(
        inner: T,
        rx: Arc<std::sync::atomic::AtomicU64>,
        tx: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self { inner, rx, tx }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for CountingIo<T> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let filled_before = buf.filled().len();
        match std::pin::Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let read = buf.filled().len() - filled_before;
                self.rx.fetch_add(read as u64, Ordering::Relaxed);
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for CountingIo<T> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match std::pin::Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => {
                self.tx.fetch_add(n as u64, Ordering::Relaxed);
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::value_objects::RuleId;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn entry(pattern: &str, enabled: bool) -> SslProxyEntry {
        SslProxyEntry {
            id: RuleId::new(),
            host_pattern: pattern.to_string(),
            enabled,
            exclude: false,
        }
    }

    #[test]
    fn ssl_matcher_exclusion_tunnels_pinned_hosts() {
        let entries = vec![entry("*", true), SslProxyEntry::exclude("*.apple.com")];
        assert!(!ssl_mitm_enabled(&entries, "www.apple.com"));
        assert!(ssl_mitm_enabled(&entries, "example.com"));
    }

    #[test]
    fn ssl_matcher_empty_list_is_passthrough() {
        assert!(!ssl_mitm_enabled(&[], "example.com"));
    }

    #[test]
    fn ssl_matcher_wildcard_all() {
        assert!(ssl_mitm_enabled(&[entry("*", true)], "anything.io"));
        assert!(ssl_mitm_enabled(
            &[entry("*", true)],
            "deep.a.b.example.com"
        ));
    }

    #[test]
    fn ssl_matcher_suffix_covers_bare_domain() {
        let entries = vec![entry("*.example.com", true)];
        assert!(ssl_mitm_enabled(&entries, "example.com"));
        assert!(ssl_mitm_enabled(&entries, "api.example.com"));
        assert!(!ssl_mitm_enabled(&entries, "example.com.evil.org"));
        assert!(!ssl_mitm_enabled(&entries, "other.org"));
    }

    #[test]
    fn ssl_matcher_exact_and_disabled() {
        assert!(ssl_mitm_enabled(
            &[entry("api.example.com", true)],
            "api.example.com"
        ));
        assert!(!ssl_mitm_enabled(
            &[entry("api.example.com", true)],
            "v2.api.example.com"
        ));
        assert!(!ssl_mitm_enabled(&[entry("*", false)], "example.com"));
        // Несовпадение в непустом списке → Passthrough.
        assert!(!ssl_mitm_enabled(&[entry("a.com", true)], "b.com"));
    }

    #[tokio::test]
    async fn counting_io_counts_both_directions() {
        // a — конец под счётчиком; b — второй конец duplex.
        let (a, mut b) = tokio::io::duplex(64);
        let rx = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let tx = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut counted = CountingIo::new(a, rx.clone(), tx.clone());

        // write a → b: учитывается в tx.
        counted.write_all(b"hello").await.expect("write");
        assert_eq!(tx.load(Ordering::Relaxed), 5);

        // echo b → a: читаем со второго конца и пишем назад, rx растёт.
        let mut buf = [0u8; 8];
        let n = b.read(&mut buf).await.expect("read from b");
        assert_eq!(&buf[..n], b"hello");
        b.write_all(b"echo!").await.expect("write back");
        let n = counted.read(&mut buf).await.expect("read from a");
        assert_eq!(&buf[..n], b"echo!");
        assert_eq!(rx.load(Ordering::Relaxed), 5);
    }
}
