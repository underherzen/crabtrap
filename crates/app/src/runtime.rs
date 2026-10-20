//! EngineRuntime — мост async↔gpui (решение D4 в PLAN.md) + супервизор.
//!
//! Архитектура (починено «Start не работает», 2026-10-03):
//! - ОДИН долгоживущий tokio-рантайм на потоке `crabtrap-engine` — его
//!   `Handle` используют UI-задачи (сертификаты, поиск) и он НЕ умирает
//!   вместе с движком.
//! - `engine::run` — ПЕРЕЗАПУСКАЕМАЯ задача на этом рантайме: `Stop`
//!   завершает её (порт освобождается), следующий `Start` через
//!   [`EngineHandle`] поднимает заново (свежий командный канал, перепуш
//!   правил/throttle/ssl-списка/захвата тел из репозиториев).
//! - Обмен с gpui ТОЛЬКО через async-channel: `EngineCommand` сюда,
//!   `ProxyEvent` обратно (`ChannelEventPublisher` живёт независимо от
//!   рестартов движка).

use std::sync::Arc;
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::proxy_config::{EngineCommand, ProxySettingsDto};
use application::dto::rules::ThrottleSpecDto;
use application::services::{CertificateService, InterceptionPipeline};
use application::use_cases::RecordExchangeEventUseCase;
use async_channel::{Receiver, Sender};
use domain::repositories::{BodyRepository, RuleRepository};
use gpui_kit::App;
use infrastructure::services::ChannelEventPublisher;

use crate::ui::clients::ClientsStore;
use crate::ui::scripting::console::ScriptConsoleStore;
use crate::ui::status_bar::EngineStatusStore;
use crate::ui::traffic::store::TrafficStore;

/// Сколько событий дренировать за одно пробуждение pump'а.
const PUMP_BATCH: usize = 64;
/// Сколько ждать остановки рантайма при shutdown.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Вместимость командного канала движка.
const CMD_CHANNEL_CAP: usize = 16;

/// Зависимости для запуска движка (клонируются в каждый рестарт).
#[derive(Clone)]
struct EngineBoot {
    events: Arc<ChannelEventPublisher>,
    pipeline: Arc<dyn InterceptionPipeline>,
    certificate_service: Arc<dyn CertificateService>,
    bodies: Arc<dyn BodyRepository>,
    rules: Option<Arc<dyn RuleRepository>>,
    /// Очередь Repeat / Compose: одна на приложение, переживает рестарты.
    direct_requests: engine::DirectRequests,
}

/// Супервизор движка: держит командный канал актуальным, перезапускает
/// `engine::run` после Stop/падения, перепушивает горячее состояние.
struct EngineSupervisor {
    rt: tokio::runtime::Handle,
    boot: EngineBoot,
    /// Конфиг следующего подъёма движка: адрес листенера меняет `Start`
    /// (переключение доступа из локальной сети).
    cfg: std::sync::RwLock<engine::EngineConfig>,
    /// Текущий командный канал (меняется на каждый рестарт движка).
    cmd_tx: std::sync::RwLock<Sender<EngineCommand>>,
    /// Задача движка (None/finished = движок не работает).
    engine_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Последний SetBodyCapture — перепушить после рестарта.
    last_body_capture: std::sync::RwLock<Option<bool>>,
    /// Движку отправлен Stop: задача дописывает ресурсы, ждём её конца
    /// перед рестартом (порт закрывается раньше, чем JoinHandle
    /// помечается finished — без флага здесь race).
    stopping: std::sync::atomic::AtomicBool,
    /// Сериализация рестартов (tokio-мьют: await внутри rt).
    restart_lock: tokio::sync::Mutex<()>,
}

impl EngineSupervisor {
    /// Поднять движок, если он не работает; после подъёма отправить `cmd`
    /// (обычно `Start`). Неблокирующе: работа уезжает на rt.
    /// `Start` с другим адресом переносит работающий листенер: Stop
    /// текущему движку и подъём на новом адресе.
    fn request_start(self: &Arc<Self>, cmd: EngineCommand) {
        let sup = Arc::clone(self);
        self.rt.spawn(async move {
            let _guard = sup.restart_lock.lock().await;
            let current = sup
                .engine_task
                .lock()
                .expect("engine_task lock")
                .as_ref()
                .is_some_and(|t| !t.is_finished());
            let moved = match &cmd {
                EngineCommand::Start { settings } => sup.apply_settings(settings),
                _ => false,
            };
            let stopping = sup.stopping.load(std::sync::atomic::Ordering::SeqCst);
            if current && moved && !stopping {
                let tx = sup.cmd_tx.read().expect("cmd_tx lock").clone();
                let _ = tx.try_send(EngineCommand::Stop);
            }
            if !current || stopping || moved {
                sup.spawn_engine_locked().await;
            }
            let tx = sup.cmd_tx.read().expect("cmd_tx lock").clone();
            let _ = tx.try_send(cmd);
        });
    }

    /// Запомнить настройки `Start` для подъёма; true — сменился адрес.
    fn apply_settings(&self, settings: &ProxySettingsDto) -> bool {
        let mut cfg = self.cfg.write().expect("cfg lock");
        let moved = cfg.listen_addr != settings.listen_addr;
        cfg.listen_addr = settings.listen_addr.clone();
        cfg.max_concurrent_conns = settings.max_concurrent_conns;
        moved
    }

    /// Развернуть новую задачу engine::run + свежий канал + перепуш
    /// состояния. Вызывать под `restart_lock`.
    async fn spawn_engine_locked(self: &Arc<Self>) {
        // Дождаться завершения старой задачи (порт должен освободиться
        // до повторного bind). Guard не живёт через await (Send).
        let old_task = self.engine_task.lock().expect("engine_task lock").take();
        if let Some(task) = old_task {
            let _ = task.await;
        }

        let (tx, rx) = async_channel::bounded(CMD_CHANNEL_CAP);
        let boot = self.boot.clone();
        let cfg = self.cfg.read().expect("cfg lock").clone();
        let handle = self.rt.spawn(async move {
            let deps = engine::EngineDeps {
                events: boot.events,
                pipeline: boot.pipeline,
                certificate_service: boot.certificate_service,
                bodies: boot.bodies,
                direct_requests: Some(boot.direct_requests),
            };
            match engine::run(cfg, deps, rx).await {
                Ok(()) => tracing::info!("engine task finished (Stop)"),
                Err(e) => tracing::error!(error = %e, "engine task failed"),
            }
        });
        *self.cmd_tx.write().expect("cmd_tx lock") = tx;
        *self.engine_task.lock().expect("engine_task lock") = Some(handle);
        self.stopping
            .store(false, std::sync::atomic::Ordering::SeqCst);

        // Перепуш горячего состояния (правила могли обновиться, пока
        // движок стоял).
        if let Some(rules) = &self.boot.rules {
            match rules.get().await {
                Ok(snapshot) => {
                    let tx = self.cmd_tx.read().expect("cmd_tx lock").clone();
                    let _ = tx.try_send(EngineCommand::UpdateRules {
                        snapshot: snapshot.clone(),
                    });
                    let _ = tx.try_send(EngineCommand::UpdateSslProxying {
                        entries: snapshot.effective_ssl_entries(),
                    });
                    let _ = tx.try_send(EngineCommand::UpdateThrottle {
                        spec: snapshot
                            .effective_throttle()
                            .map(ThrottleSpecDto::from_profile),
                    });
                    let _ = tx.try_send(EngineCommand::SetDnsOverrides {
                        entries: snapshot.effective_dns_overrides(),
                    });
                    let _ = tx.try_send(EngineCommand::SetReverseProxies {
                        entries: snapshot.effective_reverse_proxies(),
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cannot re-push rules after engine restart")
                }
            }
        }
        if let Some(enabled) = *self.last_body_capture.read().expect("capture lock") {
            let tx = self.cmd_tx.read().expect("cmd_tx lock").clone();
            let _ = tx.try_send(EngineCommand::SetBodyCapture { enabled });
        }
    }
}

/// Умный хендл команд движка для UI. `send(Start{..})` перезапускает
/// остановленный движок; остальные команды идут в текущий канал (если
/// движок стоит — команда теряется с warning, как раньше).
#[derive(Clone)]
pub struct EngineHandle {
    sup: Arc<EngineSupervisor>,
}

impl EngineHandle {
    pub fn send(&self, cmd: EngineCommand) {
        if matches!(cmd, EngineCommand::Start { .. }) {
            self.sup.request_start(cmd);
            return;
        }
        if matches!(cmd, EngineCommand::Stop) {
            self.sup
                .stopping
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if let EngineCommand::SetBodyCapture { enabled } = &cmd {
            *self.sup.last_body_capture.write().expect("capture lock") = Some(*enabled);
        }
        let tx = self.sup.cmd_tx.read().expect("cmd_tx lock").clone();
        if let Err(e) = tx.try_send(cmd) {
            tracing::warn!(error = %e, "engine command dropped (engine stopped?)");
        }
    }
}

pub struct EngineRuntime {
    sup: Arc<EngineSupervisor>,
    direct: engine::DirectHttpClient,
    thread: Option<std::thread::JoinHandle<()>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl EngineRuntime {
    /// Поднять рантайм + движок. События — через publisher (единый канал
    /// на всё приложение, живёт независимо от рестартов движка); тела — в
    /// общий BodyRepository (тот же инстанс, что у UI).
    pub fn start(
        events: Arc<ChannelEventPublisher>,
        pipeline: Arc<dyn InterceptionPipeline>,
        certificate_service: Arc<dyn CertificateService>,
        bodies: Arc<dyn BodyRepository>,
        rules: Option<Arc<dyn RuleRepository>>,
        cfg: engine::EngineConfig,
    ) -> Self {
        let (cmd_tx, cmd_rx) = async_channel::bounded(CMD_CHANNEL_CAP);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("engine tokio runtime builds");
        let rt = Arc::new(rt);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Поток-носитель рантайма: живёт до shutdown, обеспечивает
        // работу фоновых задач (UI-мосты, движок).
        let rt_thread = Arc::clone(&rt);
        let thread = std::thread::Builder::new()
            .name("crabtrap-engine".to_string())
            .spawn(move || {
                let rt = rt_thread;
                rt.block_on(async move {
                    let mut rx = shutdown_rx;
                    while !*rx.borrow_and_update() {
                        if rx.changed().await.is_err() {
                            break;
                        }
                    }
                });
                tracing::info!("engine runtime thread finished");
            })
            .expect("spawn crabtrap-engine thread");

        let handle = rt.handle().clone();
        let (direct, direct_requests) = engine::direct_channel();
        let sup = Arc::new(EngineSupervisor {
            rt: handle,
            boot: EngineBoot {
                events,
                pipeline,
                certificate_service,
                bodies,
                rules,
                direct_requests,
            },
            cfg: std::sync::RwLock::new(cfg),
            cmd_tx: std::sync::RwLock::new(cmd_tx),
            engine_task: std::sync::Mutex::new(None),
            last_body_capture: std::sync::RwLock::new(None),
            stopping: std::sync::atomic::AtomicBool::new(false),
            restart_lock: tokio::sync::Mutex::new(()),
        });
        // Первый запуск движка (порт из cfg).
        let sup_for_start = Arc::clone(&sup);
        sup.rt.spawn(async move {
            let _guard = sup_for_start.restart_lock.lock().await;
            sup_for_start.spawn_engine_locked().await;
        });
        let _ = cmd_rx; // приёмник уезжает в engine::run внутри spawn_engine_locked

        Self {
            sup,
            direct,
            thread: Some(thread),
            shutdown_tx,
        }
    }

    /// Хендл tokio-рантайма движка (для UI-задач, требующих tokio).
    /// Стабилен между рестартами движка.
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.sup.rt.clone()
    }

    /// Прямая отправка запросов (Repeat / Compose) через движок.
    pub fn direct_client(&self) -> engine::DirectHttpClient {
        self.direct.clone()
    }

    /// Умный отправитель команд (перезапускает движок на Start).
    pub fn command_handle(&self) -> EngineHandle {
        EngineHandle {
            sup: Arc::clone(&self.sup),
        }
    }

    /// Приёмник событий движка (клонируется в pump'ы).
    pub fn events(&self, publisher: &ChannelEventPublisher) -> Receiver<ProxyEvent> {
        let _ = &self.sup;
        publisher.subscribe()
    }

    /// Graceful-остановка: сигнал потоку-носителю + join с таймаутом.
    pub fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(thread) = self.thread.take() {
            let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
            std::thread::spawn(move || {
                let _ = thread.join();
                let _ = done_tx.send(());
            });
            if done_rx.recv_timeout(SHUTDOWN_TIMEOUT).is_err() {
                tracing::warn!("engine thread did not stop within timeout");
            }
        }
    }
}

/// Pump событий движка → GPUI-Entities (пачками по PUMP_BATCH).
///
/// Каждый прогон: пачка событий → `RecordExchangeEventUseCase` (доменный
/// capture-конвейер) → `TrafficStore.apply_batch` → `EngineStatusStore`.
/// Репозитории in-memory (std-mutex, без реальных await-точек), поэтому
/// use case исполняется `block_on`'ом прямо в gpui-таске — завершается
/// первым же poll'ом (та же аргументация, что у tee-flush в engine/body.rs).
///
/// Запускается один раз на приложение; `cx.spawn` живёт на foreground
/// executor'е gpui, ожидание async-channel там легально (никакого
/// tokio-IO в gpui-тасках — D4).
/// Получатели событий в UI.
pub struct PumpTargets {
    pub status: gpui_kit::Entity<EngineStatusStore>,
    pub traffic: gpui_kit::Entity<TrafficStore>,
    pub clients: gpui_kit::Entity<ClientsStore>,
    /// Консоль скриптов (Track S).
    pub script_console: gpui_kit::Entity<ScriptConsoleStore>,
    /// Поток Mirror: завершённые обмены (`None` — поток не стартовал).
    pub mirror: Option<crate::mirror_worker::MirrorQueue>,
}

pub fn pump_events(
    cx: &mut App,
    rx: Receiver<ProxyEvent>,
    recorder: Arc<RecordExchangeEventUseCase>,
    targets: PumpTargets,
) {
    let PumpTargets {
        status: status_store,
        traffic: traffic_store,
        clients,
        script_console,
        mirror,
    } = targets;
    cx.spawn(async move |cx| {
        loop {
            let first = match rx.recv().await {
                Ok(event) => event,
                Err(_) => break, // канал закрыт — приложение завершается
            };
            let mut batch = Vec::with_capacity(PUMP_BATCH);
            batch.push(first);
            while batch.len() < PUMP_BATCH {
                match rx.try_recv() {
                    Ok(event) => batch.push(event),
                    Err(_) => break,
                }
            }
            cx.update(|app| {
                // Capture-конвейер: событие → HttpExchange в репозитории.
                // Ошибки (вытеснение ring buffer) уже смягчены в use case.
                for event in &batch {
                    if let Err(e) = futures::executor::block_on(recorder.execute(event.clone())) {
                        tracing::warn!(error = %e, "record exchange event failed");
                    }
                }
                status_store.update(app, |state, _| {
                    state.apply_batch(&batch);
                });
                traffic_store.update(app, |state, cx| {
                    state.apply_batch(&batch);
                    cx.notify();
                });
                ClientsStore::on_events(&clients, &batch, app);
                crate::ui::breakpoint::on_events(&batch, app);
                notify_engine_errors(&batch, app);
                // После recorder'а: ответ уже в репозитории.
                if let Some(mirror) = &mirror {
                    for event in &batch {
                        if let ProxyEvent::ExchangeCompleted { id, .. } = event {
                            mirror.completed(*id);
                        }
                    }
                }
                script_console.update(app, |console, cx| {
                    if console.apply_batch(&batch) {
                        cx.notify();
                    }
                });
            });
        }
    })
    .detach();
}

/// Ошибки движка, у которых нет своего окна, — уведомлением: листенер
/// Reverse Proxy не встал (в Tools запись выглядит включённой, а порт не
/// слушает), сообщение WebSocket не ушло.
fn notify_engine_errors(batch: &[ProxyEvent], cx: &mut App) {
    for event in batch {
        let message = match event {
            ProxyEvent::ReverseProxyStatus {
                error: Some(error), ..
            } => format!("Reverse Proxy: {error}"),
            ProxyEvent::WebSocketSendFailed { error, .. } => {
                format!("WebSocket message not sent: {error}")
            }
            _ => continue,
        };
        crate::ui::notify::notify_error(message, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn port_accepts(port: u16) -> bool {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    }

    /// Ждём состояние порта с таймаутом (рестарты движка асинхронны).
    fn wait_port(port: u16, want: bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if port_accepts(port) == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Регрессия «кнопка Start не работает» (2026-10-03): Stop завершает
    /// задачу движка, следующий Start поднимает заново (супервизор).
    #[test]
    fn engine_restarts_after_stop_and_start() {
        let port = free_port();
        let listen = format!("127.0.0.1:{port}");
        let events = Arc::new(ChannelEventPublisher::new());
        let pipeline: Arc<dyn InterceptionPipeline> =
            Arc::new(infrastructure::services::PassthroughInterceptionPipeline::new());
        let certs: Arc<dyn CertificateService> =
            Arc::new(infrastructure::services::RcgenCertificateService::new(
                tempfile::tempdir().unwrap().path().to_path_buf(),
            ));
        let bodies: Arc<dyn BodyRepository> = Arc::new(
            infrastructure::repositories::MemorySpillBodyStore::from_config(
                &config::AppConfig::default(),
            ),
        );
        let rules: Arc<dyn RuleRepository> =
            Arc::new(infrastructure::repositories::FileRuleRepository::new(
                tempfile::tempdir().unwrap().path().join("rules.json"),
            ));

        let runtime = EngineRuntime::start(
            events.clone(),
            pipeline,
            certs,
            bodies,
            Some(rules),
            engine::EngineConfig::new(&listen, 16),
        );

        // Стартовый подъём движка.
        assert!(
            wait_port(port, true, Duration::from_secs(10)),
            "engine did not start"
        );

        // Stop → порт освобождается.
        runtime.command_handle().send(EngineCommand::Stop);
        assert!(
            wait_port(port, false, Duration::from_secs(10)),
            "engine did not stop"
        );

        // Start → порт снова слушает (регрессия: раньше канал был мёртв).
        runtime.command_handle().send(EngineCommand::Start {
            settings: application::dto::proxy_config::ProxySettingsDto {
                listen_addr: listen.clone(),
                max_concurrent_conns: 16,
            },
        });
        assert!(
            wait_port(port, true, Duration::from_secs(10)),
            "engine did not restart"
        );

        runtime.shutdown();
    }

    fn start_runtime(listen: &str) -> EngineRuntime {
        let pipeline: Arc<dyn InterceptionPipeline> =
            Arc::new(infrastructure::services::PassthroughInterceptionPipeline::new());
        let certs: Arc<dyn CertificateService> =
            Arc::new(infrastructure::services::RcgenCertificateService::new(
                tempfile::tempdir().unwrap().path().to_path_buf(),
            ));
        let bodies: Arc<dyn BodyRepository> = Arc::new(
            infrastructure::repositories::MemorySpillBodyStore::from_config(
                &config::AppConfig::default(),
            ),
        );
        EngineRuntime::start(
            Arc::new(ChannelEventPublisher::new()),
            pipeline,
            certs,
            bodies,
            None,
            engine::EngineConfig::new(listen, 16),
        )
    }

    fn start_on(listen: &str) -> EngineCommand {
        EngineCommand::Start {
            settings: application::dto::proxy_config::ProxySettingsDto {
                listen_addr: listen.to_string(),
                max_concurrent_conns: 16,
            },
        }
    }

    /// Режим LAN переключается на лету: Start с другим адресом переносит
    /// работающий листенер, а Start после Stop поднимает новый адрес.
    #[test]
    fn start_with_new_address_moves_the_listener() {
        let (first, second, third) = (free_port(), free_port(), free_port());
        let runtime = start_runtime(&format!("127.0.0.1:{first}"));
        assert!(
            wait_port(first, true, Duration::from_secs(10)),
            "engine did not start"
        );

        runtime
            .command_handle()
            .send(start_on(&format!("127.0.0.1:{second}")));
        assert!(
            wait_port(second, true, Duration::from_secs(10)),
            "new address not bound"
        );
        assert!(
            wait_port(first, false, Duration::from_secs(10)),
            "old address still bound"
        );

        runtime.command_handle().send(EngineCommand::Stop);
        assert!(
            wait_port(second, false, Duration::from_secs(10)),
            "engine did not stop"
        );
        runtime
            .command_handle()
            .send(start_on(&format!("127.0.0.1:{third}")));
        assert!(
            wait_port(third, true, Duration::from_secs(10)),
            "restart ignored new address"
        );

        runtime.shutdown();
    }
}
