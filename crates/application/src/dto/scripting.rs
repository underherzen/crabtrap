//! DTO скриптов (Track S): вызов JS-правила и его результат.
//!
//! Тела здесь — уже без Content-Encoding (пайплайн распаковывает перед
//! вызовом и решает, как отдать результат), `None` — тело не загружено.

use serde::{Deserialize, Serialize};

use domain::value_objects::http_vo::HeaderEntry;
use domain::value_objects::{ExchangeId, RuleId};

/// Какой скрипт выполнить.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptSourceDto {
    pub rule_id: RuleId,
    pub name: String,
    pub source: String,
}

/// Запрос глазами скрипта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRequestDto {
    pub method: String,
    /// Absolute URL.
    pub url: String,
    pub headers: Vec<HeaderEntry>,
    pub body: Option<Vec<u8>>,
}

/// Ответ глазами скрипта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptResponseDto {
    pub status: u16,
    pub headers: Vec<HeaderEntry>,
    pub body: Option<Vec<u8>>,
}

impl ScriptResponseDto {
    /// Пустой ответ Mock API: его заполняет `onResponse`.
    pub fn blank() -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: Some(Vec::new()),
        }
    }
}

/// Вызов: `response = None` — `onRequest`, иначе `onResponse`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptCall {
    pub script: ScriptSourceDto,
    /// `None` — пробный запуск из редактора (обмена нет).
    pub exchange_id: Option<ExchangeId>,
    pub request: ScriptRequestDto,
    pub response: Option<ScriptResponseDto>,
}

/// Что вернул скрипт.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptOutput {
    Request(ScriptRequestDto),
    Response(ScriptResponseDto),
    /// Функции фазы в скрипте нет — обмен идёт без изменений.
    NotDefined,
}

/// Уровень записи консоли скрипта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptLogLevel {
    Debug,
    Info,
    Log,
    Warn,
    Error,
}

/// Запись консоли (`console.log` / ошибка выполнения).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptLogDto {
    pub level: ScriptLogLevel,
    pub message: String,
}

/// Итог выполнения: результат (или текст ошибки) и консоль.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRun {
    pub output: Result<ScriptOutput, String>,
    pub logs: Vec<ScriptLogDto>,
}
