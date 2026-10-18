//! ПОРТ исполнения скриптов (Track S). Реализация — infrastructure
//! (JS-движок в пуле потоков); пайплайн перехвата и пробный запуск из
//! редактора знают только этот интерфейс.
//!
//! Контракт:
//! - метод infallible: синтаксическая ошибка, исключение, таймаут —
//!   `ScriptRun.output = Err(текст)`, обмен идёт без изменений;
//! - скрипт изолирован: без доступа к файлам и сети;
//! - `$sharedState` общий для всех скриптов (живёт в памяти процесса).

use async_trait::async_trait;

use crate::dto::scripting::{ScriptCall, ScriptRun};

#[async_trait]
pub trait ScriptRuntime: Send + Sync {
    async fn run(&self, call: ScriptCall) -> ScriptRun;

    /// Сбросить `$sharedState` (кнопка в редакторе).
    fn clear_shared_state(&self) {}
}
