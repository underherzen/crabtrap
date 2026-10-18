//! HTTP/2 сквозь MITM (Phase 11a): клиент и origin выбирают протокол по
//! ALPN независимо — h2↔h2, h2-клиент ↔ h1-origin, h1-клиент ↔ h2-origin;
//! в трафике протокол клиента (HTTP/2) и версия ответа origin.

use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::sync::Arc;
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::proxy_config::EngineCommand;
use application::services::CertificateService as _;
use bytes::Bytes;
use domain::entities::DnsOverride;
use domain::value_objects::Host;
use domain::value_objects::http_vo::{HttpVersionVo, Protocol};
use engine::{EngineConfig, EngineDeps};
use http_body_util::{BodyExt as _, Full};
use hyper_util::rt::{TokioExecutor, TokioIo};
use infrastructure::services::RcgenCertificateService;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

mod common;
use common::*;

/// TLS-origin `origin.local` на hyper (h1/h2 auto) с заданным ALPN;
/// отвечает версией HTTP, на которой пришёл запрос, и полученным Host
/// (`x-origin-host`, «none» — заголовка не было) и TE (`x-origin-te`).
async fn spawn_origin(alpn: &[&[u8]]) -> (std::net::SocketAddr, Vec<u8>) {
    let key = rcgen::KeyPair::generate().expect("key");
    let mut params = rcgen::CertificateParams::default();
    params.subject_alt_names = vec![rcgen::SanType::DnsName(
        "origin.local".try_into().expect("san"),
    )];
    let cert = params.self_signed(&key).expect("cert");
    let der = cert.der().as_ref().to_vec();
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(der.clone())],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )
        .expect("server config");
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(sock).await else {
                    return;
                };
                let service = hyper::service::service_fn(
                    |req: http::Request<hyper::body::Incoming>| async move {
                        let version = format!("{:?}", req.version());
                        let host = req
                            .headers()
                            .get(http::header::HOST)
                            .map_or("none".to_string(), |h| {
                                h.to_str().unwrap_or("?").to_string()
                            });
                        let te = req
                            .headers()
                            .get(http::header::TE)
                            .map_or("none".to_string(), |h| {
                                h.to_str().unwrap_or("?").to_string()
                            });
                        let resp = http::Response::builder()
                            .header("x-origin-version", version.clone())
                            .header("x-origin-host", host)
                            .header("x-origin-te", te)
                            .body(Full::new(Bytes::from(format!("origin saw {version}"))))
                            .expect("response");
                        Ok::<_, std::convert::Infallible>(resp)
                    },
                );
                let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(tls), service)
                    .await;
            });
        }
    });
    (addr, der)
}

async fn spawn_mitm_engine(
    service: Arc<RcgenCertificateService>,
    origin_der: Vec<u8>,
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<EngineCommand>,
) {
    let publisher = RecordingPublisher::new();
    let (cmd_tx, cmd_rx) = async_channel::bounded(16);
    let deps = EngineDeps {
        events: publisher.clone(),
        pipeline: Arc::new(TestPipeline),
        certificate_service: service,
        bodies: body_store(),
        direct_requests: None,
    };
    let cfg = EngineConfig::new("127.0.0.1:0", 16)
        .with_extra_root_certs(vec![origin_der])
        .with_dns_overrides(vec![DnsOverride {
            host: Host::new("origin.local").expect("host"),
            ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        }]);
    tokio::spawn(engine::run(cfg, deps, cmd_rx));
    let addr = publisher.wait_listening().await;
    cmd_tx
        .send(EngineCommand::UpdateSslProxying {
            entries: vec![domain::entities::rule::SslProxyEntry::include("*")],
        })
        .await
        .expect("ssl list");
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, publisher, cmd_tx)
}

/// CONNECT + TLS к движку с ALPN клиента; возвращает TLS-стрим.
async fn mitm_tls(
    engine: &str,
    port: u16,
    service: &RcgenCertificateService,
    alpn: &[&[u8]],
) -> tokio_rustls::client::TlsStream<TcpStream> {
    let mut client = TcpStream::connect(engine).await.expect("connect");
    client
        .write_all(
            format!("CONNECT origin.local:{port} HTTP/1.1\r\nHost: origin.local\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("CONNECT");
    let head = read_head_only(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let ca_path = service.ensure_ca().await.expect("ca").cert_path;
    let ca_pem = tokio::fs::read_to_string(ca_path).await.expect("ca pem");
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        roots.add(cert.expect("pem")).expect("root");
    }
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    let name = rustls::pki_types::ServerName::try_from("origin.local".to_string()).expect("name");
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, client)
        .await
        .expect("tls")
}

async fn h2_get(
    tls: tokio_rustls::client::TlsStream<TcpStream>,
    port: u16,
) -> (http::response::Parts, String) {
    h2_get_with(tls, port, &[]).await
}

async fn h2_get_with(
    tls: tokio_rustls::client::TlsStream<TcpStream>,
    port: u16,
    headers: &[(&str, &str)],
) -> (http::response::Parts, String) {
    assert_eq!(
        tls.get_ref().1.alpn_protocol(),
        Some(b"h2".as_slice()),
        "engine must speak h2"
    );
    let (mut sender, conn) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
            .await
            .expect("h2 handshake");
    tokio::spawn(conn);
    let request = headers
        .iter()
        .fold(http::Request::builder(), |builder, (name, value)| {
            builder.header(*name, *value)
        })
        .uri(format!("https://origin.local:{port}/h2"))
        .body(Full::new(Bytes::new()))
        .expect("request");
    let response = sender.send_request(request).await.expect("h2 response");
    let (parts, body) = response.into_parts();
    let body = body.collect().await.expect("body").to_bytes();
    (parts, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn h2_client_and_h2_origin() {
    let certs = tempfile::tempdir().expect("certs");
    let service = Arc::new(RcgenCertificateService::new(certs.path().to_path_buf()));
    let (origin, der) = spawn_origin(&[b"h2", b"http/1.1"]).await;
    let (engine, events, cmd_tx) = spawn_mitm_engine(service.clone(), der).await;

    let tls = mitm_tls(&engine, origin.port(), &service, &[b"h2", b"http/1.1"]).await;
    let (parts, body) = h2_get(tls, origin.port()).await;
    assert_eq!(parts.status, 200);
    assert_eq!(body, "origin saw HTTP/2.0", "upstream must be h2 too");
    // Хост в h2 — `:authority`; отдельный `host` Google Front End сбрасывает
    // RST_STREAM(PROTOCOL_ERROR).
    assert_eq!(
        parts.headers["x-origin-host"], "none",
        "h2 upstream must not carry host"
    );

    let protocol = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeStarted { exchange, .. } if exchange.path == "/h2" => {
            Some(exchange.protocol)
        }
        _ => None,
    })
    .await;
    assert_eq!(protocol, Protocol::Http2);
    let version = wait_event(&events, |e| match e {
        ProxyEvent::ResponseHeaders { version, .. } => Some(*version),
        _ => None,
    })
    .await;
    assert_eq!(version, HttpVersionVo::H2);
    wait_event(&events, |e| {
        matches!(e, ProxyEvent::ExchangeCompleted { .. }).then_some(())
    })
    .await;
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn h2_client_and_h1_only_origin() {
    let certs = tempfile::tempdir().expect("certs");
    let service = Arc::new(RcgenCertificateService::new(certs.path().to_path_buf()));
    let (origin, der) = spawn_origin(&[b"http/1.1"]).await;
    let (engine, _events, cmd_tx) = spawn_mitm_engine(service.clone(), der).await;

    let tls = mitm_tls(&engine, origin.port(), &service, &[b"h2", b"http/1.1"]).await;
    let (parts, body) = h2_get(tls, origin.port()).await;
    assert_eq!(parts.status, 200);
    assert_eq!(body, "origin saw HTTP/1.1");
    let host = format!("origin.local:{}", origin.port());
    assert_eq!(
        parts.headers["x-origin-host"],
        host.as_str(),
        "HTTP/1.1 needs Host"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn h1_client_and_h2_origin() {
    let certs = tempfile::tempdir().expect("certs");
    let service = Arc::new(RcgenCertificateService::new(certs.path().to_path_buf()));
    let (origin, der) = spawn_origin(&[b"h2", b"http/1.1"]).await;
    let (engine, _events, cmd_tx) = spawn_mitm_engine(service.clone(), der).await;

    let mut tls = mitm_tls(&engine, origin.port(), &service, &[b"http/1.1"]).await;
    tls.write_all(b"GET /h1 HTTP/1.1\r\nHost: origin.local\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(TEST_TIMEOUT, tls.read_to_end(&mut raw)).await;
    let text = String::from_utf8_lossy(&raw);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.contains("origin saw HTTP/2.0"), "{text}");
    assert!(text.contains("x-origin-host: none"), "{text}");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn te_trailers_reaches_h2_origin() {
    // gRPC без `te: trailers` отвергается; `Connection: te` рядом заставил бы
    // h2-клиент hyper снять и сам TE.
    let certs = tempfile::tempdir().expect("certs");
    let service = Arc::new(RcgenCertificateService::new(certs.path().to_path_buf()));
    let (origin, der) = spawn_origin(&[b"h2", b"http/1.1"]).await;
    let (engine, _events, cmd_tx) = spawn_mitm_engine(service.clone(), der).await;

    let tls = mitm_tls(&engine, origin.port(), &service, &[b"h2", b"http/1.1"]).await;
    let (parts, body) = h2_get_with(tls, origin.port(), &[("te", "trailers")]).await;
    assert_eq!(body, "origin saw HTTP/2.0");
    assert_eq!(parts.headers["x-origin-te"], "trailers");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn te_trailers_reaches_h1_origin() {
    let certs = tempfile::tempdir().expect("certs");
    let service = Arc::new(RcgenCertificateService::new(certs.path().to_path_buf()));
    let (origin, der) = spawn_origin(&[b"http/1.1"]).await;
    let (engine, _events, cmd_tx) = spawn_mitm_engine(service.clone(), der).await;

    let tls = mitm_tls(&engine, origin.port(), &service, &[b"h2", b"http/1.1"]).await;
    let (parts, body) = h2_get_with(tls, origin.port(), &[("te", "trailers")]).await;
    assert_eq!(body, "origin saw HTTP/1.1");
    assert_eq!(parts.headers["x-origin-te"], "trailers");
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
