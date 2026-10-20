//! Общее состояние движка и обработка `EngineCommand`.
//!
//! Снапшоты правил/throttle хранятся под `RwLock` и читаются конвейером
//! (hot-reload без разрыва соединений). Фазы 6+ начнут их применять;
//! в Phase 2 они только принимаются и логируются.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use tokio::sync::RwLock;

use application::dto::proxy_config::EngineCommand;
use application::dto::rules::ThrottleSpecDto;
use application::services::{CertificateService, EventPublisher, InterceptionPipeline};
use domain::entities::network::{DnsOverride, ExternalProxyConfig, spoofed_ip};
use domain::entities::rule::{RuleSet, SslProxyEntry};
use domain::repositories::BodyRepository;
use domain::value_objects::ConnectionId;

use crate::client::{UpstreamClient, UpstreamClients};
use crate::mitm::CertIssuer;

/// Что делать командному циклу дальше.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandFlow {
    Continue,
    /// Stop: прекратить accept, оборвать задачи соединений.
    Shutdown,
}

/// Состояние, разделяемое между листенером, соединениями и туннелями.
pub(crate) struct EngineState {
    pub(crate) events: Arc<dyn EventPublisher>,
    pub(crate) pipeline: Arc<dyn InterceptionPipeline>,
    /// Исходящие клиенты; пересоздаются при смене DNS-override'ов
    /// (см. [`EngineState::upstream`]).
    upstream: std::sync::RwLock<UpstreamClients>,
    /// Доверенные корни сверх системных — для пересоздания клиентов.
    extra_root_certs: Vec<Vec<u8>>,
    /// Выпускатель MITM-листьев (Phase 3): LRU CertifiedKey + ServerConfig.
    pub(crate) certs: Arc<CertIssuer>,
    /// Адрес листенера (None — остановлен).
    listen_addr: RwLock<Option<String>>,
    active_conns: AtomicUsize,
    conn_seq: AtomicU64,
    /// Снапшот правил (EngineCommand::UpdateRules).
    pub(crate) rules: RwLock<RuleSet>,
    pub(crate) throttle: RwLock<Option<ThrottleSpecDto>>,
    pub(crate) ssl_entries: RwLock<Vec<SslProxyEntry>>,
    pub(crate) external_proxy: RwLock<Option<ExternalProxyConfig>>,
    /// DNS-override'ы: разделяются с исходящим коннектором (Arc), применяются
    /// и при dial-first CONNECT, и при dial UpstreamClient.
    pub(crate) dns_overrides: Arc<RwLock<Vec<DnsOverride>>>,
    /// Задачи CONNECT-туннелей (переживают http1-соединение, из которого
    /// выросли) — Stop их тоже прерывает.
    pub(crate) tunnels: RwLock<tokio::task::JoinSet<()>>,
    /// Хранилище тел (Phase 4): tee-захват MonitoredBody.
    pub(crate) bodies: Arc<dyn BodyRepository>,
    /// Захват тел включён (EngineConfig.record_bodies; переключается
    /// командой SetBodyCapture без остановки движка).
    pub(crate) record_bodies: AtomicBool,
    /// Потолок tee-буфера одного тела (EngineConfig.body_capture_cap_bytes).
    pub(crate) body_capture_cap_bytes: u64,
    /// Потолок тела для пайплайна (EngineConfig.intercept_body_cap_bytes).
    pub(crate) intercept_body_cap_bytes: u64,
}

impl EngineState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        events: Arc<dyn EventPublisher>,
        pipeline: Arc<dyn InterceptionPipeline>,
        upstream: UpstreamClients,
        extra_root_certs: Vec<Vec<u8>>,
        certificate_service: Arc<dyn CertificateService>,
        dns_overrides: Arc<RwLock<Vec<DnsOverride>>>,
        bodies: Arc<dyn BodyRepository>,
        record_bodies: bool,
        body_capture_cap_bytes: u64,
        intercept_body_cap_bytes: u64,
    ) -> Self {
        Self {
            events,
            pipeline,
            upstream: std::sync::RwLock::new(upstream),
            extra_root_certs,
            certs: Arc::new(CertIssuer::new(certificate_service)),
            listen_addr: RwLock::new(None),
            active_conns: AtomicUsize::new(0),
            conn_seq: AtomicU64::new(0),
            rules: RwLock::new(RuleSet::default()),
            throttle: RwLock::new(None),
            ssl_entries: RwLock::new(Vec::new()),
            external_proxy: RwLock::new(None),
            dns_overrides,
            tunnels: RwLock::new(tokio::task::JoinSet::new()),
            bodies,
            record_bodies: AtomicBool::new(record_bodies),
            body_capture_cap_bytes,
            intercept_body_cap_bytes,
        }
    }

    /// Адрес из DNS Spoofing для хоста (`None` — обычный DNS).
    pub(crate) async fn spoofed_ip(&self, host: &str) -> Option<std::net::IpAddr> {
        spoofed_ip(&self.dns_overrides.read().await, host)
    }

    /// Исходящий клиент: обычный или HTTP/1.1 для апгрейдов.
    pub(crate) fn upstream(&self, upgrade: bool) -> UpstreamClient {
        let clients = self.upstream.read().unwrap_or_else(|e| e.into_inner());
        match upgrade {
            true => clients.upgrade.clone(),
            false => clients.client.clone(),
        }
    }

    /// Новая таблица DNS Spoofing. Изменилась — клиенты пересоздаются с
    /// пустыми пулами, иначе keep-alive соединения ещё до 90 с ходили бы по
    /// старому адресу. Запросы в полёте доживают на старых клиентах.
    /// Клиенты строятся вне цикла команд (загрузка системных корней
    /// синхронная и небыстрая); таблица и клиенты меняются вместе — запрос
    /// не увидит новую таблицу со старым пулом.
    async fn set_dns_overrides(&self, entries: Vec<DnsOverride>) {
        if *self.dns_overrides.read().await == entries {
            return;
        }
        let roots = self.extra_root_certs.clone();
        let dns = Arc::clone(&self.dns_overrides);
        let built = tokio::task::spawn_blocking(move || UpstreamClients::build(&roots, &dns)).await;
        let mut table = self.dns_overrides.write().await;
        *table = entries;
        match built {
            Ok(Ok(fresh)) => *self.upstream.write().unwrap_or_else(|e| e.into_inner()) = fresh,
            Ok(Err(error)) => tracing::error!(
                %error,
                "dns overrides: upstream clients not rebuilt, pooled connections keep their address"
            ),
            Err(error) => tracing::error!(
                %error,
                "dns overrides: client rebuild task failed, pooled connections keep their address"
            ),
        }
    }

    /// Листенер встал на адрес.
    pub(crate) async fn set_listening(&self, addr: Option<String>) {
        *self.listen_addr.write().await = addr;
        self.publish_status();
    }

    /// Следующий порядковый номер соединения (для логов).
    pub(crate) fn next_conn_seq(&self) -> u64 {
        self.conn_seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Регистрация активного соединения + событие статуса.
    pub(crate) fn conn_opened(&self) {
        self.active_conns.fetch_add(1, Ordering::SeqCst);
        self.publish_status();
    }

    /// Завершение соединения (идемпотентно на уровне вызова — см.
    /// `ConnGuard`) + событие статуса.
    pub(crate) fn conn_closed(&self) {
        self.active_conns.fetch_sub(1, Ordering::SeqCst);
        self.publish_status();
    }

    fn publish_status(&self) {
        let listening = self.listen_addr.try_read().ok().and_then(|g| g.clone());
        self.events
            .publish(application::dto::capture::ProxyEvent::EngineStatus {
                listening,
                active_conns: self.active_conns.load(Ordering::SeqCst),
                error: None,
            });
    }

    /// Событие ошибки движка (bind и т.п.): UI видит причину, кнопка Start
    /// у супервизора перезапускает движок.
    pub(crate) async fn publish_error(&self, error: String) {
        *self.listen_addr.write().await = None;
        self.events
            .publish(application::dto::capture::ProxyEvent::EngineStatus {
                listening: None,
                active_conns: self.active_conns.load(Ordering::SeqCst),
                error: Some(error),
            });
    }

    /// Применить команду к состоянию. `CommandFlow::Shutdown` — сигнал
    /// командному циклу останавливаться.
    pub(crate) async fn handle_command(&self, cmd: EngineCommand) -> CommandFlow {
        match cmd {
            EngineCommand::Start { settings } => {
                let current = self.listen_addr.read().await.clone();
                match current {
                    Some(addr) if Some(&addr) == Some(&settings.listen_addr) => {
                        tracing::debug!(%addr, "Start: engine already listening, ignoring");
                    }
                    Some(addr) => {
                        tracing::warn!(
                            current = %addr,
                            requested = %settings.listen_addr,
                            "Start with a different address ignored: restart required (Phase 2)"
                        );
                    }
                    None => {
                        // Phase 2: run() слушает с самого старта; сюда движок
                        // попадает только после Stop (respawn листенера —
                        // фаза будущих итераций).
                        tracing::warn!(
                            requested = %settings.listen_addr,
                            "Start on stopped engine: restart required (Phase 2)"
                        );
                    }
                }
                CommandFlow::Continue
            }
            EngineCommand::Stop => CommandFlow::Shutdown,
            EngineCommand::UpdateRules { snapshot } => {
                tracing::info!(
                    version = snapshot.version,
                    ssl = snapshot.ssl_proxy.len(),
                    rewrite = snapshot.rewrite.len(),
                    "rules snapshot updated (hot-reload)"
                );
                self.pipeline.update_rules(snapshot.clone());
                *self.rules.write().await = snapshot;
                CommandFlow::Continue
            }
            EngineCommand::UpdateSslProxying { entries } => {
                tracing::info!(n = entries.len(), "ssl-proxying list updated");
                *self.ssl_entries.write().await = entries;
                CommandFlow::Continue
            }
            EngineCommand::UpdateThrottle { spec } => {
                match &spec {
                    Some(s) => {
                        tracing::info!(name = %s.name, down = ?s.down_bps, up = ?s.up_bps, latency_ms = s.latency_ms, "throttle updated")
                    }
                    None => tracing::info!("throttle disabled"),
                }
                *self.throttle.write().await = spec;
                CommandFlow::Continue
            }
            EngineCommand::ResolveIntercept { id, resolution } => {
                // Припаркованный обмен ждёт решения внутри пайплайна.
                self.pipeline.resolve_intercept(id, resolution);
                CommandFlow::Continue
            }
            EngineCommand::SetExternalProxy { cfg } => {
                if let Some(ref p) = cfg {
                    tracing::info!(host = %p.host, port = p.port, "external proxy set (применение — Phase 9)");
                } else {
                    tracing::info!("external proxy cleared");
                }
                *self.external_proxy.write().await = cfg;
                CommandFlow::Continue
            }
            EngineCommand::SetDnsOverrides { entries } => {
                tracing::info!(n = entries.len(), "dns overrides set");
                self.set_dns_overrides(entries).await;
                CommandFlow::Continue
            }
            // Перехватывается циклом `run` (там живут листенеры).
            EngineCommand::SetReverseProxies { .. } => CommandFlow::Continue,
            EngineCommand::SetBodyCapture { enabled } => {
                self.record_bodies.store(enabled, Ordering::Relaxed);
                tracing::info!(enabled, "body capture toggled (Phase 4)");
                CommandFlow::Continue
            }
        }
    }

    /// Graceful-остановка: прервать туннели.
    /// Запустить туннельную задачу, попутно убрав из JoinSet завершённые
    /// (иначе их записи копятся всё время работы движка).
    pub(crate) async fn spawn_tunnel(
        &self,
        task: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let mut tunnels = self.tunnels.write().await;
        while tunnels.try_join_next().is_some() {}
        tunnels.spawn(task);
    }

    pub(crate) async fn abort_tunnels(&self) {
        self.tunnels.write().await.abort_all();
    }
}

/// Идемпотентный гарды закрытия соединения: http1-фьючерс и туннельная
/// задача завершаются в разное время — событие/декремент строго один раз.
///
/// Гард держит слот лимита соединений (`_permit`): он освобождается, когда
/// уходит последний клон — т.е. и после завершения туннеля, выросшего из
/// этого соединения.
pub(crate) struct ConnGuard {
    state: Arc<EngineState>,
    conn_id: ConnectionId,
    closed: std::sync::atomic::AtomicBool,
    /// CONNECT отдал соединение туннельной задаче — закрывает она.
    upgraded: std::sync::atomic::AtomicBool,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl ConnGuard {
    pub(crate) fn new(
        state: Arc<EngineState>,
        conn_id: ConnectionId,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Arc<Self> {
        Arc::new(Self {
            state,
            conn_id,
            closed: std::sync::atomic::AtomicBool::new(false),
            upgraded: std::sync::atomic::AtomicBool::new(false),
            _permit: permit,
        })
    }

    /// Соединение уходит в туннель: http1-задача его больше не закрывает.
    pub(crate) fn mark_upgraded(&self) {
        self.upgraded.store(true, Ordering::SeqCst);
    }

    /// Закрыть по завершении http1-задачи, если соединение не стало туннелем.
    pub(crate) fn close_unless_upgraded(&self) {
        if !self.upgraded.load(Ordering::SeqCst) {
            self.close();
        }
    }

    /// Идентификатор охраняемого соединения (MITM-ветка поднимает на
    /// туннеле новый ProxyService с тем же conn_id).
    pub(crate) fn conn_id(&self) -> ConnectionId {
        self.conn_id
    }

    /// Закрыть соединение (первый вызов публикует событие). Последний клон
    /// гарда закрывает и сам (Drop) — на случай абортнутой туннельной задачи.
    pub(crate) fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.state.conn_closed();
        self.state
            .events
            .publish(application::dto::capture::ProxyEvent::ConnectionClosed { id: self.conn_id });
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.close();
    }
}
