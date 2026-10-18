//! Общие помощники интеграционных тестов движка: тестовые двойники
//! (publisher/pipeline/cert-service), запуск движка, ожидание событий,
//! чтение головы ответа.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::services::{ConnectDecision, InterceptContext, InterceptOutcome, RouteDecision};
use application::services::{EventPublisher, InterceptionPipeline};
use async_trait::async_trait;
use domain::value_objects::Host;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

use engine::{EngineConfig, EngineDeps};

/// Сертификатный сервис, который не нужен: эти тесты не включают MITM
/// (ssl-список пуст → ветка TLS не достижима).
pub struct UnusedCertService;

#[async_trait]
impl application::services::CertificateService for UnusedCertService {
    async fn ensure_ca(
        &self,
    ) -> Result<application::dto::certificates::CaStatusDto, domain::errors::DomainError> {
        Err(domain::errors::DomainError::InfrastructureError(
            "no CA in passthrough tests".into(),
        ))
    }
    async fn issue_leaf(
        &self,
        _host: &str,
        _alpn: &[String],
    ) -> Result<application::dto::certificates::LeafCertDto, domain::errors::DomainError> {
        Err(domain::errors::DomainError::InfrastructureError(
            "no leaves in passthrough tests".into(),
        ))
    }
    async fn trust_status(
        &self,
    ) -> Result<application::dto::certificates::TrustStatusDto, domain::errors::DomainError> {
        Ok(Default::default())
    }
    async fn install_trust(
        &self,
    ) -> Result<application::dto::certificates::TrustStatusDto, domain::errors::DomainError> {
        Ok(Default::default())
    }
    async fn regenerate_ca(
        &self,
    ) -> Result<application::dto::certificates::CaStatusDto, domain::errors::DomainError> {
        Err(domain::errors::DomainError::InfrastructureError(
            "no CA in passthrough tests".into(),
        ))
    }
    fn ca_cert_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from("/unused/ca.pem")
    }
}

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

pub const TEST_TIMEOUT: Duration = Duration::from_secs(10);

// ==== тестовые двойники ====

/// Записывающий publisher: события копятся в Vec (запрашиваются снапшотом).
pub struct RecordingPublisher {
    events: std::sync::Mutex<Vec<ProxyEvent>>,
}

impl RecordingPublisher {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            events: std::sync::Mutex::new(Vec::new()),
        })
    }

    pub fn snapshot(&self) -> Vec<ProxyEvent> {
        self.events.lock().expect("events lock").clone()
    }

    /// Ждём EngineStatus{listening: Some} и возвращаем адрес.
    pub async fn wait_listening(self: &Arc<Self>) -> String {
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

pub struct TestPipeline;

#[async_trait]
impl InterceptionPipeline for TestPipeline {
    async fn on_connect(&self, _host: &Host, _port: u16) -> ConnectDecision {
        ConnectDecision::Passthrough
    }

    async fn on_request(&self, context: InterceptContext) -> InterceptOutcome {
        InterceptOutcome::Proceed {
            message: context.message,
            route: RouteDecision::Origin,
        }
    }

    async fn on_response(&self, context: InterceptContext) -> InterceptOutcome {
        InterceptOutcome::Proceed {
            message: context.message,
            route: RouteDecision::Origin,
        }
    }
}

/// Поднять движок на ephemeral-порту; возвращает (адрес, publisher, стоп).
/// Тела складываются в настоящий MemorySpillBodyStore (tempdir) — интеграция
/// tee → репозиторий проверяется по-настоящему.
pub async fn spawn_engine() -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<application::dto::proxy_config::EngineCommand>,
) {
    spawn_engine_with(EngineConfig::new("127.0.0.1:0", 16), body_store()).await
}

pub async fn spawn_engine_with(
    cfg: EngineConfig,
    bodies: Arc<infrastructure::repositories::MemorySpillBodyStore>,
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<application::dto::proxy_config::EngineCommand>,
) {
    spawn_engine_full(cfg, bodies, Arc::new(TestPipeline)).await
}

/// Движок с заданным пайплайном перехвата (правила — RulesInterceptionPipeline).
pub async fn spawn_engine_with_pipeline(
    pipeline: Arc<dyn InterceptionPipeline>,
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<application::dto::proxy_config::EngineCommand>,
) {
    spawn_engine_full(EngineConfig::new("127.0.0.1:0", 16), body_store(), pipeline).await
}

/// Движок с заданными конфигом и пайплайном.
pub async fn spawn_engine_cfg_pipeline(
    cfg: EngineConfig,
    pipeline: Arc<dyn InterceptionPipeline>,
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<application::dto::proxy_config::EngineCommand>,
) {
    spawn_engine_full(cfg, body_store(), pipeline).await
}

/// Движок с заданными конфигом, пайплайном и publisher'ом (пайплайн,
/// публикующий свои события, — скрипты — пишет в тот же recorder).
pub async fn spawn_engine_cfg_pipeline_with_events(
    cfg: EngineConfig,
    pipeline: Arc<dyn InterceptionPipeline>,
    publisher: Arc<RecordingPublisher>,
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<application::dto::proxy_config::EngineCommand>,
) {
    spawn_engine_inner(cfg, body_store(), pipeline, publisher).await
}

async fn spawn_engine_full(
    cfg: EngineConfig,
    bodies: Arc<infrastructure::repositories::MemorySpillBodyStore>,
    pipeline: Arc<dyn InterceptionPipeline>,
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<application::dto::proxy_config::EngineCommand>,
) {
    spawn_engine_inner(cfg, bodies, pipeline, RecordingPublisher::new()).await
}

/// Движок с очередью прямой отправки (Repeat / Compose).
pub async fn spawn_engine_with_direct() -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<application::dto::proxy_config::EngineCommand>,
    engine::DirectHttpClient,
) {
    let (client, queue) = engine::direct_channel();
    let (addr, events, cmd) = spawn_engine_deps(
        EngineConfig::new("127.0.0.1:0", 16),
        body_store(),
        Arc::new(TestPipeline),
        RecordingPublisher::new(),
        Some(queue),
    )
    .await;
    (addr, events, cmd, client)
}

async fn spawn_engine_inner(
    cfg: EngineConfig,
    bodies: Arc<infrastructure::repositories::MemorySpillBodyStore>,
    pipeline: Arc<dyn InterceptionPipeline>,
    publisher: Arc<RecordingPublisher>,
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<application::dto::proxy_config::EngineCommand>,
) {
    spawn_engine_deps(cfg, bodies, pipeline, publisher, None).await
}

async fn spawn_engine_deps(
    cfg: EngineConfig,
    bodies: Arc<infrastructure::repositories::MemorySpillBodyStore>,
    pipeline: Arc<dyn InterceptionPipeline>,
    publisher: Arc<RecordingPublisher>,
    direct_requests: Option<engine::DirectRequests>,
) -> (
    String,
    Arc<RecordingPublisher>,
    async_channel::Sender<application::dto::proxy_config::EngineCommand>,
) {
    let (cmd_tx, cmd_rx) = async_channel::bounded(16);
    let deps = EngineDeps {
        events: publisher.clone(),
        pipeline,
        certificate_service: Arc::new(UnusedCertService),
        bodies,
        direct_requests,
    };
    tokio::spawn(engine::run(cfg, deps, cmd_rx));
    let addr = publisher.wait_listening().await;
    (addr, publisher, cmd_tx)
}

/// Реальный body-store в tempdir (spill-порог маленький — большие тела
/// уходят на диск, как в продакшене).
pub fn body_store() -> Arc<infrastructure::repositories::MemorySpillBodyStore> {
    Arc::new(infrastructure::repositories::MemorySpillBodyStore::new(
        1024,
        u64::MAX,
        tempfile::tempdir().expect("body cache tempdir").keep(),
    ))
}

/// Прочитать только голову ответа (для CONNECT — дальше туннель).
pub async fn read_head_only(stream: &mut TcpStream) -> String {
    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(idx) = find_head_end(&buf) {
            return String::from_utf8_lossy(&buf[..idx]).into_owned();
        }
        let n = tokio::time::timeout_at(deadline, stream.read(&mut chunk))
            .await
            .expect("head-only timeout")
            .expect("head-only read");
        if n == 0 {
            panic!("EOF before response head");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

pub fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Подождать событие, удовлетворяющее предикату (события асинхронные).
pub async fn wait_event<T>(
    events: &Arc<RecordingPublisher>,
    pick: impl Fn(&ProxyEvent) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let Some(found) = events.snapshot().iter().find_map(&pick) {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected event did not arrive within timeout");
}
