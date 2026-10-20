//! Скомпилированные шаблоны правил: компиляция — один раз на шаблон, а не
//! на каждое сообщение. Чтение — под RwLock (горячий путь), компиляция —
//! вне блокировки; переполненный кеш очищается целиком.

use std::collections::HashMap;
use std::sync::{OnceLock, PoisonError, RwLock};

/// Сколько разных шаблонов держать; при переполнении кеш очищается.
const CACHE_LIMIT: usize = 1024;

type Cache = RwLock<HashMap<String, Option<regex::Regex>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Regex из кеша по ключу `key`; нет — собрать `build` и запомнить
/// (в том числе неудачу — `None`).
fn cached(key: &str, build: impl FnOnce() -> Option<regex::Regex>) -> Option<regex::Regex> {
    if let Some(found) = cache()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(key)
    {
        return found.clone();
    }
    let built = build();
    let mut cache = cache().write().unwrap_or_else(PoisonError::into_inner);
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key.to_string(), built.clone());
    built
}

/// Regex по шаблону (`None` — шаблон не компилируется).
pub fn compiled(pattern: &str) -> Option<regex::Regex> {
    cached(pattern, || regex::Regex::new(pattern).ok())
}

/// Regex, совпадающий только со всей строкой (как в Proxyman: частичное
/// совпадение — не совпадение); без `case_sensitive` — без учёта регистра.
/// `None` — шаблон некорректен сам по себе или ломает обёртку.
pub fn whole_match(pattern: &str, case_sensitive: bool) -> Option<regex::Regex> {
    let wrapped = whole_string_pattern(pattern, case_sensitive);
    cached(&wrapped, || whole_match_check(pattern, case_sensitive).ok())
}

/// Проверить шаблон для [`whole_match`]: он компилируется сам по себе (иначе
/// `a)|(b` вывернул бы якоря наизнанку) и внутри якорей (`(?x)` с
/// комментарием `#` съел бы закрывающую скобку).
pub fn whole_match_check(
    pattern: &str,
    case_sensitive: bool,
) -> Result<regex::Regex, regex::Error> {
    regex::Regex::new(pattern)?;
    regex::Regex::new(&whole_string_pattern(pattern, case_sensitive))
}

fn whole_string_pattern(pattern: &str, case_sensitive: bool) -> String {
    let flags = if case_sensitive { "" } else { "(?i)" };
    format!("{flags}^(?:{pattern})$")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_pattern_is_compiled_once_and_bad_one_is_none() {
        let a = compiled(r"^a+$").expect("valid");
        assert!(a.is_match("aaa"));
        assert!(compiled(r"^a+$").is_some());
        assert!(compiled("(unclosed").is_none());
    }

    #[test]
    fn whole_match_anchors_alternation() {
        let re = whole_match("a|b", true).expect("valid");
        assert!(re.is_match("a"));
        assert!(!re.is_match("ab"), "alternation stays inside the anchors");
        assert!(whole_match("ABC", false).expect("valid").is_match("abc"));
        assert!(!whole_match("ABC", true).expect("valid").is_match("abc"));
    }

    #[test]
    fn patterns_that_break_the_anchors_are_rejected() {
        assert!(whole_match_check("a)|(b", false).is_err());
        assert!(whole_match("a)|(b", false).is_none());
        assert!(whole_match_check("(?x)a # trailing comment", false).is_err());
        assert!(whole_match_check(r"https://a\.test/.*", false).is_ok());
    }
}
