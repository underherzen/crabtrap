//! Скрипты (Track S): готовые фрагменты кода и пробный запуск из
//! редактора. Правило-скрипт редактируется в `rules::script_edit`.

pub mod snippets;
mod test_script;

pub use test_script::{TestScriptDeps, TestScriptUseCase, describe_request, describe_response};
