//! Скрипты (Track S): окно Script Editor и консоль. Список скриптов —
//! вкладка Scripting окна Tools (`ui::rules`).

pub mod console;
mod editor;

pub use editor::ScriptTools;
pub mod window;
