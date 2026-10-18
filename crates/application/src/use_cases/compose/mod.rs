//! Сценарии Repeat / Edit & Resend / Compose (Phase 7b). Отправка — порт
//! `DirectHttpService` (реализация — engine: тот же конвейер, что у
//! клиентов, обмен попадает в трафик с тегом).

pub mod form;
mod repeat;

pub use repeat::{RepeatExchangeUseCase, SendComposedRequestUseCase};
