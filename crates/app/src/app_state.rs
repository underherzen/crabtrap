//! AppState — общее состояние приложения: конфиг +
//! Arc<dyn Port>-зависимости + хендл движка. UI-компоненты получают его
//! клоном (всё внутри — Arc/Clone).

use std::sync::{Arc, RwLock};

use application::dto::proxy_config::ProxySettingsDto;

use application::services::{
    BodyDecoderService, CertificateService, ClientProcessService, DirectHttpService, ReleaseFeed,
    ScriptRuntime, SystemProxyService,
};
use domain::repositories::{BodyRepository, ExchangeRepository, RuleRepository};
use infrastructure::repositories::{
    FileRuleRepository, InMemoryExchangeRepository, MemorySpillBodyStore,
};
use infrastructure::services::ChannelEventPublisher;

use crate::runtime::EngineHandle;

/// Общее состояние приложения.
///
/// Поля-порты понадобятся экранам Phase 4+ (TrafficStore, инспекторы,
/// редактор правил); в Phase 3 экран сертификатов использует
/// `certificate_service`.
#[allow(dead_code)]
#[derive(Clone)]
pub struct AppState {
    pub config: config::AppConfig,
    pub exchange_repository: Arc<dyn ExchangeRepository>,
    pub body_repository: Arc<dyn BodyRepository>,
    pub rule_repository: Arc<dyn RuleRepository>,
    pub event_publisher: Arc<ChannelEventPublisher>,
    /// Сертификаты MITM (Phase 3): RcgenCertificateService.
    pub certificate_service: Arc<dyn CertificateService>,
    /// Декомпрессия тел инспектора (Phase 5): RustBodyDecoder (flate2+brotli).
    pub body_decoder: Arc<dyn BodyDecoderService>,
    /// Системный прокси macOS (networksetup): кнопка «macOS Proxy».
    pub system_proxy_service: Arc<dyn SystemProxyService>,
    /// Клиент обмена (Track G8): lsof + libproc + иконки NSWorkspace.
    pub client_process_service: Arc<dyn ClientProcessService>,
    /// Скрипты (Track S): тот же пул, что у движка — пробный запуск из
    /// редактора и общий `$sharedState`.
    pub script_runtime: Arc<dyn ScriptRuntime>,
    /// Repeat / Compose через движок (Phase 7b).
    pub direct_http: Arc<dyn DirectHttpService>,
    /// Check for Updates: последний релиз на GitHub.
    pub release_feed: Arc<dyn ReleaseFeed>,
    /// Хендл движка: команды UI → engine (async-channel, решение D4);
    /// `Start` перезапускает остановленный движок (супервизор).
    pub engine_command: EngineHandle,
    /// Текущий адрес листенера (меняется в Settings → Network).
    pub listen_addr: ListenAddr,
}

/// `owner/repo` релизов — из `repository` в Cargo.toml.
fn release_repo() -> &'static str {
    let url = env!("CARGO_PKG_REPOSITORY");
    crate::ui::updates::repo_slug(url).unwrap_or_else(|| {
        tracing::warn!(
            url,
            "package repository is not a GitHub URL: update check disabled"
        );
        ""
    })
}

/// Адрес листенера прокси. Меняется на лету (доступ из локальной сети),
/// поэтому не читается из `config` — там снимок на момент запуска.
#[derive(Clone)]
pub struct ListenAddr(Arc<RwLock<String>>);

impl ListenAddr {
    pub fn new(addr: impl Into<String>) -> Self {
        Self(Arc::new(RwLock::new(addr.into())))
    }

    pub fn get(&self) -> String {
        self.0.read().expect("listen addr lock").clone()
    }

    pub fn set(&self, addr: impl Into<String>) {
        *self.0.write().expect("listen addr lock") = addr.into();
    }
}

impl AppState {
    /// In-memory композиция Phase 2/3. В приложении используется
    /// [`Self::compose_with`] (общий body-store); оставлен для тестов/будущих
    /// сборок без общего стора.
    #[allow(dead_code)]
    pub fn compose(
        config: config::AppConfig,
        event_publisher: Arc<ChannelEventPublisher>,
        engine_command: EngineHandle,
        direct_http: Arc<dyn DirectHttpService>,
    ) -> Self {
        let body_repository = Arc::new(MemorySpillBodyStore::from_config(&config));
        let rule_repository = Arc::new(FileRuleRepository::from_config(&config));
        let certificate_service: Arc<dyn CertificateService> =
            Arc::new(infrastructure::services::RcgenCertificateService::from_config(&config));
        Self::compose_with(
            config,
            event_publisher,
            engine_command,
            body_repository,
            rule_repository,
            certificate_service,
            Arc::new(infrastructure::services::BoaScriptRuntime::new()),
            direct_http,
        )
    }

    /// Композиция с общим body-store (Phase 4): движок пишет тела туда же,
    /// куда UI будет их читать (tee ↔ инспектор — один инстанс).
    /// `rule_repository` передаётся снаружи: тот же инстанс получает
    /// супервизор движка (перепуш правил при рестарте). `certificate_service`
    /// — тот же инстанс, что у движка: иначе Regenerate CA на экране
    /// сертификатов не доходил до движка (он подписывал старым CA из памяти).
    #[allow(clippy::too_many_arguments)]
    pub fn compose_with(
        config: config::AppConfig,
        event_publisher: Arc<ChannelEventPublisher>,
        engine_command: EngineHandle,
        body_repository: Arc<MemorySpillBodyStore>,
        rule_repository: Arc<dyn RuleRepository>,
        certificate_service: Arc<dyn CertificateService>,
        script_runtime: Arc<dyn ScriptRuntime>,
        direct_http: Arc<dyn DirectHttpService>,
    ) -> Self {
        let exchange_repository =
            Arc::new(InMemoryExchangeRepository::new(config.capture.buffer_len));
        let body_decoder: Arc<dyn BodyDecoderService> =
            Arc::new(infrastructure::services::RustBodyDecoder::new());
        let system_proxy_service: Arc<dyn SystemProxyService> =
            Arc::new(infrastructure::services::NetworksetupSystemProxyService::new());
        let client_process_service: Arc<dyn ClientProcessService> =
            Arc::new(infrastructure::services::MacClientProcessService::new());
        let listen_addr = ListenAddr::new(config.proxy.listen_addr.clone());
        let release_feed: Arc<dyn ReleaseFeed> = Arc::new(
            infrastructure::services::GithubReleaseFeed::new(release_repo()),
        );
        Self {
            config,
            exchange_repository,
            body_repository,
            rule_repository,
            event_publisher,
            certificate_service,
            body_decoder,
            system_proxy_service,
            client_process_service,
            script_runtime,
            direct_http,
            release_feed,
            engine_command,
            listen_addr,
        }
    }

    /// Настройки для `EngineCommand::Start` (текущий адрес листенера).
    pub fn proxy_settings(&self) -> ProxySettingsDto {
        ProxySettingsDto {
            listen_addr: self.listen_addr.get(),
            max_concurrent_conns: self.config.proxy.max_concurrent_conns,
        }
    }
}
