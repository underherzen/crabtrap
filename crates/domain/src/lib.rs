//! Чистая предметная область crabtrap: сущности, value objects, контракты
//! репозиториев и доменные ошибки.
//!
//! Правила слоя:
//! - **Никакого IO**: ни файлов, ни сокетов, ни tokio, ни gpui.
//! - Репозитории — только trait'ы (контракты); реализации живут в
//!   `infrastructure`.
//! - Всё сериализуемо через serde (JSON-персистентность rules/сессий).

pub mod entities;
pub mod errors;
pub mod repositories;
pub mod value_objects;

pub use entities::*;
pub use errors::DomainError;
pub use repositories::*;
pub use value_objects::*;
