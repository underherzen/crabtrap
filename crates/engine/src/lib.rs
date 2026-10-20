//! Engine — адаптер прокси-движка crabtrap (Phase 2): прозрачный HTTP-прокси
//! + CONNECT-туннелирование, чистый tokio, без gpui.
//!
//! Слой знает только порты application (`EventPublisher`,
//! `InterceptionPipeline`) и публикует `ProxyEvent`'ы; команды принимает
//! через `async_channel` (`EngineCommand`). MITM-расшифровка — Phase 3.
//!
//! Точка входа: [`run`]; headless-бинарь — `crabtrapd`.

mod body;
mod bypass;
mod client;
mod commands;
mod connect;
mod direct;
mod direct_hit;
pub mod error;
mod exchange_timing;
mod forward;
mod http_proxy;
mod intercept_body;
mod mitm;
mod mobileconfig;
mod reverse;
mod server;
mod throttle;
mod tunnel_record;
mod wifi;
mod ws;
mod ws_frames;

use std::sync::Arc;

use application::dto::proxy_config::EngineCommand;
use application::services::{CertificateService, EventPublisher, InterceptionPipeline};
use domain::entities::network::DnsOverride;
use domain::repositories::BodyRepository;
use tokio::net::TcpListener;

pub use crate::direct::{DirectHttpClient, DirectRequests, direct_channel};
pub use crate::error::EngineError;

/// Конфигурация запуска движка.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Адрес HTTP(S)-листенера, например `127.0.0.1:8080`.
    pub listen_addr: String,
    /// Максимум одновременных клиентских соединений.
    pub max_concurrent_conns: usize,
    /// Доверенные DER-корни СВЕРХ системных (тесты локальных origin,
    /// будущие upstream-прокси).
    pub extra_root_certs: Vec<Vec<u8>>,
    /// DNS-override'ы на старте (дальше — EngineCommand::SetDnsOverrides).
    pub dns_overrides: Vec<DnsOverride>,
    /// Захватывать ли тела в BodyRepository (Phase 4).
    pub record_bodies: bool,
    /// Потолок tee-буфера тела: выше — сохраняется обрезанный префикс,
    /// поток продолжает проксироваться (D5).
    pub body_capture_cap_bytes: u64,
    /// Потолок тела, которое движок собирает в память для пайплайна
    /// (`InterceptionPipeline::wants_body`: скрипты, rewrite тела). Больше —
    /// тело идёт потоком без правок.
    pub intercept_body_cap_bytes: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:8080".to_string(),
            max_concurrent_conns: 1024,
            extra_root_certs: Vec::new(),
            dns_overrides: Vec::new(),
            record_bodies: true,
            body_capture_cap_bytes: 50 * 1024 * 1024,
            intercept_body_cap_bytes: 16 * 1024 * 1024,
        }
    }
}

impl EngineConfig {
    /// Из настроек прокси (кто собирает композицию — уже держит свой конфиг;
    /// сам engine от крейта `config` не зависит).
    pub fn new(listen_addr: impl Into<String>, max_concurrent_conns: usize) -> Self {
        Self {
            listen_addr: listen_addr.into(),
            max_concurrent_conns,
            ..Default::default()
        }
    }

    /// Extra-корни доверия исходящего TLS (DER).
    pub fn with_extra_root_certs(mut self, ders: Vec<Vec<u8>>) -> Self {
        self.extra_root_certs = ders;
        self
    }

    /// DNS-override'ы (host → ip) на старте.
    pub fn with_dns_overrides(mut self, overrides: Vec<DnsOverride>) -> Self {
        self.dns_overrides = overrides;
        self
    }

    /// Настройки захвата тел (Phase 4): вкл/выкл + потолок tee-буфера.
    pub fn with_body_capture(mut self, record_bodies: bool, cap_bytes: u64) -> Self {
        self.record_bodies = record_bodies;
        self.body_capture_cap_bytes = cap_bytes;
        self
    }

    /// Потолок тела, собираемого для пайплайна перехвата.
    pub fn with_intercept_body_cap(mut self, cap_bytes: u64) -> Self {
        self.intercept_body_cap_bytes = cap_bytes;
        self
    }
}

/// Зависимости движка (порты application).
pub struct EngineDeps {
    pub events: Arc<dyn EventPublisher>,
    pub pipeline: Arc<dyn InterceptionPipeline>,
    /// Сертификаты MITM (Phase 3): реализация в infrastructure, движок
    /// получает PEM-DTO и не зависит от rcgen.
    pub certificate_service: Arc<dyn CertificateService>,
    /// Хранилище захваченных тел (Phase 4): tee в MonitoredBody сбрасывает
    /// сюда декодированные кадры.
    pub bodies: Arc<dyn BodyRepository>,
    /// Очередь Repeat / Compose из UI ([`direct_channel`]); `None` — без
    /// прямой отправки (headless, тесты).
    pub direct_requests: Option<DirectRequests>,
}

/// Пауза при ошибках accept (EMFILE и т.п.).
const ACCEPT_BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);

/// Процесс-широкий crypto-провайдер rustls: ring (D2). Идемпотентно —
/// второй вызов (или установка внутри rustls-фич) не ошибка.
fn ensure_ring_provider() {
    if let Err(already) = rustls::crypto::ring::default_provider().install_default() {
        tracing::debug!(
            provider = ?already.cipher_suites.len(),
            "rustls crypto provider already installed, keeping it"
        );
    }
}

/// Запустить движок: слушает `cfg.listen_addr`, обслуживает соединения и
/// обрабатывает команды из `cmd_rx` до `Stop` (или закрытия канала).
///
/// Возвращает `Ok(())` на штатный Stop. Листенер встаёт немедленно —
/// повторный `Start` с другим адресом игнорируется (рестарт процесса —
/// смена адреса в Phase 2 не on the fly).
pub async fn run(
    cfg: EngineConfig,
    deps: EngineDeps,
    cmd_rx: async_channel::Receiver<EngineCommand>,
) -> Result<(), EngineError> {
    ensure_ring_provider();
    let dns_overrides = Arc::new(tokio::sync::RwLock::new(cfg.dns_overrides.clone()));
    let upstream = client::UpstreamClients::build(&cfg.extra_root_certs, &dns_overrides)?;
    let state = Arc::new(commands::EngineState::new(
        Arc::clone(&deps.events),
        deps.pipeline,
        upstream,
        cfg.extra_root_certs.clone(),
        deps.certificate_service,
        dns_overrides,
        deps.bodies,
        cfg.record_bodies,
        cfg.body_capture_cap_bytes,
        cfg.intercept_body_cap_bytes,
    ));

    // Сначала состояние/события, потом bind: ошибка bind видна UI как
    // EngineStatus{error}, а не только как возврат Err (супервизор app
    // перезапускает движок по кнопке Start).
    let listener = match TcpListener::bind(&cfg.listen_addr).await {
        Ok(l) => l,
        Err(source) => {
            let msg = format!("cannot bind {}: {source}", cfg.listen_addr);
            tracing::error!(%msg, "engine bind failed");
            state.set_listening(None).await;
            state.publish_error(msg).await;
            return Err(EngineError::Bind {
                addr: cfg.listen_addr.clone(),
                source,
            });
        }
    };
    let local_addr = listener
        .local_addr()
        .map_err(|source| EngineError::Bind {
            addr: cfg.listen_addr.clone(),
            source,
        })?
        .to_string();
    tracing::info!(%local_addr, max_concurrent_conns = cfg.max_concurrent_conns, "crabtrap engine listening");

    if let Some(requests) = &deps.direct_requests {
        requests.open();
    }
    state.set_listening(Some(local_addr)).await;

    let semaphore = Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrent_conns));
    let mut reverse = reverse::ReverseProxies::new(
        Arc::clone(&state),
        Arc::clone(&semaphore),
        listener.local_addr().ok(),
    );
    let mut conns: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    if let Some(queue) = deps.direct_requests {
        conns.spawn(direct::serve(
            Arc::clone(&state),
            Arc::clone(&semaphore),
            queue,
        ));
    }

    let outcome = loop {
        tokio::select! {
            biased;
            maybe_cmd = cmd_rx.recv() => {
                match maybe_cmd {
                    // Листенеры Reverse Proxy живут в этом цикле (лимит
                    // соединений общий с основным).
                    Ok(EngineCommand::SetReverseProxies { entries }) => reverse.apply(entries).await,
                    Ok(cmd) => {
                        if state.handle_command(cmd).await == commands::CommandFlow::Shutdown {
                            tracing::info!("engine stopping (Stop command)");
                            break Ok(());
                        }
                    }
                    Err(_) => {
                        tracing::info!("command channel closed, engine shutting down");
                        break Ok(());
                    }
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        let _ = stream.set_nodelay(true);
                        let permit = match Arc::clone(&semaphore).try_acquire_owned() {
                            Ok(permit) => permit,
                            Err(_) => {
                                tracing::warn!(%peer, "connection limit reached, dropping connection");
                                continue;
                            }
                        };
                        // Завершённые задачи соединений не копим в JoinSet.
                        while conns.try_join_next().is_some() {}
                        conns.spawn(server::serve_connection(
                            Arc::clone(&state),
                            stream,
                            peer,
                            permit,
                            None,
                        ));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed, backing off");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                }
            }
        }
    };

    // Graceful shutdown: перестать принимать (drop listener), прервать
    // соединения и туннели, дождаться завершения задач.
    drop(listener);
    reverse.stop_all().await;
    conns.abort_all();
    while conns.join_next().await.is_some() {}
    state.abort_tunnels().await;
    state.set_listening(None).await;
    tracing::info!("engine stopped");
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_listens_on_8080() {
        assert_eq!(EngineConfig::default().listen_addr, "127.0.0.1:8080");
        assert!(EngineConfig::default().max_concurrent_conns > 0);
    }
}
