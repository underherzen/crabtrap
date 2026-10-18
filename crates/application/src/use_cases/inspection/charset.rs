//! Кодировки текстовых тел: разбор Content-Type и декодирование charset.
//!
//! Общий код инспектора (вкладка Body) и поиска по телам: оба должны
//! видеть один и тот же текст (windows-1251, UTF-16 по BOM и т.п.).

use encoding_rs::Encoding;

/// Разбор Content-Type: `(mime нижним регистром, charset-параметр)`.
/// Параметры после `;` отрезаются; `charset=` ищется case-insensitive,
/// кавычки значения снимаются.
pub fn split_content_type(ct: &str) -> (String, Option<String>) {
    let mut parts = ct.split(';');
    let mime = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    let charset = parts
        .filter_map(|p| {
            // `get` вместо среза: 8-й байт может попасть внутрь UTF-8
            // символа во враждебном заголовке — срез бы паниковал.
            let p = p.trim();
            p.get(..8)
                .filter(|key| key.eq_ignore_ascii_case("charset="))
                .and_then(|_| p.get(8..))
        })
        .next()
        .map(|v| v.trim().trim_matches('"').to_ascii_lowercase());
    (mime, charset)
}

/// Кодировка для декодирования: BOM сильнее всего (UTF-8/UTF-16LE/BE),
/// затем `charset` из Content-Type (encoding_rs понимает все алиасы IANA,
/// включая `cp1251`), по умолчанию — UTF-8.
pub fn sniff_charset(bytes: &[u8], charset_param: Option<&str>) -> &'static Encoding {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return encoding_rs::UTF_8;
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return encoding_rs::UTF_16LE;
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return encoding_rs::UTF_16BE;
    }
    charset_param
        .and_then(|label| Encoding::for_label(label.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8)
}

/// Декодирование в UTF-8: BOM-снифф + кодировка из параметра, lossy
/// (encoding_rs заменяет невалидные последовательности U+FFFD).
/// Возвращает (текст, кодировка, были_ли_замены).
pub fn decode_charset(
    bytes: &[u8],
    charset_param: Option<&str>,
) -> (String, &'static Encoding, bool) {
    let encoding = sniff_charset(bytes, charset_param);
    let (text, used, had_errors) = encoding.decode(bytes);
    (text.into_owned(), used, had_errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- split_content_type ---

    #[test]
    fn content_type_mime_and_charset() {
        assert_eq!(
            split_content_type("application/json; charset=utf-8"),
            ("application/json".into(), Some("utf-8".into()))
        );
        assert_eq!(
            split_content_type("Text/HTML ; Charset=\"Windows-1251\""),
            ("text/html".into(), Some("windows-1251".into()))
        );
        assert_eq!(split_content_type("image/png"), ("image/png".into(), None));
        assert_eq!(split_content_type(""), (String::new(), None));
    }

    #[test]
    fn split_content_type_survives_multibyte_param() {
        // 9-байтовый параметр, 8-й байт — середина «é»: не паника.
        assert_eq!(
            split_content_type("text/html; abcdefgé"),
            ("text/html".into(), None)
        );
        assert_eq!(
            split_content_type("text/plain; ЧАРСЕТ=x; charset=utf-8"),
            ("text/plain".into(), Some("utf-8".into()))
        );
    }

    // --- charset sniff / decode ---

    #[test]
    fn charset_bom_wins() {
        let utf8_bom: &[u8] = &[0xEF, 0xBB, 0xBF, b'a'];
        assert_eq!(sniff_charset(utf8_bom, Some("koi8-r")), encoding_rs::UTF_8);
        let le: &[u8] = &[0xFF, 0xFE, 0x31, 0x04]; // "я" в UTF-16LE
        assert_eq!(sniff_charset(le, None), encoding_rs::UTF_16LE);
        let be: &[u8] = &[0xFE, 0xFF, 0x04, 0x31];
        assert_eq!(sniff_charset(be, None), encoding_rs::UTF_16BE);
    }

    #[test]
    fn charset_param_and_default() {
        assert_eq!(
            sniff_charset(b"hello", Some("cp1251")),
            encoding_rs::WINDOWS_1251
        );
        assert_eq!(
            sniff_charset(b"hello", Some("bogus-label")),
            encoding_rs::UTF_8
        );
        assert_eq!(sniff_charset(b"hello", None), encoding_rs::UTF_8);
    }

    #[test]
    fn decode_charset_windows1251_and_utf16le() {
        // «Привет» в windows-1251.
        let cp1251: &[u8] = &[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
        let (text, enc, errors) = decode_charset(cp1251, Some("windows-1251"));
        assert_eq!(text, "Привет");
        assert_eq!(enc, encoding_rs::WINDOWS_1251);
        assert!(!errors);

        // UTF-16LE с BOM (BOM-снифф сильнее параметра).
        let mut le = vec![0xFF, 0xFE];
        le.extend("A".encode_utf16().flat_map(u16::to_le_bytes));
        let (text, enc, _) = decode_charset(&le, Some("utf-8"));
        assert_eq!(text, "A");
        assert_eq!(enc, encoding_rs::UTF_16LE);
    }

    #[test]
    fn decode_charset_lossy() {
        // Невалидный UTF-8 → U+FFFD, had_errors = true.
        let (text, _, errors) = decode_charset(&[0x61, 0xFF, 0x62], None);
        assert_eq!(text, "a\u{FFFD}b");
        assert!(errors);
    }
}
