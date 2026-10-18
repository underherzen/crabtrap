//! Один JS-контекст boa (живёт в своём потоке пула: контексты `!Send`).
//!
//! Скрипт компилируется в замыкание-фабрику — его `var`/функции не
//! попадают в глобальный объект и не видны другим скриптам; фабрика
//! кешируется по (правило, хэш исходника).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;

use application::dto::scripting::{
    ScriptCall, ScriptLogDto, ScriptLogLevel, ScriptOutput, ScriptRun,
};
use boa_engine::builtins::promise::PromiseState;
use boa_engine::{Context, JsError, JsNativeError, JsObject, JsValue, Source, js_string};
use domain::value_objects::RuleId;
use lru::LruCache;
use serde_json::{Value, json};

use super::bindings::{
    js_headers_are_form, request_from_js, request_to_js, response_from_js, response_to_js,
};
use super::shared_state::SharedState;

const PRELUDE: &str = include_str!("prelude.js");
/// Итераций одного цикла: бесконечный цикл обрывается ошибкой.
const LOOP_ITERATION_LIMIT: u64 = 10_000_000;
const RECURSION_LIMIT: usize = 512;
/// Скомпилированных скриптов на поток.
const COMPILED_CACHE: usize = 64;

pub struct Worker {
    context: Context,
    compiled: LruCache<(RuleId, u64), JsObject>,
    /// Предупреждения рантайма текущего вызова (в консоль).
    warnings: Vec<String>,
}

impl Worker {
    pub fn new() -> Result<Self, String> {
        let mut context = Context::builder().build().map_err(|e| e.to_string())?;
        let limits = context.runtime_limits_mut();
        limits.set_loop_iteration_limit(LOOP_ITERATION_LIMIT);
        limits.set_recursion_limit(RECURSION_LIMIT);
        super::addons::install(&mut context).map_err(|e| format!("script addons failed: {e}"))?;
        context
            .eval(Source::from_bytes(PRELUDE))
            .map_err(|e| format!("script prelude failed: {e}"))?;
        Ok(Self {
            context,
            compiled: LruCache::new(NonZeroUsize::new(COMPILED_CACHE).expect("non-zero cache")),
            warnings: Vec::new(),
        })
    }

    /// Выполнить вызов. Ошибка попадает и в `output`, и в консоль.
    pub fn run(&mut self, call: &ScriptCall, shared: &SharedState) -> ScriptRun {
        self.warnings.clear();
        let output = self.execute(call, shared);
        let mut logs = self.take_logs();
        logs.extend(self.warnings.drain(..).map(|message| ScriptLogDto {
            level: ScriptLogLevel::Warn,
            message,
        }));
        if let Err(error) = &output {
            logs.push(ScriptLogDto {
                level: ScriptLogLevel::Error,
                message: error.clone(),
            });
        }
        ScriptRun { output, logs }
    }

    fn execute(&mut self, call: &ScriptCall, shared: &SharedState) -> Result<ScriptOutput, String> {
        // `$sharedState` — до компиляции: его видит и код верхнего уровня.
        let before = shared.snapshot();
        self.set_global("$sharedState", &Value::Object(before.clone()))?;
        let handlers = self.compile(call)?;
        let name = if call.response.is_some() {
            "onResponse"
        } else {
            "onRequest"
        };
        let function = handlers
            .get(js_string!(name), &mut self.context)
            .map_err(|e| e.to_string())?;
        let Some(function) = function.as_callable() else {
            return Ok(ScriptOutput::NotDefined);
        };

        let context_arg = self.js_value(&json!({
            "scriptName": call.script.name,
            "ruleId": call.script.rule_id.to_string(),
        }))?;
        let url = JsValue::from(js_string!(call.request.url.as_str()));
        let request_json = request_to_js(&call.request);
        let request = self.js_value(&request_json)?;
        let response_json = call.response.as_ref().map(response_to_js);
        let response = match &response_json {
            Some(json) => Some(self.js_value(json)?),
            None => None,
        };
        // Объект фазы (его тело отслеживаем) и снимок тела до вызова: скрипт
        // меняет аргумент на месте.
        let (target, target_json) = match (&response, &response_json) {
            (Some(response), Some(json)) => (response.clone(), json),
            _ => (request.clone(), &request_json),
        };
        let body_before = self.stringify_property(&target, "body")?;
        let mut args = vec![context_arg, url, request];
        args.extend(response);

        let returned = function
            .call(&JsValue::undefined(), &args, &mut self.context)
            .and_then(|value| self.settle(value))
            .map_err(|e| e.to_string());
        self.merge_shared_state(shared, &before);
        let returned = returned?;

        // Скрипт мог изменить объект и не вернуть его — берём аргумент.
        let edited = if returned.is_object() {
            returned
        } else {
            target
        };
        let mut edited_json = self.checked_json(&edited)?;
        self.normalize_body(&edited, &mut edited_json, target_json, &body_before)?;
        Ok(match &call.response {
            Some(original) => ScriptOutput::Response(response_from_js(original, &edited_json)),
            None => ScriptOutput::Request(request_from_js(&call.request, &edited_json)),
        })
    }

    /// Тело после скрипта. Сравнение — по `JSON.stringify` в JS: перевод в
    /// serde_json превращает целые вне i32 в дробные, и нетронутое тело
    /// выглядело бы изменённым. Не изменилось — исходное значение (байты
    /// уйдут как были); изменён объект — его JS-сериализация (целые без
    /// `.0`); у формы объект остаётся объектом.
    fn normalize_body(
        &mut self,
        edited: &JsValue,
        edited_json: &mut Value,
        original_json: &Value,
        body_before: &Option<String>,
    ) -> Result<(), String> {
        let body_after = self.stringify_property(edited, "body")?;
        let Some(map) = edited_json.as_object_mut() else {
            return Ok(());
        };
        if body_after == *body_before {
            map.insert("body".into(), original_json["body"].clone());
        } else if map
            .get("body")
            .is_some_and(|b| b.is_object() || b.is_array())
            && !js_headers_are_form(&Value::Object(map.clone()))
            && let Some(text) = body_after
        {
            map.insert("body".into(), Value::String(text));
        }
        Ok(())
    }

    /// Влить `$sharedState` после вызова; не объект / не переводится в
    /// JSON — предупреждение в консоль, изменения не сохраняются.
    fn merge_shared_state(
        &mut self,
        shared: &SharedState,
        before: &serde_json::Map<String, Value>,
    ) {
        let state = self
            .context
            .global_object()
            .get(js_string!("$sharedState"), &mut self.context)
            .map_err(|e| e.to_string())
            .and_then(|value| self.checked_json(&value));
        match state {
            Ok(Value::Object(after)) => shared.merge(before, &after),
            Ok(_) => self
                .warnings
                .push("$sharedState must stay an object; changes dropped".into()),
            Err(error) => self
                .warnings
                .push(format!("$sharedState not saved: {error}")),
        }
    }

    /// Фабрика обработчиков скрипта (из кеша или после компиляции).
    fn compile(&mut self, call: &ScriptCall) -> Result<JsObject, String> {
        let mut hasher = DefaultHasher::new();
        call.script.source.hash(&mut hasher);
        let key = (call.script.rule_id, hasher.finish());
        if let Some(handlers) = self.compiled.get(&key) {
            return Ok(handlers.clone());
        }
        // Первая строка исходника — на одной строке с обёрткой: номера
        // строк в ошибках совпадают с редактором.
        let wrapped = format!(
            "(function () {{{}\n;return {{\
             onRequest: typeof onRequest === \"function\" ? onRequest : undefined,\
             onResponse: typeof onResponse === \"function\" ? onResponse : undefined }};\n}})()",
            call.script.source
        );
        let handlers = self
            .context
            .eval(Source::from_bytes(&wrapped))
            .map_err(|e| e.to_string())?
            .as_object()
            .ok_or_else(|| "script did not produce handlers".to_string())?;
        self.compiled.put(key, handlers.clone());
        Ok(handlers)
    }

    /// Результат обработчика: Promise (async-функция) — выполнить очередь
    /// заданий и взять итог. Таймеров нет, поэтому Promise, ждущий после
    /// пустой очереди, не разрешится никогда — ошибка, а не вечный цикл
    /// (`JsPromise::await_blocking` крутился бы без конца).
    fn settle(&mut self, value: JsValue) -> boa_engine::JsResult<JsValue> {
        self.context.run_jobs()?;
        let Some(promise) = value.as_promise() else {
            return Ok(value);
        };
        match promise.state() {
            PromiseState::Fulfilled(result) => Ok(result),
            PromiseState::Rejected(reason) => Err(JsError::from_opaque(reason)),
            PromiseState::Pending => Err(JsNativeError::error()
                .with_message("handler returned a promise that never settles")
                .into()),
        }
    }

    /// `JSON.stringify` свойства (с проверкой размера/глубины); `None` —
    /// значение не сериализуется (undefined, функция).
    fn stringify_property(
        &mut self,
        object: &JsValue,
        name: &str,
    ) -> Result<Option<String>, String> {
        let Some(object) = object.as_object() else {
            return Ok(None);
        };
        let value = object
            .get(js_string!(name), &mut self.context)
            .map_err(|e| e.to_string())?;
        let stringify = self
            .context
            .global_object()
            .get(js_string!("__crabtrapStringify"), &mut self.context)
            .ok()
            .and_then(|f| f.as_callable())
            .ok_or_else(|| "script prelude is missing".to_string())?;
        let text = stringify
            .call(&JsValue::undefined(), &[value], &mut self.context)
            .map_err(|e| format!("{name}: {e}"))?;
        if text.is_undefined() {
            return Ok(None);
        }
        let text = text
            .to_string(&mut self.context)
            .map_err(|e| e.to_string())?;
        Ok(Some(text.to_std_string_escaped()))
    }

    /// Перевод в JSON после проверки размера/глубины в JS (обход boa
    /// рекурсивный — глубокое значение переполнило бы стек потока).
    fn checked_json(&mut self, value: &JsValue) -> Result<Value, String> {
        let check = self
            .context
            .global_object()
            .get(js_string!("__crabtrapCheck"), &mut self.context)
            .ok()
            .and_then(|f| f.as_callable())
            .ok_or_else(|| "script prelude is missing".to_string())?;
        check
            .call(
                &JsValue::undefined(),
                std::slice::from_ref(value),
                &mut self.context,
            )
            .map_err(|e| e.to_string())?;
        self.json_value(value)
    }

    fn take_logs(&mut self) -> Vec<ScriptLogDto> {
        let taken = self
            .context
            .global_object()
            .get(js_string!("__crabtrapTakeLogs"), &mut self.context)
            .ok()
            .and_then(|f| f.as_callable())
            .and_then(|f| f.call(&JsValue::undefined(), &[], &mut self.context).ok());
        let Some(Value::Array(entries)) = taken.and_then(|v| self.checked_json(&v).ok()) else {
            return Vec::new();
        };
        entries
            .iter()
            .filter_map(|entry| {
                let level = match entry.get(0)?.as_str()? {
                    "debug" => ScriptLogLevel::Debug,
                    "info" => ScriptLogLevel::Info,
                    "warn" => ScriptLogLevel::Warn,
                    "error" => ScriptLogLevel::Error,
                    _ => ScriptLogLevel::Log,
                };
                let message = entry.get(1)?.as_str()?.to_string();
                Some(ScriptLogDto { level, message })
            })
            .collect()
    }

    fn set_global(&mut self, name: &str, value: &Value) -> Result<(), String> {
        let value = self.js_value(value)?;
        self.context
            .global_object()
            .set(js_string!(name), value, false, &mut self.context)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn js_value(&mut self, value: &Value) -> Result<JsValue, String> {
        JsValue::from_json(value, &mut self.context).map_err(|e| e.to_string())
    }

    fn json_value(&mut self, value: &JsValue) -> Result<Value, String> {
        value
            .to_json(&mut self.context)
            .map(|v| v.unwrap_or(Value::Null))
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
