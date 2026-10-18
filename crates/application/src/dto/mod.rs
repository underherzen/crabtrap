//! DTO на границах слоёв (engine → application → UI и обратно).

pub mod capture;
pub mod certificates;
pub mod clients;
pub mod compose;
pub mod inspection;
pub mod proxy_config;
pub mod rules;
pub mod scripting;
pub mod sessions;
pub mod updates;

pub use capture::{Direction, ExchangeSummaryDto, HeaderEntryDto, ProxyEvent, TrafficStatsDto};
pub use proxy_config::{EngineCommand, EngineStatusDto, ProxySettingsDto, SystemProxyStatusDto};
pub use rules::{InterceptResolutionDto, RuleSetDto, ThrottleSpecDto};
