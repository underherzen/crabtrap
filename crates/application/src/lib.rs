//! Application-слой crabtrap: use cases, DTO и ПОРТЫ (trait'ы сервисов).
//!
//! Правила слоя:
//! - **Без tokio** — runtime-agnostic (движок/engine и UI сами выбирают
//!   рантайм; обмен между ними — только через `async-channel`).
//! - Оркестрация «что сделать», но не детали IO: реализации портов —
//!   в `infrastructure` (каналы, файлы, osascript) и `engine` (прокси).
//! - Use case принимает зависимости явно через `new(Arc<dyn ...>)`.

pub mod dto;
pub mod matching;
pub mod services;
pub mod use_cases;
