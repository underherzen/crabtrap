//! `$sharedState` — объект, общий для всех скриптов (как в Proxyman):
//! токен из одного ответа подставляется в следующие запросы.
//!
//! Каждый поток пула держит свой JS-контекст, поэтому состояние живёт
//! здесь: перед вызовом скрипт получает снимок, после — в общее состояние
//! вливаются только изменённые им ключи, рекурсивно по вложенным объектам
//! (параллельный скрипт, правивший другие ключи, не затирается). Счётчик
//! `n++` из двух параллельных скриптов может потерять инкремент — общее
//! состояние не транзакционное.

use std::sync::Mutex;

use serde_json::{Map, Value};

#[derive(Debug, Default)]
pub struct SharedState {
    values: Mutex<Map<String, Value>>,
}

impl SharedState {
    pub fn snapshot(&self) -> Map<String, Value> {
        self.values.lock().expect("shared state lock").clone()
    }

    /// Влить изменения скрипта: `before` — снимок, который он получил,
    /// `after` — что осталось после вызова. Удалённые ключи удаляются.
    pub fn merge(&self, before: &Map<String, Value>, after: &Map<String, Value>) {
        let mut values = self.values.lock().expect("shared state lock");
        merge_map(&mut values, before, after);
    }

    pub fn clear(&self) {
        self.values.lock().expect("shared state lock").clear();
    }
}

/// Изменения `before → after` поверх `target`: вложенные объекты —
/// по ключам, остальное — заменой.
fn merge_map(
    target: &mut Map<String, Value>,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) {
    for (key, value) in after {
        let previous = before.get(key);
        if previous == Some(value) {
            continue;
        }
        match (previous, value, target.get_mut(key)) {
            (Some(Value::Object(old)), Value::Object(new), Some(Value::Object(current))) => {
                merge_map(current, old, new);
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
    for key in before.keys().filter(|key| !after.contains_key(*key)) {
        target.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn only_changed_keys_are_merged() {
        let state = SharedState::default();
        state.merge(&Map::new(), &map(json!({"token": "a", "n": 1})));
        let seen_by_first = state.snapshot();
        let seen_by_second = state.snapshot();
        state.merge(&seen_by_first, &map(json!({"token": "b", "n": 1})));
        state.merge(&seen_by_second, &map(json!({"token": "a", "n": 2})));
        assert_eq!(state.snapshot(), map(json!({"token": "b", "n": 2})));
    }

    #[test]
    fn nested_objects_merge_per_key() {
        let state = SharedState::default();
        state.merge(&Map::new(), &map(json!({"obj": {"a": 1, "b": 1}})));
        let first = state.snapshot();
        let second = state.snapshot();
        state.merge(&first, &map(json!({"obj": {"a": 2, "b": 1}})));
        state.merge(&second, &map(json!({"obj": {"a": 1, "b": 3}})));
        assert_eq!(state.snapshot(), map(json!({"obj": {"a": 2, "b": 3}})));
    }

    #[test]
    fn deleted_keys_are_removed() {
        let state = SharedState::default();
        state.merge(&Map::new(), &map(json!({"a": 1, "b": 2})));
        let before = state.snapshot();
        state.merge(&before, &map(json!({"b": 2})));
        assert_eq!(state.snapshot(), map(json!({"b": 2})));
    }
}
