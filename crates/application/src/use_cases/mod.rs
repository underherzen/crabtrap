//! Use cases — сценарии «что сделать». Паттерн:
//! `struct XUseCase { deps: Arc<dyn T> }`, `new(...)`,
//! `pub async fn execute(dto) -> Result<Out, DomainError>`.
//!
//! Контексты зеркалят `dto/` и будущие экраны UI.

pub mod capture;
pub mod certificates;
pub mod compose;
pub mod inspection;
pub mod proxy_config;
pub mod rules;
pub mod scripting;
pub mod sessions;
pub mod updates;

pub use capture::{ClearTrafficUseCase, GetExchangeListUseCase, RecordExchangeEventUseCase};
pub use certificates::{GetCaStatusUseCase, InstallTrustUseCase, RegenerateCaUseCase};
pub use inspection::GetExchangeDetailUseCase;
pub use proxy_config::{
    GetSystemProxyStatusUseCase, SetSystemProxyUseCase, UnsetSystemProxyUseCase,
};
pub use rules::{GetRuleSetUseCase, UpdateRuleSetUseCase};
