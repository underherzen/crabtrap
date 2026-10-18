//! Исполнение скриптов (Track S): JS-движок boa в пуле потоков за портом
//! `ScriptRuntime`.

mod addons;
mod bindings;
mod query;
mod runtime;
mod shared_state;
mod worker;

pub use runtime::{BoaScriptRuntime, DEFAULT_SCRIPT_TIMEOUT};

#[cfg(test)]
mod integration_tests;
