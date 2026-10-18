//! Query-строка после правки `request.queries` скриптом: нетронутые
//! параметры остаются байт-в-байт (кодировка, порядок, `?flag` без `=`),
//! кодируются только изменённые и новые.

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde_json::{Map, Value};

use super::bindings::scalar_list;

/// Что кодировать в ключе/значении query (как `encodeURIComponent`, но
/// оставляя безопасные для query `/`, `:`, `@`, `,`).
const QUERY_COMPONENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b'=')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b']')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

fn encode(text: &str) -> String {
    utf8_percent_encode(text, QUERY_COMPONENT).to_string()
}

fn decoded_key(segment: &str) -> String {
    let raw = segment.split_once('=').map_or(segment, |(key, _)| key);
    url::form_urlencoded::parse(raw.as_bytes())
        .next()
        .map(|(key, _)| key.into_owned())
        .unwrap_or_default()
}

fn encoded_pairs(key: &str, value: &Value) -> Vec<String> {
    scalar_list(value)
        .into_iter()
        .map(|item| format!("{}={}", encode(key), encode(&item)))
        .collect()
}

/// Новая query (`None` — без query). `before` — объект `queries`, который
/// получил скрипт, `after` — что он вернул (не объект — правка игнорируется).
pub fn rebuild_query(
    original: Option<&str>,
    before: &Map<String, Value>,
    after: &Value,
) -> Option<String> {
    let Value::Object(after) = after else {
        return original.map(str::to_string);
    };
    let mut segments: Vec<String> = Vec::new();
    let mut emitted: Vec<String> = Vec::new();
    for segment in original
        .unwrap_or_default()
        .split('&')
        .filter(|s| !s.is_empty())
    {
        let key = decoded_key(segment);
        match after.get(&key) {
            // Удалён скриптом.
            None => {}
            Some(value) if before.get(&key) == Some(value) => segments.push(segment.to_string()),
            // Изменён: новые значения на месте первого вхождения.
            Some(value) if !emitted.contains(&key) => {
                segments.extend(encoded_pairs(&key, value));
                emitted.push(key);
            }
            Some(_) => {}
        }
    }
    for (key, value) in after.iter().filter(|(key, _)| !before.contains_key(*key)) {
        segments.extend(encoded_pairs(key, value));
    }
    (!segments.is_empty()).then(|| segments.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn untouched_params_keep_raw_bytes_and_order() {
        let original = "q=a%20b&flag&t=%FF&page=1";
        let before = map(json!({"q": "a b", "flag": "", "t": "\u{fffd}", "page": "1"}));
        let mut after = before.clone();
        after.insert("page".into(), json!("2"));
        let query = rebuild_query(Some(original), &before, &Value::Object(after));
        assert_eq!(query.as_deref(), Some("q=a%20b&flag&t=%FF&page=2"));
    }

    #[test]
    fn deleted_and_added_params() {
        let before = map(json!({"a": "1", "b": "2"}));
        let after = json!({"b": "2", "c": "x y/z"});
        let query = rebuild_query(Some("a=1&b=2"), &before, &after);
        assert_eq!(query.as_deref(), Some("b=2&c=x%20y/z"));
        assert_eq!(
            rebuild_query(Some("a=1"), &map(json!({"a": "1"})), &json!({})),
            None
        );
    }

    #[test]
    fn repeated_key_changed_in_place() {
        let before = map(json!({"a": ["1", "3"], "b": "2"}));
        let after = json!({"a": ["9"], "b": "2"});
        assert_eq!(
            rebuild_query(Some("a=1&b=2&a=3"), &before, &after).as_deref(),
            Some("a=9&b=2")
        );
    }

    #[test]
    fn non_object_is_ignored() {
        let before = map(json!({"a": "1"}));
        assert_eq!(
            rebuild_query(Some("a=1"), &before, &json!("a=2")).as_deref(),
            Some("a=1")
        );
    }
}
