//! Исходящий HTTP(S)-клиент прокси (absolute-form запросы к origin).
//!
//! `hyper_util` legacy-клиент с пулом соединений; TLS — tokio-rustls на
//! системных корнях (rustls-native-certs) ПЛЮС инъектируемые extra-roots
//! (тесты локальных origin; будущие внешние прокси), ALPN только
//! `http/1.1` (h2 — Phase 11). CONNECT-туннели этот клиент НЕ используют
//! (connect.rs).
//!
//! Метрики диалапа (connect/tls) доставляются в расширения ответа через
//! `Connected::extra` (`ConnectMetrics`) — http_proxy.rs кладёт их в
//! TimingPhases. Ограничение: для соединений из пула метрики отражают
//! диалап ЭТОГО соединения (исторический), а не текущего обмена.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use hyper::rt;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tower_service::Service;

use crate::body::MonitoredBody;
use crate::error::EngineError;
use domain::entities::network::DnsOverride;

/// Таймаут на установку исходящего соединения (только connect; на тело
/// таймаута нет — стриминг больших загрузок).
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Пул: покоящиеся соединения закрываем через 90 сек, максимум 4 на хост.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const POOL_MAX_IDLE_PER_HOST: usize = 4;

/// Транспорт исходящего соединения: либо TCP, либо TLS поверх TCP.
pub(crate) enum MaybeTlsStream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl tokio::io::AsyncRead for MaybeTlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for MaybeTlsStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// Метрики установки исходящего соединения (мс; tls_ms = 0 для plain TCP).
/// Попадает в расширения ответа через `Connected::extra`.
#[derive(Clone, Debug)]
pub(crate) struct ConnectMetrics {
    pub connect_ms: f64,
    pub tls_ms: f64,
}

/// `http(s)://authority` → MeteredStream (tokio-rustls, native + extra roots).
#[derive(Clone)]
pub(crate) struct HttpsConnector {
    http: HttpConnector,
    tls: tokio_rustls::TlsConnector,
    /// DNS-override'ы (общие с EngineState; семена Phase 10).
    dns: Arc<RwLock<Vec<DnsOverride>>>,
}

impl HttpsConnector {
    /// `extra_roots` — DER-сертификаты доверия сверх системных
    /// (интеграционные тесты, будущие upstream-прокси).
    pub(crate) fn new(
        extra_roots: &[Vec<u8>],
        dns: Arc<RwLock<Vec<DnsOverride>>>,
        allow_h2: bool,
    ) -> Result<Self, EngineError> {
        let mut roots = rustls::RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for error in &native.errors {
            tracing::warn!(%error, "failed to load some native certificates");
        }
        for cert in native.certs {
            if let Err(e) = roots.add(cert) {
                tracing::warn!(error = %e, "skipping unsupported native certificate");
            }
        }
        for der in extra_roots {
            let cert = rustls::pki_types::CertificateDer::from(der.clone());
            if let Err(e) = roots.add(cert) {
                tracing::warn!(error = %e, "skipping unsupported extra root certificate");
            }
        }
        if roots.is_empty() {
            return Err(EngineError::NoRootCertificates);
        }

        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        // h2 (Phase 11): legacy-клиент ведёт h2-соединение, если origin
        // выбрал его в ALPN (`Connected::negotiated_h2`).
        config.alpn_protocols = if allow_h2 {
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        } else {
            vec![b"http/1.1".to_vec()]
        };

        let mut http = HttpConnector::new();
        http.set_connect_timeout(Some(CONNECT_TIMEOUT));

        Ok(Self {
            http,
            tls: tokio_rustls::TlsConnector::from(Arc::new(config)),
            dns,
        })
    }
}

impl Service<http::Uri> for HttpsConnector {
    type Response = TokioIo<MeteredStream>;
    type Error = EngineError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: http::Uri) -> Self::Future {
        let mut http = self.http.clone();
        let tls = self.tls.clone();
        let dns = Arc::clone(&self.dns);
        Box::pin(async move {
            let started = Instant::now();
            let scheme = dst.scheme_str().unwrap_or("http");
            if scheme != "http" && scheme != "https" {
                return Err(EngineError::InvalidUri(format!(
                    "unsupported scheme: {scheme:?}"
                )));
            }
            let default_port: u16 = if scheme == "https" { 443 } else { 80 };
            let host = dst
                .host()
                .ok_or_else(|| EngineError::InvalidUri("uri without host".into()))?
                .to_string();
            let port = dst.port_u16().unwrap_or(default_port);

            // DNS-override (семена Phase 10): адрес подменяется, SNI/ServerName
            // остаются оригинальными.
            let override_ip = dns
                .read()
                .await
                .iter()
                .find(|o| o.host.as_str() == host)
                .map(|o| o.ip);

            let tcp = match override_ip {
                Some(ip) => {
                    let stream =
                        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((ip, port)))
                            .await
                            .map_err(|_| {
                                EngineError::Io(std::io::Error::new(
                                    std::io::ErrorKind::TimedOut,
                                    "connect (dns override) timed out",
                                ))
                            })?
                            .map_err(EngineError::Io)?;
                    TokioIo::new(stream).into_inner()
                }
                None => {
                    // Внутренний HttpConnector (tower Service<Uri>): DNS
                    // (happy-eyeballs) и connect-timeout.
                    let plain_uri: http::Uri = format!("http://{host}:{port}")
                        .parse()
                        .map_err(|e| EngineError::InvalidUri(format!("connector uri: {e}")))?;
                    let tcp: TokioIo<TcpStream> = http.call(plain_uri).await.map_err(|e| {
                        // Причина (refused/DNS) — в source(): тащим цепочку в текст.
                        EngineError::Io(std::io::Error::other(format!(
                            "connect: {}",
                            crate::error::error_chain(&e)
                        )))
                    })?;
                    tcp.into_inner()
                }
            };
            let connect_ms = started.elapsed().as_secs_f64() * 1000.0;

            if scheme == "https" {
                let name = rustls::pki_types::ServerName::try_from(host.clone())
                    .map_err(|e| EngineError::Tls(format!("invalid server name {host}: {e}")))?;
                let stream = tls
                    .connect(name, tcp)
                    .await
                    .map_err(|e| EngineError::Tls(format!("tls handshake: {e}")))?;
                let tls_ms = started.elapsed().as_secs_f64() * 1000.0 - connect_ms;
                let h2 = stream.get_ref().1.alpn_protocol() == Some(b"h2".as_slice());
                Ok(TokioIo::new(MeteredStream {
                    inner: MaybeTlsStream::Tls(Box::new(stream)),
                    metrics: ConnectMetrics { connect_ms, tls_ms },
                    h2,
                }))
            } else {
                Ok(TokioIo::new(MeteredStream {
                    inner: MaybeTlsStream::Plain(tcp),
                    metrics: ConnectMetrics {
                        connect_ms,
                        tls_ms: 0.0,
                    },
                    h2: false,
                }))
            }
        })
    }
}

/// Стрим с метриками диалапа: `Connected::extra` доставит их в extensions
/// ответа (см. модульный комментарий про пул).
pub(crate) struct MeteredStream {
    inner: MaybeTlsStream,
    metrics: ConnectMetrics,
    /// Origin выбрал h2 в ALPN.
    h2: bool,
}

impl tokio::io::AsyncRead for MeteredStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for MeteredStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

// hyper-util помечает метаданные соединения (пул/absolute-form) через
// `Connection`; прямые соединения crabtrap — не «за прокси». Сюда же
// кладём метрики диалапа — legacy-клиент скопирует их в extensions ответа.
impl hyper_util::client::legacy::connect::Connection for MeteredStream {
    fn connected(&self) -> hyper_util::client::legacy::connect::Connected {
        let connected =
            hyper_util::client::legacy::connect::Connected::new().extra(self.metrics.clone());
        if self.h2 {
            connected.negotiated_h2()
        } else {
            connected
        }
    }
}

/// Клиент исходящих запросов прокси.
#[derive(Clone)]
pub(crate) struct UpstreamClient {
    inner: Client<HttpsConnector, MonitoredBody>,
}

impl UpstreamClient {
    /// Обычный клиент: h2 или HTTP/1.1 по выбору origin.
    pub(crate) fn new(
        extra_roots: &[Vec<u8>],
        dns: Arc<RwLock<Vec<DnsOverride>>>,
    ) -> Result<Self, EngineError> {
        Self::build(extra_roots, dns, true)
    }

    /// Только HTTP/1.1 — для апгрейдов (WebSocket): `Upgrade` в h2 нет.
    pub(crate) fn http1_only(
        extra_roots: &[Vec<u8>],
        dns: Arc<RwLock<Vec<DnsOverride>>>,
    ) -> Result<Self, EngineError> {
        Self::build(extra_roots, dns, false)
    }

    fn build(
        extra_roots: &[Vec<u8>],
        dns: Arc<RwLock<Vec<DnsOverride>>>,
        allow_h2: bool,
    ) -> Result<Self, EngineError> {
        let connector = HttpsConnector::new(extra_roots, dns, allow_h2)?;
        let inner = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
            .build(connector);
        Ok(Self { inner })
    }

    pub(crate) fn request(
        &self,
        req: http::Request<MonitoredBody>,
    ) -> hyper_util::client::legacy::ResponseFuture {
        self.inner.request(req)
    }
}

// Симметрия: ответ upstream-клиента — hyper::body::Incoming; мониторы
// навешиваются в http_proxy.rs на обе стороны.
#[allow(dead_code)]
fn _assert_io_traits() {
    fn assert_read_write<T: rt::Read + rt::Write>() {}
    assert_read_write::<TokioIo<MeteredStream>>();
}
