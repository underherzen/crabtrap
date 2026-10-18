//! Страница настройки устройства по адресу-ловушке `http://crabtrap.ssl`
//! (как chls.pro/ssl у Charles): прокси уже прописан на устройстве — набирать
//! IP Mac не нужно, адрес прокси на странице и в профиле — адрес листенера.

use std::sync::Arc;
use std::time::Duration;

use application::dto::proxy_config::EngineCommand;
use application::services::CertificateService as _;
use engine::{EngineConfig, EngineDeps};
use infrastructure::services::RcgenCertificateService;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

mod common;
use common::*;

/// Движок с настоящим CA во временном каталоге (страница и профиль его читают).
async fn spawn_engine_with_ca() -> (
    tempfile::TempDir,
    String,
    async_channel::Sender<EngineCommand>,
) {
    let certs = tempfile::tempdir().expect("certs");
    let service = Arc::new(RcgenCertificateService::new(certs.path().to_path_buf()));
    service.ensure_ca().await.expect("ca");
    let publisher = RecordingPublisher::new();
    let (cmd_tx, cmd_rx) = async_channel::bounded(16);
    let deps = EngineDeps {
        events: publisher.clone(),
        pipeline: Arc::new(TestPipeline),
        certificate_service: service,
        bodies: body_store(),
        direct_requests: None,
    };
    tokio::spawn(engine::run(
        EngineConfig::new("127.0.0.1:0", 16),
        deps,
        cmd_rx,
    ));
    let addr = publisher.wait_listening().await;
    (certs, addr, cmd_tx)
}

/// Запрос через прокси (absolute-form), ответ целиком (Connection: close).
async fn via_proxy(engine: &str, url: &str) -> String {
    let mut client = TcpStream::connect(engine).await.expect("connect");
    let request = format!("GET {url} HTTP/1.1\r\nHost: crabtrap.ssl\r\nConnection: close\r\n\r\n");
    client.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut raw)).await;
    String::from_utf8_lossy(&raw).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_host_serves_the_certificate_page() {
    let (_certs, engine, cmd_tx) = spawn_engine_with_ca().await;
    let response = via_proxy(&engine, "http://crabtrap.ssl/").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("crabtrap device setup"), "{response}");
    assert!(
        response.contains(&engine),
        "proxy address is the listener: {response}"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_host_profile_points_at_the_listener() {
    let (_certs, engine, cmd_tx) = spawn_engine_with_ca().await;
    let (_, port) = engine.rsplit_once(':').expect("port");
    let response = via_proxy(
        &engine,
        "http://crabtrap.ssl/crabtrap.mobileconfig?ssid=Home",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.contains("<key>ProxyServer</key><string>127.0.0.1</string>"),
        "{response}"
    );
    assert!(
        response.contains(&format!("<integer>{port}</integer>")),
        "{response}"
    );
    cmd_tx.send(EngineCommand::Stop).await.ok();
}
