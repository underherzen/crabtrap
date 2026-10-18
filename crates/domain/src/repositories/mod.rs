//! Контракты репозиториев. Реализации — в `infrastructure`.
//! Все методы возвращают `Result<_, DomainError>` (Option — для «не найдено»).

pub mod body_repository;
pub mod certificate_repository;
pub mod exchange_repository;
pub mod external_proxy_repository;
pub mod network_repository;
pub mod rule_repository;
pub mod session_repository;
pub mod ssl_proxy_repository;

pub use body_repository::BodyRepository;
pub use certificate_repository::CertificateRepository;
pub use exchange_repository::{ExchangeRepository, RequestHead, TrafficStats};
pub use external_proxy_repository::ExternalProxyRepository;
pub use network_repository::NetworkRepository;
pub use rule_repository::RuleRepository;
pub use session_repository::{BodySource, SessionRepository};
pub use ssl_proxy_repository::SslProxyRepository;
