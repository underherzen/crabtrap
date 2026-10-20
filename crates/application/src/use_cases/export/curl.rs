//! «Copy as cURL»: команда, повторяющая запрос в терминале (zsh/bash).
//! Значения — в одинарных кавычках. Текстовое тело — `--data-raw` (curl не
//! читает файл по `@…`); бинарное — через `printf` с восьмеричными
//! escape-последовательностями в stdin (`--data-binary @-`), чтобы байты,
//! включая NUL, дошли без внешнего файла.

use domain::value_objects::http_vo::HeaderEntry;

/// Заголовки, которые curl выставляет сам: копия из захвата сломала бы
/// запрос (неверная длина после правки тела, chunked без chunked-тела).
const CURL_MANAGED_HEADERS: [&str; 3] = ["content-length", "transfer-encoding", "connection"];

/// Команда curl для запроса.
pub fn curl_command(
    method: &str,
    url: &str,
    headers: &[HeaderEntry],
    body: Option<&[u8]>,
) -> String {
    let body = body.filter(|b| !b.is_empty());
    let text_body = body.map(std::str::from_utf8);
    let mut parts = vec!["curl".to_string()];
    match (method.to_ascii_uppercase().as_str(), body.is_some()) {
        ("GET", false) => {}
        ("HEAD", _) => parts.push("--head".into()),
        (method, _) => parts.push(format!("-X {}", shell_quote(method))),
    }
    // `[]{}` в URL (`?a[]=1`) curl иначе разворачивает как шаблон.
    if url.contains(['[', ']', '{', '}']) {
        parts.push("--globoff".into());
    }
    parts.push(shell_quote(url));
    parts.extend(
        headers
            .iter()
            .filter(|h| !is_managed(h) && !is_implied_host(h, url))
            .map(|h| format!("-H {}", shell_quote(&header_arg(h)))),
    );
    match text_body {
        Some(Ok(text)) => {
            parts.push(format!("--data-raw {}", shell_quote(text)));
            parts.join(" \\\n  ")
        }
        Some(Err(_)) => {
            parts.push("--data-binary @-".into());
            let bytes = body.unwrap_or_default();
            format!("printf {} | {}", printf_octal(bytes), parts.join(" \\\n  "))
        }
        None => parts.join(" \\\n  "),
    }
}

fn is_managed(header: &HeaderEntry) -> bool {
    CURL_MANAGED_HEADERS
        .iter()
        .any(|managed| managed.eq_ignore_ascii_case(&header.name))
}

/// `Host`, совпадающий с хостом URL, curl выставит сам; другой (виртуальный
/// хост, обращение по IP) — сохраняется.
fn is_implied_host(header: &HeaderEntry, url: &str) -> bool {
    header.name.eq_ignore_ascii_case("host")
        && url::Url::parse(url).is_ok_and(|u| {
            let authority = match (u.host_str(), u.port()) {
                (Some(host), Some(port)) => format!("{host}:{port}"),
                (Some(host), None) => host.to_string(),
                (None, _) => return false,
            };
            let value = header.value.trim().to_ascii_lowercase();
            let default_port = u.port_or_known_default().map(|p| format!(":{p}"));
            value == authority
                || default_port.is_some_and(|port| value == format!("{authority}{port}"))
        })
}

/// `Name: value`; пустое значение — `Name;` (иначе curl заголовок убирает).
fn header_arg(header: &HeaderEntry) -> String {
    match header.value.is_empty() {
        true => format!("{};", header.name),
        false => format!("{}: {}", header.name, header.value),
    }
}

/// `'…'` с `'` → `'\''` — безопасно для любого текста в sh.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Формат `printf` (в кавычках), печатающий ровно `bytes`: печатный ASCII
/// как есть, `%` и `\` экранированы, остальное — `\ooo` (POSIX printf).
fn printf_octal(bytes: &[u8]) -> String {
    let format: String = bytes
        .iter()
        .map(|&b| match b {
            b'%' => "%%".to_string(),
            b'\\' => r"\\".to_string(),
            0x20..=0x7e => (b as char).to_string(),
            _ => format!(r"\{b:03o}"),
        })
        .collect();
    shell_quote(&format)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(list: &[(&str, &str)]) -> Vec<HeaderEntry> {
        list.iter().map(|(n, v)| HeaderEntry::new(*n, *v)).collect()
    }

    #[test]
    fn get_without_body_has_no_method_flag() {
        let cmd = curl_command(
            "GET",
            "https://api.test/v1/users?page=2",
            &headers(&[("Accept", "application/json"), ("Host", "api.test")]),
            None,
        );
        assert_eq!(
            cmd,
            "curl \\\n  'https://api.test/v1/users?page=2' \\\n  -H 'Accept: application/json'"
        );
    }

    #[test]
    fn post_with_json_body_and_quotes() {
        let cmd = curl_command(
            "post",
            "https://api.test/items",
            &headers(&[
                ("Content-Type", "application/json"),
                ("Content-Length", "15"),
                ("X-Note", "it's"),
            ]),
            Some(br#"{"name":"O'Neil"}"#),
        );
        assert!(
            cmd.starts_with("curl \\\n  -X 'POST' \\\n  'https://api.test/items'"),
            "{cmd}"
        );
        assert!(cmd.contains(r"-H 'X-Note: it'\''s'"), "{cmd}");
        assert!(!cmd.contains("Content-Length"), "{cmd}");
        assert!(
            cmd.ends_with(r#"--data-raw '{"name":"O'\''Neil"}'"#),
            "{cmd}"
        );
    }

    #[test]
    fn body_starting_with_at_is_never_a_file_reference() {
        let cmd = curl_command("POST", "http://a.test/x", &[], Some(b"@/etc/passwd"));
        assert!(cmd.ends_with("--data-raw '@/etc/passwd'"), "{cmd}");
        assert!(!cmd.contains("--data-binary '@"), "{cmd}");
    }

    #[test]
    fn binary_body_is_piped_through_printf_with_octal_escapes() {
        let cmd = curl_command(
            "PUT",
            "http://a.test/x",
            &[],
            Some(&[0x00, b'a', 0xff, b'\'', b'\\', b'%']),
        );
        assert!(
            cmd.starts_with(r"printf '\000a\377'\''\\%%' | curl"),
            "{cmd}"
        );
        assert!(cmd.ends_with("--data-binary @-"), "{cmd}");
    }

    #[test]
    fn host_is_kept_only_when_it_differs_from_the_url() {
        let virtual_host = curl_command(
            "GET",
            "http://10.0.0.5/",
            &headers(&[("Host", "staging.example.com")]),
            None,
        );
        assert!(
            virtual_host.contains("-H 'Host: staging.example.com'"),
            "{virtual_host}"
        );
        let same = curl_command(
            "GET",
            "https://api.test:8443/",
            &headers(&[("Host", "API.test:8443")]),
            None,
        );
        assert!(!same.contains("Host"), "{same}");
        let default_port = curl_command(
            "GET",
            "https://api.test/",
            &headers(&[("Host", "api.test:443")]),
            None,
        );
        assert!(!default_port.contains("Host"), "{default_port}");
    }

    #[test]
    fn brackets_disable_globbing_and_empty_headers_survive() {
        let cmd = curl_command(
            "GET",
            "https://a.test/?filter[id]=3",
            &headers(&[("X-Empty", "")]),
            None,
        );
        assert!(cmd.contains("--globoff"), "{cmd}");
        assert!(cmd.contains("-H 'X-Empty;'"), "{cmd}");
    }

    #[test]
    fn head_and_empty_body() {
        assert_eq!(
            curl_command("HEAD", "http://a.test/", &[], None),
            "curl \\\n  --head \\\n  'http://a.test/'"
        );
        assert_eq!(
            curl_command("DELETE", "http://a.test/1", &[], Some(b"")),
            "curl \\\n  -X 'DELETE' \\\n  'http://a.test/1'"
        );
    }
}
