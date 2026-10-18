//! Парсер текстового фильтра таблицы трафика: строка тулбара → `FilterQuery`.
//!
//! Размещение: парсер переводит UI-синтаксис в доменный VO. Домен остаётся
//! чистым (не знает о текстовой грамматике), а `dto/capture.rs` — общий
//! baseline, который в Phase 5 параллельно меняют другие агенты; поэтому
//! парсер живёт отдельным модулем рядом с поисковым use case (та же зона
//! ответственности: текст запроса → сужение вида таблицы).
//!
//! # Грамматика
//!
//! Токены разделяются пробелами, комбинируются через AND:
//!
//! - `status:200` — точный код; `status:500-599` — диапазон включительно;
//!   `status:4xx` — класс (`2xx`/`3xx`/`4xx`/`5xx`);
//!   `status:200,301,5xx` — список через запятую; повторные `status:`-токены
//!   объединяются (OR внутри поля);
//! - `host:*.example.com` — glob хоста (`*`/`?`, матчинг case-insensitive);
//! - `method:POST` — метод (case-insensitive);
//! - `type:json` — подстрока Content-Type ответа (case-insensitive);
//! - `re:<regex>` — регулярка по полному URL (`scheme://host/path`);
//!   последний `re:` побеждает;
//! - прочее (`users`, `foo:bar`, битый `status:abc`) — freetext-токен:
//!   подстрока в host/path. Несколько freetext-токенов тоже AND-ятся
//!   (хранятся в `FilterQuery::text` через пробел; view-слой [`store`]
//!   сплитит по пробелу и требует каждый токен).
//!
//! [`store`]: crabtrap app: `ui::traffic::store::row_matches`
//!
//! Пустая/нераспознанная строка → `FilterQuery::default()` (всё видно).

use domain::value_objects::filter_query::FilterQuery;

/// Верхний порог расширения диапазона статусов (защита от `status:0-65535`
/// в однострочном инпуте — 100 кодов на токен достаточно).
const MAX_STATUS_RANGE_SPAN: u16 = 1000;

/// Распарсить строку фильтра тулбара в `FilterQuery`.
pub fn parse_filter_query(input: &str) -> FilterQuery {
    let mut query = FilterQuery::default();
    let mut free: Vec<String> = Vec::new();

    for token in input.split_whitespace() {
        match token.split_once(':') {
            Some((key, value)) if !key.is_empty() && !value.is_empty() => {
                match key.to_ascii_lowercase().as_str() {
                    "status" => match parse_status_spec(value) {
                        Some(codes) => query.status_codes.extend(codes),
                        // Битый статус-токен — обычный freetext (поиск по
                        // строке «status:abc» в host/path).
                        None => free.push(token.to_string()),
                    },
                    "host" => query.hosts.push(value.to_string()),
                    "method" => query.methods.push(value.to_string()),
                    "type" => query.content_types.push(value.to_string()),
                    "re" | "regex" => query.regex = Some(value.to_string()),
                    _ => free.push(token.to_string()),
                }
            }
            // `host:` / `:value` / просто `value` — freetext.
            _ => free.push(token.to_string()),
        }
    }

    if !free.is_empty() {
        query.text = Some(free.join(" "));
    }
    query
}

/// `200` | `500-599` | `4xx` | `200,301,5xx` → список кодов.
/// None — непарсимый spec (вызывающий сделает freetext).
fn parse_status_spec(spec: &str) -> Option<Vec<u16>> {
    let mut codes = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        if let Some(class) = part.strip_suffix("xx").or_else(|| part.strip_suffix("XX")) {
            let digit = class.parse::<u16>().ok()?;
            let base = digit.checked_mul(100)?;
            if digit > 9 || base == 0 {
                return None;
            }
            codes.extend(base..base + 100);
        } else if let Some((lo, hi)) = part.split_once('-') {
            let lo: u16 = lo.trim().parse().ok()?;
            let hi: u16 = hi.trim().parse().ok()?;
            if hi < lo || hi.saturating_sub(lo) >= MAX_STATUS_RANGE_SPAN {
                return None;
            }
            codes.extend(lo..=hi);
        } else {
            codes.push(part.parse::<u16>().ok()?);
        }
    }
    if codes.is_empty() {
        return None;
    }
    Some(codes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_token_lands_in_regex_field() {
        let q = parse_filter_query("re:^https://api\\. host:*.x.com");
        assert_eq!(q.regex.as_deref(), Some("^https://api\\."));
        assert_eq!(q.hosts, vec!["*.x.com".to_string()]);
        assert!(q.text.is_none());
    }

    #[test]
    fn empty_input_yields_default() {
        assert_eq!(parse_filter_query(""), FilterQuery::default());
        assert_eq!(parse_filter_query("   "), FilterQuery::default());
        assert!(parse_filter_query("").is_empty());
    }

    #[test]
    fn freetext_single_token_lands_in_text() {
        let q = parse_filter_query("users");
        assert_eq!(q.text.as_deref(), Some("users"));
        assert!(q.hosts.is_empty() && q.methods.is_empty());
    }

    #[test]
    fn freetext_tokens_are_and_combined_in_order() {
        let q = parse_filter_query("api token");
        assert_eq!(q.text.as_deref(), Some("api token"));
    }

    #[test]
    fn status_exact_code() {
        let q = parse_filter_query("status:200");
        assert_eq!(q.status_codes, vec![200]);
        assert!(q.text.is_none());
    }

    #[test]
    fn status_inclusive_range() {
        let q = parse_filter_query("status:500-599");
        assert_eq!(q.status_codes.first(), Some(&500));
        assert_eq!(q.status_codes.last(), Some(&599));
        assert_eq!(q.status_codes.len(), 100);
    }

    #[test]
    fn status_single_step_range() {
        let q = parse_filter_query("status:404-404");
        assert_eq!(q.status_codes, vec![404]);
    }

    #[test]
    fn status_class_xx() {
        let q = parse_filter_query("status:4xx");
        assert_eq!(q.status_codes.len(), 100);
        assert!(q.status_codes.contains(&400) && q.status_codes.contains(&499));
        assert!(!q.status_codes.contains(&500));
    }

    #[test]
    fn status_comma_list_mixes_forms() {
        let q = parse_filter_query("status:200,301,5xx");
        assert!(q.status_codes.contains(&200));
        assert!(q.status_codes.contains(&301));
        assert!(q.status_codes.contains(&503));
        assert_eq!(q.status_codes.len(), 102);
    }

    #[test]
    fn status_repeated_tokens_merge() {
        let q = parse_filter_query("status:200 status:404");
        assert_eq!(q.status_codes, vec![200, 404]);
    }

    #[test]
    fn status_malformed_falls_back_to_freetext() {
        let q = parse_filter_query("status:abc");
        assert!(q.status_codes.is_empty());
        assert_eq!(q.text.as_deref(), Some("status:abc"));

        let q = parse_filter_query("status:99999");
        assert!(q.status_codes.is_empty());
        assert_eq!(q.text.as_deref(), Some("status:99999"));

        // Диапазон наоборот / шире cap — тоже freetext.
        let q = parse_filter_query("status:500-100");
        assert_eq!(q.text.as_deref(), Some("status:500-100"));
        let q = parse_filter_query("status:0-65535");
        assert_eq!(q.text.as_deref(), Some("status:0-65535"));

        // Пустое значение после двоеточия — freetext-токен целиком.
        let q = parse_filter_query("status:");
        assert_eq!(q.text.as_deref(), Some("status:"));

        // Нестандартный класс.
        let q = parse_filter_query("status:99xx");
        assert_eq!(q.text.as_deref(), Some("status:99xx"));
    }

    #[test]
    fn host_glob_token() {
        let q = parse_filter_query("host:*.example.com");
        assert_eq!(q.hosts, vec!["*.example.com"]);
        assert!(q.text.is_none());

        let q = parse_filter_query("host:api.example.com");
        assert_eq!(q.hosts, vec!["api.example.com"]);
    }

    #[test]
    fn method_token() {
        let q = parse_filter_query("method:POST");
        assert_eq!(q.methods, vec!["POST"]);
    }

    #[test]
    fn type_token_is_content_type_substring() {
        let q = parse_filter_query("type:json");
        assert_eq!(q.content_types, vec!["json"]);
    }

    #[test]
    fn keys_are_case_insensitive_values_kept() {
        let q = parse_filter_query("STATUS:200 Host:*.example.com METHOD:post TYPE:JSON");
        assert_eq!(q.status_codes, vec![200]);
        assert_eq!(q.hosts, vec!["*.example.com"]);
        assert_eq!(q.methods, vec!["post"]);
        assert_eq!(q.content_types, vec!["JSON"]);
    }

    #[test]
    fn unknown_prefixed_token_is_freetext() {
        let q = parse_filter_query("foo:bar");
        assert_eq!(q.text.as_deref(), Some("foo:bar"));
        assert!(q.hosts.is_empty() && q.methods.is_empty() && q.content_types.is_empty());
    }

    #[test]
    fn multiple_tokens_and_combined() {
        let q = parse_filter_query("status:4xx host:*.example.com method:POST type:json users");
        assert_eq!(q.status_codes.len(), 100);
        assert_eq!(q.hosts, vec!["*.example.com"]);
        assert_eq!(q.methods, vec!["POST"]);
        assert_eq!(q.content_types, vec!["json"]);
        assert_eq!(q.text.as_deref(), Some("users"));
    }

    #[test]
    fn colon_only_and_leading_colon_are_freetext() {
        assert_eq!(parse_filter_query(":").text.as_deref(), Some(":"));
        assert_eq!(parse_filter_query(":8080").text.as_deref(), Some(":8080"));
    }
}
