//! Infrastructure-слой crabtrap: in-memory/файловые реализации портов
//! domain-репозиториев и application-сервисов.
//!
//! Здесь допустимы файлы, каналы, блокировки, каталоги — но НЕ бизнес-логика
//! (её место в domain/application). Сетевые адаптеры (osascript, security)
//! добавляются в Phase 3+.

pub mod repositories;
pub mod services;
