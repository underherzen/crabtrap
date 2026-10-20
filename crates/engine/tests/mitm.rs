//! Интеграционные тесты MITM (Phase 3): локальный TLS-origin `origin.local`,
//! реальный RcgenCertificateService в tempdir, CONNECT → расшифровка.

use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::sync::Arc;
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::services::{CertificateService as _, EventPublisher, InterceptionPipeline};
use async_trait::async_trait;
use domain::entities::network::DnsOverride;
use domain::value_objects::Host;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use engine::{EngineConfig, EngineDeps};
use infrastructure::services::RcgenCertificateService;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

// ==== тестовые двойники ====

struct RecordingPublisher {
    events: std::sync::Mutex<Vec<ProxyEvent>>,
}

impl RecordingPublisher {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn snapshot(&self) -> Vec<ProxyEvent> {
        self.events.lock().expect("events lock").clone()
    }

    async fn wait_listening(self: &Arc<Self>) -> String {
        let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            let found = self
                .events
                .lock()
                .expect("events lock")
                .iter()
                .find_map(|e| match e {
                    ProxyEvent::EngineStatus {
                        listening: Some(addr),
                        ..
                    } => Some(addr.clone()),
                    _ => None,
                });
            if let Some(addr) = found {
                return addr;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("engine did not report listening within timeout");
    }
}

impl EventPublisher for RecordingPublisher {
    fn publish(&self, event: ProxyEvent) {
        self.events.lock().expect("events lock").push(event);
    }
}

struct TestPipeline;

#[async_trait]
impl InterceptionPipeline for TestPipeline {
    async fn on_connect(&self, _host: &Host, _port: u16) -> application::services::ConnectDecision {
        application::services::ConnectDecision::Passthrough
    }
    async fn on_request(
        &self,
        context: application::services::InterceptContext,
    ) -> application::services::InterceptOutcome {
        application::services::InterceptOutcome::Proceed {
            message: context.message,
            route: application::services::RouteDecision::Origin,
        }
    }
    async fn on_response(
        &self,
        context: application::services::InterceptContext,
    ) -> application::services::InterceptOutcome {
        application::services::InterceptOutcome::Proceed {
            message: context.message,
            route: application::services::RouteDecision::Origin,
        }
    }
}

// ==== проводные помощники ====

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Голова ответа до \r\n\r\n (для CONNECT достаточно; байты после головы
/// могут быть отброшены — для ответов с телом используйте read_response).
async fn read_head_only<T: AsyncRead + Unpin>(stream: &mut T) -> String {
    let (_, head, _) = read_response_inner(stream, false).await;
    head
}

/// Полный HTTP-ответ (голова + тело по Content-Length).
async fn read_response<T: AsyncRead + Unpin>(stream: &mut T) -> (u16, String, Vec<u8>) {
    read_response_inner(stream, true).await
}

async fn read_response_inner<T: AsyncRead + Unpin>(
    stream: &mut T,
    with_body: bool,
) -> (u16, String, Vec<u8>) {
    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    // Голова; «хвост» того же сегмента сохраняем для тела.
    let head_end = loop {
        if let Some(idx) = find_head_end(&buf) {
            break idx;
        }
        let n = tokio::time::timeout_at(deadline, stream.read(&mut chunk))
            .await
            .expect("head read timeout")
            .expect("head read");
        if n == 0 {
            panic!("EOF before response head");
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .expect("status code");
    if !with_body {
        return (status, head, Vec::new());
    }
    let content_length = head
        .lines()
        .find_map(|l| {
            let (n, v) = l.split_once(':')?;
            if n.eq_ignore_ascii_case("content-length") {
                v.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body: Vec<u8> = buf.split_off(head_end + 4);
    while body.len() < content_length {
        let n = tokio::time::timeout_at(deadline, stream.read(&mut chunk))
            .await
            .expect("body read timeout")
            .expect("body read");
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    (status, head, body)
}

use tokio::io::AsyncRead;

/// Самоподписанный TLS-origin `origin.local` на ephemeral-порту.
/// Возвращает (адрес, DER-сертификат origin для extra-roots, PEM для
/// клиентских root-store'ов теста).
async fn spawn_tls_origin() -> (
    std::net::SocketAddr,
    Vec<u8>,
    String,
    tokio::sync::oneshot::Receiver<Vec<u8>>, /* увиденный запрос */
) {
    let key = rcgen::KeyPair::generate().expect("origin key");
    let mut params = rcgen::CertificateParams::default();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "origin.local");
    params.subject_alt_names = vec![rcgen::SanType::DnsName(
        "origin.local".try_into().expect("san"),
    )];
    let cert = params.self_signed(&key).expect("origin cert");
    let cert_pem = cert.pem();
    let cert_der = cert.der().as_ref().to_vec();

    let mut server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(cert_der.clone())],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                key.serialize_der(),
            )),
        )
        .expect("origin server config");
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let addr = listener.local_addr().expect("origin addr");
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    // Arc-Mutex: dial-probe движка (голый TCP без TLS) тоже доходит сюда —
    // sender обязан пережить неудачный TLS-accept и достаться реальному
    // соединению.
    let seen_tx = Arc::new(std::sync::Mutex::new(Some(seen_tx)));
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            let seen_tx = Arc::clone(&seen_tx);
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(sock).await else {
                    return;
                };
                // Один запрос-один ответ: читаем голову, отвечаем, закрываем.
                let (mut rx, mut tx) = tokio::io::split(tls);
                let mut buf = Vec::new();
                let mut chunk = [0u8; 2048];
                loop {
                    let n = match tokio::time::timeout(TEST_TIMEOUT, rx.read(&mut chunk)).await {
                        Ok(Ok(n)) if n > 0 => n,
                        _ => break,
                    };
                    buf.extend_from_slice(&chunk[..n]);
                    if find_head_end(&buf).is_some() {
                        break;
                    }
                }
                let taken = seen_tx.lock().expect("seen lock").take();
                if let Some(tx) = taken {
                    let _ = tx.send(buf.clone());
                }
                let body = b"hello from tls origin";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let mut raw = resp.into_bytes();
                raw.extend_from_slice(body);
                let _ = tx.write_all(&raw).await;
                let _ = tx.shutdown().await;
            });
        }
    });
    (addr, cert_der, cert_pem, seen_rx)
}

/// Клиентский rustls-config с доверенным PEM-корнем + ALPN http/1.1.
fn client_tls_config(root_pem: &str) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(root_pem.as_bytes()) {
        roots.add(cert.expect("root pem")).expect("add root");
    }
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

/// DER из PEM (первый сертификат).
fn pem_der(pem: &str) -> Vec<u8> {
    CertificateDer::pem_slice_iter(pem.as_bytes())
        .next()
        .expect("cert in pem")
        .expect("pem parse")
        .as_ref()
        .to_vec()
}

// ==== тесты ====

/// Движок с реальным cert-сервисом в tempdir + DNS override origin.local.
async fn spawn_mitm_engine(
    service: Arc<RcgenCertificateService>,
    origin_der: Vec<u8>,
    ssl_patterns: &[&str],
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<application::dto::proxy_config::EngineCommand>,
) {
    let publisher = RecordingPublisher::new();
    let (cmd_tx, cmd_rx) = async_channel::bounded(16);
    // Тела — реальный store в tempdir (tee-захват проверяется интеграционно).
    let bodies = Arc::new(infrastructure::repositories::MemorySpillBodyStore::new(
        1024,
        u64::MAX,
        tempfile::tempdir().expect("body cache tempdir").keep(),
    ));
    let deps = EngineDeps {
        events: publisher.clone(),
        pipeline: Arc::new(TestPipeline),
        certificate_service: service,
        bodies,
        direct_requests: None,
    };
    let cfg = EngineConfig::new("127.0.0.1:0", 16)
        .with_extra_root_certs(vec![origin_der])
        .with_dns_overrides(vec![DnsOverride::new(
            "origin.local",
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        )]);
    tokio::spawn(engine::run(cfg, deps, cmd_rx));
    let addr = publisher.wait_listening().await;
    if !ssl_patterns.is_empty() {
        let entries: Vec<domain::entities::rule::SslProxyEntry> = ssl_patterns
            .iter()
            .map(|p| domain::entities::rule::SslProxyEntry::include(*p))
            .collect();
        cmd_tx
            .send(application::dto::proxy_config::EngineCommand::UpdateSslProxying { entries })
            .await
            .expect("send ssl entries");
    }
    (addr, publisher, cmd_tx)
}

#[tokio::test(flavor = "multi_thread")]
async fn mitm_decrypts_connect_and_serves_leaf_for_origin_local() {
    init_tracing();
    let certs_dir = tempfile::tempdir().expect("certs tempdir");
    let service = Arc::new(RcgenCertificateService::new(certs_dir.path().to_path_buf()));
    let (origin_addr, origin_der, _origin_pem, seen_rx) = spawn_tls_origin().await;

    let (engine_addr, events, cmd_tx) =
        spawn_mitm_engine(service.clone(), origin_der, &["*"]).await;

    // CONNECT origin.local:443 (порт любого значения не имеет — override).
    let mut client = TcpStream::connect(&engine_addr)
        .await
        .expect("connect engine");
    client
        .write_all(
            format!(
                "CONNECT origin.local:{} HTTP/1.1\r\nHost: origin.local\r\n\r\n",
                origin_addr.port()
            )
            .as_bytes(),
        )
        .await
        .expect("write CONNECT");
    let head = read_head_only(&mut client).await;
    assert!(
        head.starts_with("HTTP/1.1 200"),
        "CONNECT must succeed: {head}"
    );

    // TLS от имени клиента: корни = НАШ CA (движок), SNI origin.local.
    let ca_status = service.ensure_ca().await.expect("CA status");
    let ca_pem = tokio::fs::read_to_string(&ca_status.cert_path)
        .await
        .expect("read engine ca.pem");
    let tls_config = client_tls_config(&ca_pem);
    let connector = tokio_rustls::TlsConnector::from(tls_config);
    let name =
        rustls::pki_types::ServerName::try_from("origin.local".to_string()).expect("server name");
    let mut tls = connector
        .connect(name, client)
        .await
        .expect("client TLS against engine-issued leaf");

    // GET по расшифрованному каналу → ответ origin (round-trip).
    tls.write_all(
        b"GET /secure/path?q=1 HTTP/1.1\r\nHost: origin.local\r\nConnection: close\r\n\r\n",
    )
    .await
    .expect("write GET");
    let (status, resp_head, body) = read_response(&mut tls).await;
    assert_eq!(status, 200, "{resp_head}");
    assert_eq!(body, b"hello from tls origin");

    // Origin действительно получил запрос (через второй TLS-хоп движка).
    let seen = tokio::time::timeout(TEST_TIMEOUT, seen_rx)
        .await
        .expect("origin served")
        .expect("origin request");
    let seen = String::from_utf8_lossy(&seen);
    assert!(
        seen.starts_with("GET /secure/path?q=1 HTTP/1.1\r\n"),
        "origin must see decrypted request, got: {seen}"
    );

    // События: расшифрованный exchange с tls_mitm=Some(true).
    let snapshot = events.snapshot();
    let (started, tls_info) = snapshot
        .iter()
        .find_map(|e| match e {
            ProxyEvent::ExchangeStarted { exchange, tls, .. } if exchange.scheme == "https" => {
                Some((exchange.clone(), tls.clone()))
            }
            _ => None,
        })
        .expect("decrypted ExchangeStarted");
    assert_eq!(started.host, "origin.local");
    assert_eq!(started.path, "/secure/path?q=1");
    assert_eq!(started.tls_mitm, Some(true), "exchange must be marked MITM");
    // Phase 4: параметры TLS-сессии клиент↔crabtrap едут событием.
    let tls = tls_info.expect("ExchangeStarted carries TlsInfoDto for MITM");
    assert!(tls.mitm);
    assert_eq!(tls.sni.as_deref(), Some("origin.local"));
    assert!(
        tls.version
            .as_deref()
            .unwrap_or_default()
            .starts_with("TLSv1"),
        "negotiated version reported, got {:?}",
        tls.version
    );
    assert!(
        tls.cipher_suite.is_some(),
        "negotiated cipher suite reported"
    );
    assert!(
        snapshot
            .iter()
            .any(|e| matches!(e, ProxyEvent::ResponseHeaders { status: 200, .. })),
        "ResponseHeaders for decrypted exchange"
    );
    let completed = snapshot
        .iter()
        .find_map(|e| match e {
            ProxyEvent::ExchangeCompleted { timing, .. } => Some(*timing),
            _ => None,
        })
        .expect("ExchangeCompleted");
    assert!(
        completed.tls_ms > 0.0,
        "MITM handshake must fill tls_ms, got {:?}",
        completed
    );
    assert!(completed.connect_ms >= 0.0);

    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::Stop)
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn non_matching_ssl_list_leaves_true_tunnel_with_origin_cert() {
    init_tracing();
    let certs_dir = tempfile::tempdir().expect("certs tempdir");
    let service = Arc::new(RcgenCertificateService::new(certs_dir.path().to_path_buf()));
    let (origin_addr, _origin_der, origin_pem, _seen_rx) = spawn_tls_origin().await;

    // Список НЕ покрывает origin.local → туннель.
    let (engine_addr, events, cmd_tx) =
        spawn_mitm_engine(service.clone(), Vec::new(), &["*.unrelated.org"]).await;

    let mut client = TcpStream::connect(&engine_addr)
        .await
        .expect("connect engine");
    client
        .write_all(
            format!(
                "CONNECT origin.local:{} HTTP/1.1\r\nHost: origin.local\r\n\r\n",
                origin_addr.port()
            )
            .as_bytes(),
        )
        .await
        .expect("write CONNECT");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 200"), "CONNECT tunnel: {head}");

    // Клиент доверяет ТОЛЬКО origin-сертификату: туннель отдаёт его as-is.
    let tls_config = client_tls_config(&origin_pem);
    let connector = tokio_rustls::TlsConnector::from(tls_config);
    let name = rustls::pki_types::ServerName::try_from("origin.local".to_string()).unwrap();
    let mut tls = connector
        .connect(name, client)
        .await
        .expect("origin cert must be served verbatim in tunnel mode");

    // Дискриминатор: peer-сертификат = сертификат origin (не наш лист).
    let (_, tls_conn) = tls.get_ref();
    let peer_der = tls_conn
        .peer_certificates()
        .expect("peer cert")
        .first()
        .expect("leaf")
        .as_ref()
        .to_vec();
    assert_eq!(
        peer_der,
        pem_der(&origin_pem),
        "tunnel must present origin cert"
    );

    // Круг: запрос тоже работает (через туннель).
    tls.write_all(b"GET /tunnel HTTP/1.1\r\nHost: origin.local\r\nConnection: close\r\n\r\n")
        .await
        .expect("write GET");
    let (status, _h, body) = read_response(&mut tls).await;
    assert_eq!(status, 200);
    assert_eq!(body, b"hello from tls origin");

    // Расшифрованных обменов НЕТ (туннель): единственный обмен — сам
    // CONNECT (запись туннеля, как в Charles), без MITM.
    let snapshot = events.snapshot();
    let started: Vec<_> = snapshot
        .iter()
        .filter_map(|e| match e {
            ProxyEvent::ExchangeStarted { exchange, .. } => Some(exchange.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 1, "only the CONNECT record: {started:?}");
    assert_eq!(started[0].method, "CONNECT");
    assert_eq!(
        started[0].tls_mitm,
        Some(false),
        "tunnel must not be decrypted"
    );

    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::Stop)
        .await
        .ok();
}
