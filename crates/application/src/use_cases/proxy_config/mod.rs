//! Управление движком: start/stop, системный прокси, статус листенера.
//! Сценарии приедут вместе с реализацией ProxyEngineService в `engine`
//! (Phase 2) и SystemProxyService в `infrastructure`.

mod system_proxy;

pub use system_proxy::{
    GetSystemProxyStatusUseCase, SetSystemProxyUseCase, UnsetSystemProxyUseCase,
};
