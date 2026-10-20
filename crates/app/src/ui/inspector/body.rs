//! Вкладка «Body»: pretty-view тел обмена (Phase 5, агент D).
//!
//! Конвейер: `BodyRepository::get` → Content-Encoding (gzip/br/deflate,
//! `ctx.decoder`) → charset/BOM-детект (encoding_rs) → рендер по
//! Content-Type: JSON-дерево (коллапсируемое, виртуализованное
//! `gpui_kit::base::Tree`), image preview (image-крейт, downscale ≤2048px),
//! pretty XML/HTML, form-urlencoded таблица, monospace-текст,
//! binary-детект с hex-превью. Всё тяжёлое (чтение/декод/парсинг/картинки)
//! — вне UI-потока через [`crate::ui::async_bridge::spawn_ui_task`];
//! вкладка держит render-ready состояние per (обмен, направление) в
//! глобальном кэше слабых ссылок на Entity — RenderOnce пересоздаётся
//! каждый рендер, состояние обязано переживать.
//!
//! Гварды: >10 МиБ (decoded) — pretty-виды отказываются (Hex возьмёт);
//! image >20 МиБ — отказ; JSON-дерево — только ≤2 МиБ и ≤5000 узлов
//! (иначе pretty-текст); текст-рендер урезается до 2 МиБ.

use std::io::Cursor;
use std::sync::Arc;

use application::dto::capture::HeaderEntryDto;
use application::dto::inspection::ExchangeDetailDto;
pub use application::use_cases::inspection::{decode_charset, split_content_type};
use domain::entities::BodyRef;
use encoding_rs::Encoding;
use gpui_kit::StatefulInteractiveElement as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Context, Entity, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px,
};
use gpui_kit::{AppContext as _, InteractiveElement as _};

use super::body_text;
use super::{ExchangeContext, Side, tab_state};
use crate::ui::async_bridge::spawn_ui_task;
use crate::ui::theme::{Palette, ToneName, glass};

// ---------------------------------------------------------------------------
// Константы-гварды
// ---------------------------------------------------------------------------

/// Декодированное тело больше — pretty-виды отключаются (Hex-вкладка
/// листает такие постранично).
pub const PRETTY_LIMIT: usize = 10 * 1024 * 1024;
/// Картинка больше — даже не пытаемся декодировать.
pub const IMAGE_LIMIT: usize = 20 * 1024 * 1024;
/// JSON-дерево рендерим только при «данные ≤ 2 МиБ И узлов ≤ 5000»,
/// иначе — pretty-текст (to_string_pretty).
pub const JSON_TREE_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const JSON_TREE_MAX_NODES: usize = 5000;
/// Моно-текст урезаем до 2 МиБ при рендере (полные байты — Raw/Hex).
pub const TEXT_RENDER_CAP: usize = 2 * 1024 * 1024;
/// Окно binary-детекта: NUL в первых 8 КиБ → бинарные данные.
pub const BINARY_SNIFF_WINDOW: usize = 8 * 1024;
/// Доля символов-замен (U+FFFD) в «тексте», после которой это не текст,
/// а бинарные данные (сжатые/шифрованные байты): показываем hex, не мусор.
pub const GARBAGE_REPLACEMENT_SHARE: f64 = 0.1;
/// Лимит декодера картинок по стороне (до thumbnail).
pub const IMAGE_DECODE_MAX_DIM: u32 = 16_384;
/// Картинки больше — downscale thumbnail'ом до этой стороны.
pub const IMAGE_THUMBNAIL_MAX: u32 = 2048;

// ---------------------------------------------------------------------------
// Чистые помощники (unit-тесты внизу файла)
// ---------------------------------------------------------------------------

/// Значение первого заголовка по имени (case-insensitive).
pub fn header_value<'a>(headers: &'a [HeaderEntryDto], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}
/// Binary-детект: NUL-байт в первых [`BINARY_SNIFF_WINDOW`] байтах.
/// (Текстовые кодировки NUL не содержат; UTF-16 ловится раньше по BOM.)
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(BINARY_SNIFF_WINDOW).any(|&b| b == 0)
}

/// Сжатие по сигнатуре для тела без Content-Encoding (клиенты шлют zlib/gzip
/// как text/plain): `(кодировка для декодера, подпись)`. zlib проверяется по
/// RFC 1950 (CM=8, окно ≤32K, FCHECK); ложное срабатывание отсеет декодер
/// (контрольная сумма), тогда тело остаётся как есть. Brotli сигнатуры не имеет.
pub fn sniff_compression(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    match *bytes {
        [0x1f, 0x8b, 0x08, ..] => Some(("gzip", "gzip")),
        [0x28, 0xb5, 0x2f, 0xfd, ..] => Some(("zstd", "zstd")),
        [cmf, flg, ..]
            if cmf & 0x0f == 8
                && cmf >> 4 <= 7
                && (u16::from(cmf) << 8 | u16::from(flg)) % 31 == 0 =>
        {
            Some(("deflate", "zlib"))
        }
        _ => None,
    }
}

/// Текст после lossy-декодирования — на деле мусор из бинарных байт
/// (доля U+FFFD больше [`GARBAGE_REPLACEMENT_SHARE`]).
pub fn looks_like_garbage(text: &str) -> bool {
    let (total, replaced) = text.chars().fold((0usize, 0usize), |(total, replaced), c| {
        (
            total + 1,
            replaced + usize::from(c == char::REPLACEMENT_CHARACTER),
        )
    });
    total > 0 && replaced as f64 / total as f64 > GARBAGE_REPLACEMENT_SHARE
}

/// Парсинг `application/x-www-form-urlencoded` в пары ключ/значение
/// (`+` → пробел, `%XX` — percent-decoding; `url::form_urlencoded`).
pub fn parse_form(bytes: &[u8]) -> Vec<(String, String)> {
    url::form_urlencoded::parse(bytes)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// Число узлов JSON-значения (объект/массив считаются, листья — 1).
pub fn count_json_nodes(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::Object(map) => 1 + map.values().map(count_json_nodes).sum::<usize>(),
        serde_json::Value::Array(items) => 1 + items.iter().map(count_json_nodes).sum::<usize>(),
        _ => 1,
    }
}

/// Проходит ли JSON под коллапсируемое дерево (гварды размера/узлов).
pub fn json_tree_fits(bytes: usize, nodes: usize) -> bool {
    bytes <= JSON_TREE_MAX_BYTES && nodes <= JSON_TREE_MAX_NODES
}

/// Экранирование токена для JSON Pointer (RFC 6901): `~` → `~0`, `/` → `~1`.
pub fn pointer_escape(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Hex-превью первых `max` байтов: "47 49 46 38 39 61 …".
pub fn hex_preview(bytes: &[u8], max: usize) -> String {
    let take = bytes.len().min(max);
    let head: Vec<String> = bytes[..take].iter().map(|b| format!("{b:02X}")).collect();
    let mut s = head.join(" ");
    if bytes.len() > take {
        s.push_str(" …");
    }
    s
}

/// Класс тела по mime-типу (без параметров).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyClass {
    Json,
    Image,
    Html,
    Xml,
    Form,
    Text,
    Binary,
}

/// Классификация mime → стратегия рендера. `+json`/`+xml` суффиксы
/// (application/vnd.api+json и т.п.) считаются JSON/XML.
pub fn classify(mime: &str) -> BodyClass {
    let m = mime.trim();
    if m.is_empty() {
        return BodyClass::Binary;
    }
    if m.ends_with("+json")
        || m == "application/json"
        || m == "text/json"
        || m == "application/json5"
    {
        return BodyClass::Json;
    }
    if matches!(m, "image/png" | "image/jpeg" | "image/gif" | "image/webp") {
        return BodyClass::Image;
    }
    if m == "text/html" {
        return BodyClass::Html;
    }
    if m.ends_with("+xml") || m == "application/xml" || m == "text/xml" {
        return BodyClass::Xml;
    }
    if m == "application/x-www-form-urlencoded" {
        return BodyClass::Form;
    }
    if m.starts_with("text/")
        || m == "application/javascript"
        || m == "text/javascript"
        || m == "application/x-javascript"
        || m == "application/typescript"
        || m == "application/csv"
        || m == "text/csv"
        || m == "application/x-yaml"
        || m == "text/yaml"
        || m == "application/graphql"
    {
        return BodyClass::Text;
    }
    BodyClass::Binary
}

/// Простой depth-based отступ XML: открывающий тег — +1 глубина,
/// закрывающий — −1, self-closing/декларация/комментарий — без изменения.
/// Текст между тегами печатается на текущей глубине.
pub fn indent_xml(text: &str) -> String {
    indent_markup(text, false)
}

/// HTML с «базовым» отступом: как [`indent_xml`], но void-элементы
/// (`br`, `img`, `meta`, …) глубину не увеличивают, а содержимое
/// `<script>/<style>/<pre>/<textarea>` печатается как есть.
pub fn indent_html(text: &str) -> String {
    indent_markup(text, true)
}

/// Void-элементы HTML (не имеют закрывающего тега).
const HTML_VOID_ELEMENTS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Элементы, чьё сыроё содержимое не переформатируем (закрывающий тег
/// ищем буквально).
const HTML_RAW_TEXT: [&str; 4] = ["script", "style", "pre", "textarea"];

/// Общий сканер разметки для [`indent_xml`]/[`indent_html`].
fn indent_markup(text: &str, html_rules: bool) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut depth: usize = 0;
    let bytes = text.as_bytes();
    let mut pos = 0usize;

    let push_indent = |out: &mut String, depth: usize| {
        for _ in 0..depth {
            out.push_str("  ");
        }
    };

    while pos < bytes.len() {
        let is_decl = text[pos..].starts_with("<?");
        let is_comment = text[pos..].starts_with("<!--");
        if is_decl || is_comment {
            // Декларация <?xml ...?> / комментарий <!-- ... --> — как есть.
            let end = text[pos..]
                .find(if is_decl { "?>" } else { "-->" })
                .map(|i| pos + i + if is_decl { 2 } else { 3 })
                .unwrap_or(bytes.len());
            push_indent(&mut out, depth);
            out.push_str(&text[pos..end]);
            out.push('\n');
            pos = end;
            continue;
        }
        if bytes[pos] == b'<' && pos + 1 < bytes.len() && bytes[pos + 1] == b'/' {
            // Закрывающий тег.
            let end = text[pos..]
                .find('>')
                .map(|i| pos + i + 1)
                .unwrap_or(bytes.len());
            depth = depth.saturating_sub(1);
            push_indent(&mut out, depth);
            out.push_str(&text[pos..end]);
            out.push('\n');
            pos = end;
            continue;
        }
        if bytes[pos] == b'<' {
            let end = text[pos..]
                .find('>')
                .map(|i| pos + i + 1)
                .unwrap_or(bytes.len());
            let tag_src = &text[pos..end];
            let self_closing = tag_src.ends_with("/>");
            let name: String = tag_src
                .trim_start_matches('<')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == ':' || *c == '-' || *c == '_')
                .collect::<String>()
                .to_ascii_lowercase();
            push_indent(&mut out, depth);
            out.push_str(tag_src);
            // HTML: сырой текст <script>/<style>/<pre>/<textarea> — блоком.
            if html_rules && !self_closing && HTML_RAW_TEXT.contains(&name.as_str()) {
                let close = format!("</{name}");
                if let Some(rel) = text[end..].to_ascii_lowercase().find(&close) {
                    let stop = end + rel;
                    out.push_str(&text[end..stop]);
                    out.push('\n');
                    push_indent(&mut out, depth);
                    let tag_end = text[stop..].find('>').map(|i| stop + i + 1).unwrap_or(stop);
                    out.push_str(&text[stop..tag_end]);
                    out.push('\n');
                    pos = tag_end;
                    continue;
                }
            }
            out.push('\n');
            let opens = !self_closing
                && !name.is_empty()
                && !(html_rules && HTML_VOID_ELEMENTS.contains(&name.as_str()));
            if opens {
                depth += 1;
            }
            pos = end;
            continue;
        }
        // Текстовый узел: до следующего '<'.
        let next_tag = text[pos..]
            .find('<')
            .map(|i| pos + i)
            .unwrap_or(bytes.len());
        let chunk = text[pos..next_tag].trim();
        if !chunk.is_empty() {
            push_indent(&mut out, depth);
            out.push_str(chunk);
            out.push('\n');
        }
        pos = next_tag;
    }
    out
}

// ---------------------------------------------------------------------------
// Off-thread подготовка: байты → render-ready вид
// ---------------------------------------------------------------------------

/// Что нужно для загрузки тела направления (извлекается из DTO).
#[derive(Clone)]
struct BodySpec {
    body_ref: BodyRef,
    /// Content-Type заголовок (приоритет) либо body_ref.content_type.
    content_type: Option<String>,
    /// Сжатие сохранённых байт (`BodyRef.encoding`: gzip/br/deflate).
    content_encoding: Option<String>,
}

/// Что известно о теле направления. Сжатие — из `BodyRef.encoding`
/// (как в экспорте и поиске): движок пишет его из Content-Encoding, а у
/// импортированного из HAR тела оно пустое — HAR хранит распакованное.
fn body_spec(detail: &ExchangeDetailDto, direction: Direction) -> Option<BodySpec> {
    match direction {
        Direction::Request => {
            let body_ref = detail.request.body.clone()?;
            Some(BodySpec {
                content_type: header_value(&detail.request.headers, "Content-Type")
                    .map(str::to_string)
                    .or_else(|| body_ref.content_type.clone()),
                content_encoding: body_ref.encoding.clone(),
                body_ref,
            })
        }
        Direction::Response => {
            let body_ref = detail.response.as_ref()?.body.clone()?;
            Some(BodySpec {
                content_type: detail
                    .response
                    .as_ref()
                    .and_then(|r| header_value(&r.headers, "Content-Type").map(str::to_string))
                    .or_else(|| body_ref.content_type.clone()),
                content_encoding: body_ref.encoding.clone(),
                body_ref,
            })
        }
    }
}

/// Картинка для рендера: готовый `RenderImage` (BGRA, см. ниже) + метаданные.
pub struct DecodedImage {
    /// gpui `RenderImage` хранит пиксели в BGRA — меняем каналы R↔B.
    image: gpui_kit::RenderImage,
    width: u32,
    height: u32,
    format: &'static str,
}

/// Декодирование картинки с лимитами декодера и downscale ≤2048px.
pub fn decode_image(bytes: &[u8]) -> Result<DecodedImage, String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("guess format: {e}"))?;
    let format = match reader.format() {
        Some(image::ImageFormat::Png) => "png",
        Some(image::ImageFormat::Jpeg) => "jpeg",
        Some(image::ImageFormat::Gif) => "gif",
        Some(image::ImageFormat::WebP) => "webp",
        _ => "image",
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(IMAGE_DECODE_MAX_DIM);
    limits.max_image_height = Some(IMAGE_DECODE_MAX_DIM);
    // Гварды от декомпрессионных бомб: 16384² RGBA = 1 GiB — не даём.
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().map_err(|e| format!("decode: {e}"))?;
    let (w, h) = (img.width(), img.height());
    // Downscale до ≤2048 по большей стороне (thumbnail — быстрый box-filter).
    let img = if w.max(h) > IMAGE_THUMBNAIL_MAX {
        img.thumbnail(IMAGE_THUMBNAIL_MAX, IMAGE_THUMBNAIL_MAX)
    } else {
        img
    };
    // RGBA → BGRA: gpui рисует RenderImage как BGRA.
    let mut rgba = img.to_rgba8();
    for pixel in rgba.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(DecodedImage {
        image: gpui_kit::RenderImage::new(vec![image::Frame::new(rgba)]),
        width: w,
        height: h,
        format,
    })
}

/// Полная подготовка render-ready вида из декодированных байтов.
///
/// Вызывается вне UI-потока (внутри spawned-футуры). Возвращает вид для
/// decoded-стороны и raw-сторону для тумблера (Some — если тело было
/// Content-Encoding-сжато и декодер справился).
pub fn prepare_body(
    raw: &[u8],
    decoded: Arc<Vec<u8>>,
    content_type: Option<&str>,
    decode_note: Option<String>,
) -> (BodyView, Option<BodyView>) {
    let len = decoded.len();
    let (mime, charset) = content_type.map(split_content_type).unwrap_or_default();

    // Raw-сторона тумблера: сжатые байты — бинарные, hex-превью честнее всего.
    let raw_view = decode_note.as_ref().map(|_| {
        BodyView::Ready(Box::new(RenderKind::Binary {
            len: raw.len(),
            preview: hex_preview(raw, 32),
        }))
    });

    if len == 0 {
        return (BodyView::Empty, None);
    }

    let class = classify(&mime);
    // Картинки — отдельный лимит (20 МиБ) и отдельная ветка до общего
    // pretty-гварда: превью картинки не «текстовый» pretty-вид.
    if class == BodyClass::Image {
        if len > IMAGE_LIMIT {
            return (
                BodyView::Ready(Box::new(RenderKind::ImageTooBig { len })),
                raw_view,
            );
        }
        return match decode_image(&decoded) {
            Ok(img) => (
                BodyView::Ready(Box::new(RenderKind::Image {
                    width: img.width,
                    height: img.height,
                    format: img.format,
                    original_len: raw.len(),
                    decoded_len: len,
                    image: Arc::new(img.image),
                })),
                raw_view,
            ),
            // Не декодируется — показываем как бинарные данные.
            Err(_) => (
                BodyView::Ready(Box::new(RenderKind::Binary {
                    len,
                    preview: hex_preview(&decoded, 32),
                })),
                raw_view,
            ),
        };
    }

    if len > PRETTY_LIMIT {
        return (BodyView::TooBig { len }, raw_view);
    }

    match class {
        // Обработано выше (отдельный 20 МиБ лимит до pretty-гварда).
        BodyClass::Image => unreachable!("image handled before pretty guards"),
        BodyClass::Json => {
            match serde_json::from_slice::<serde_json::Value>(&decoded) {
                Ok(value) => {
                    let nodes = count_json_nodes(&value);
                    if json_tree_fits(len, nodes) {
                        (
                            BodyView::Ready(Box::new(RenderKind::JsonTree {
                                value: Arc::new(value),
                            })),
                            raw_view,
                        )
                    } else {
                        // Гварды размера/узлов: pretty-текст вместо дерева.
                        let text = serde_json::to_string_pretty(&value).unwrap_or_default();
                        (
                            BodyView::Ready(Box::new(RenderKind::PrettyText {
                                text: Arc::new(text),
                                encoding_note: format!("json · {nodes} nodes · pretty fallback"),
                            })),
                            raw_view,
                        )
                    }
                }
                // Невалидный JSON — как обычный текст.
                Err(_) => text_view(&decoded, charset.as_deref(), raw_view),
            }
        }
        BodyClass::Xml => {
            let (text, enc, had_errors) = decode_charset(&decoded, charset.as_deref());
            (
                BodyView::Ready(Box::new(RenderKind::PrettyText {
                    text: Arc::new(indent_xml(&text)),
                    encoding_note: encoding_label(enc, had_errors),
                })),
                raw_view,
            )
        }
        BodyClass::Html => {
            let (text, enc, had_errors) = decode_charset(&decoded, charset.as_deref());
            (
                BodyView::Ready(Box::new(RenderKind::PrettyText {
                    text: Arc::new(indent_html(&text)),
                    encoding_note: encoding_label(enc, had_errors),
                })),
                raw_view,
            )
        }
        BodyClass::Form => (
            BodyView::Ready(Box::new(RenderKind::Form {
                rows: Arc::new(parse_form(&decoded)),
            })),
            raw_view,
        ),
        BodyClass::Text => text_view(&decoded, charset.as_deref(), raw_view),
        BodyClass::Binary => {
            // NUL-снифф решает; без NUL и без ошибок декодирования — текст.
            if !is_binary(&decoded) {
                let (text, enc, had_errors) = decode_charset(&decoded, charset.as_deref());
                if !had_errors {
                    return (
                        BodyView::Ready(Box::new(RenderKind::PrettyText {
                            text: Arc::new(text),
                            encoding_note: encoding_label(enc, false),
                        })),
                        raw_view,
                    );
                }
            }
            (
                BodyView::Ready(Box::new(RenderKind::Binary {
                    len,
                    preview: hex_preview(&decoded, 32),
                })),
                raw_view,
            )
        }
    }
}

/// Моно-текст с charset-декодированием (+cap отображения).
fn text_view(
    decoded: &[u8],
    charset: Option<&str>,
    raw_view: Option<BodyView>,
) -> (BodyView, Option<BodyView>) {
    let (text, enc, had_errors) = decode_charset(decoded, charset);
    if had_errors && looks_like_garbage(&text) {
        let view = RenderKind::Binary {
            len: decoded.len(),
            preview: hex_preview(decoded, 32),
        };
        return (BodyView::Ready(Box::new(view)), raw_view);
    }
    let note = encoding_label(enc, had_errors);
    if text.len() > TEXT_RENDER_CAP {
        let mut cut = text[..TEXT_RENDER_CAP].to_string();
        cut.push_str(&format!(
            "\n\n… truncated for display ({} total), full bytes in Raw/Hex",
            human_len(text.len())
        ));
        (
            BodyView::Ready(Box::new(RenderKind::PrettyText {
                text: Arc::new(cut),
                encoding_note: note,
            })),
            raw_view,
        )
    } else {
        (
            BodyView::Ready(Box::new(RenderKind::PrettyText {
                text: Arc::new(text),
                encoding_note: note,
            })),
            raw_view,
        )
    }
}

/// Подпись кодировки: "utf-8" / "windows-1251, 3 заменённых байта".
fn encoding_label(encoding: &'static Encoding, had_errors: bool) -> String {
    if had_errors {
        format!("{}, lossy", encoding.name().to_ascii_lowercase())
    } else {
        encoding.name().to_ascii_lowercase()
    }
}

/// человекочитаемый размер (КБ/МиБ) для подписей.
fn human_len(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

// ---------------------------------------------------------------------------
// Состояние (per обмен × направление)
// ---------------------------------------------------------------------------

/// Какое тело смотрим.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Direction {
    #[default]
    Request,
    Response,
}

/// Render-ready вид тела: всё, что тяжело, посчитано вне UI-потока.
#[derive(Clone)]
pub enum BodyView {
    /// Грузим/декодируем (task в полёте).
    Loading,
    /// Тела нет: 0 байт захвачено.
    Empty,
    /// Тело не сохранилось (cap хранилища/стриминг) — байтов нет.
    NotCaptured,
    /// Ошибка чтения/декодирования.
    Failed(String),
    /// Decoded > [`PRETTY_LIMIT`] — pretty-виды отказываются (Hex листает).
    TooBig { len: usize },
    /// Готовый рендер-вид.
    Ready(Box<RenderKind>),
}

/// Конкретный рендер-вид (данные уже подготовлены).
#[derive(Clone)]
pub enum RenderKind {
    /// JSON-дерево (значение + items для `Tree`; id items = JSON Pointer).
    JsonTree { value: Arc<serde_json::Value> },
    /// Моно-текст: JSON-fallback (to_string_pretty), XML/HTML pretty, text.
    PrettyText {
        text: Arc<String>,
        /// Подпись кодировки/lossy ("utf-8", "windows-1251, 3 замены").
        encoding_note: String,
    },
    /// form-urlencoded: пары ключ/значение.
    Form { rows: Arc<Vec<(String, String)>> },
    /// Картинка: готовый к рендеру RenderImage (BGRA, ≤2048px) + подпись.
    Image {
        width: u32,
        height: u32,
        format: &'static str,
        original_len: usize,
        decoded_len: usize,
        image: Arc<gpui_kit::RenderImage>,
    },
    /// Картинка есть, но > [`IMAGE_LIMIT`].
    ImageTooBig { len: usize },
    /// Бинарные данные: размер + hex-превью первого куска.
    Binary { len: usize, preview: String },
}

/// Порты для загрузки тела (клоны из ExchangeContext; хранятся в Entity —
/// вкладка сама инициирует ленивую загрузку при переключении направления).
#[derive(Clone)]
struct BodyLoader {
    repo: Arc<dyn domain::repositories::BodyRepository>,
    decoder: Arc<dyn application::services::BodyDecoderService>,
    runtime: tokio::runtime::Handle,
}

/// Состояние одной стороны (request/response) одного обмена.
pub struct BodySide {
    /// Что грузить (None — тела у направления нет).
    spec: Option<BodySpec>,
    /// Текущий render-ready вид (decoded-сторона).
    view: BodyView,
    /// Raw-сторона для тумблера «raw/decoded» (Some — тело было сжато).
    raw_view: Option<BodyView>,
    /// Показывать сырые (недекодированные) байты?
    show_raw: bool,
    /// «decoded from gzip, 1.2 KB → 3.4 KB» (Some — если была кодировка).
    decode_note: Option<String>,
    /// Загрузка уже запущена (защита от повторного spawn в рендере).
    loading_started: bool,
    /// Дерево для JSON-вида (создаётся при готовом JsonTree).
    tree: Option<Entity<TreeState>>,
    /// Тело как текст (JSON — отформатированный) для режима Text и Copy.
    text: Option<Arc<String>>,
    /// Режим Text: read-only редактор вместо pretty-вида.
    text_mode: bool,
    /// Редактор режима Text (лениво: нужен Window).
    editor: Option<Entity<gpui_kit::component::input::EditorState>>,
}

impl BodySide {
    fn new() -> Self {
        Self {
            spec: None,
            view: BodyView::Loading,
            raw_view: None,
            show_raw: false,
            decode_note: None,
            loading_started: false,
            tree: None,
            text: None,
            text_mode: false,
            editor: None,
        }
    }
}

/// Состояние вкладки Body одного обмена: направление + обе стороны.
/// Само реализует [`gpui_kit::Render`] и монтируется в дерево элементов
/// (`.child(entity)`) — notify перерисовывает именно это поддерево,
/// RenderOnce-обёртка вкладки остаётся тонкой (паттерн `Tree` из
/// gpui-base).
pub struct BodyTabState {
    direction: Direction,
    /// Порты загрузки (обновляются вкладкой при каждом её рендере).
    loader: Option<BodyLoader>,
    req: BodySide,
    resp: BodySide,
}

impl BodyTabState {
    fn new() -> Self {
        Self {
            direction: Direction::Request,
            loader: None,
            req: BodySide::new(),
            resp: BodySide::new(),
        }
    }

    fn side(&self, direction: Direction) -> &BodySide {
        match direction {
            Direction::Request => &self.req,
            Direction::Response => &self.resp,
        }
    }

    fn side_mut(&mut self, direction: Direction) -> &mut BodySide {
        match direction {
            Direction::Request => &mut self.req,
            Direction::Response => &mut self.resp,
        }
    }

    /// Режим Text включён, а редактора ещё нет — создать (нужен Window).
    fn ensure_text_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let direction = self.direction;
        let side = self.side(direction);
        if !side.text_mode || side.editor.is_some() {
            return;
        }
        let Some(text) = side.text.clone() else {
            return;
        };
        let editor = body_text::build_text_editor(&text, window, cx);
        self.side_mut(direction).editor = Some(editor);
    }

    /// Вид, который показываем (decoded-сторона или raw по тумблеру).
    fn active_view(&self) -> &BodyView {
        let side = self.side(self.direction);
        if side.show_raw {
            side.raw_view.as_ref().unwrap_or(&side.view)
        } else {
            &side.view
        }
    }

    /// Лениво запустить загрузку стороны (идемпотентно: guard
    /// `loading_started`). Вызывается из рендеров вкладки и состояния.
    fn ensure_loaded(&mut self, direction: Direction, cx: &mut Context<Self>) {
        if self.side(direction).loading_started {
            return;
        }
        let Some(loader) = self.loader.clone() else {
            return;
        };
        let side = self.side_mut(direction);
        side.loading_started = true;
        let Some(spec) = side.spec.clone() else {
            side.view = BodyView::Empty; // тела у направления нет
            return;
        };
        side.view = BodyView::Loading;

        let entity = cx.entity();
        let runtime = loader.runtime.clone();
        let fut = async move {
            let raw = loader.repo.get(&spec.body_ref).await?;
            let Some(raw) = raw else {
                // Тело не сохранилось (cap/стриминг) — байтов нет.
                return Ok(PreparedLoad::not_captured());
            };
            if raw.is_empty() {
                return Ok(PreparedLoad::empty());
            }
            // Content-Encoding → декодер (pass-through для неизвестных).
            let (decoded, note) = match spec
                .content_encoding
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(enc)
                    if !loader.decoder.supported(enc).await
                        && !enc.eq_ignore_ascii_case("identity") =>
                {
                    return Ok(PreparedLoad::still_encoded(
                        &raw,
                        format!("{enc}: unsupported content-encoding — not decoded"),
                    ));
                }
                Some(enc) => match loader.decoder.decode(&raw, enc).await {
                    Ok(d) => {
                        let note = format!(
                            "decoded from {}, {} → {}",
                            enc.to_ascii_lowercase(),
                            human_len(raw.len()),
                            human_len(d.len())
                        );
                        (Arc::new(d), Some(note))
                    }
                    // Декодер не справился: байты всё ещё сжаты — бинарный вид.
                    Err(e) => {
                        return Ok(PreparedLoad::still_encoded(
                            &raw,
                            format!("decode {enc} failed: {e}"),
                        ));
                    }
                },
                None => sniff_and_decode(&raw, loader.decoder.as_ref()).await,
            };
            let (view, raw_view) =
                prepare_body(&raw, decoded, spec.content_type.as_deref(), note.clone());
            Ok(PreparedLoad {
                view,
                raw_view,
                note,
            })
        };
        spawn_ui_task(cx, &entity, runtime, fut, move |this, result, cx| {
            let side = this.side_mut(direction);
            match result {
                Ok(load) => {
                    side.view = load.view;
                    side.raw_view = load.raw_view;
                    side.decode_note = load.note;
                    side.show_raw = false;
                }
                Err(e) => side.view = BodyView::Failed(e.to_string()),
            }
            // JSON-дерево: build items → Entity<TreeState>.
            if let BodyView::Ready(kind) = &side.view
                && let RenderKind::JsonTree { value } = kind.as_ref()
            {
                let items = json_tree_items(value);
                let tree = cx.new(|cx| TreeState::new(cx).items(items));
                side.tree = Some(tree);
            }
            // Текст для режима Text / Copy; редактор пересоздаётся лениво.
            side.text = match &side.view {
                BodyView::Ready(kind) => body_text::body_text(kind).map(Arc::new),
                _ => None,
            };
            side.editor = None;
            cx.notify();
        });
    }
}

/// Тело без Content-Encoding: если по сигнатуре оно сжато и распаковывается —
/// распакованное с подписью, иначе байты как есть.
async fn sniff_and_decode(
    raw: &[u8],
    decoder: &dyn application::services::BodyDecoderService,
) -> (Arc<Vec<u8>>, Option<String>) {
    let Some((codec, label)) = sniff_compression(raw) else {
        return (Arc::new(raw.to_vec()), None);
    };
    match decoder.decode(raw, codec).await {
        Ok(decoded) => {
            let note = format!(
                "decoded from {label} (detected by signature, no Content-Encoding), {} → {}",
                human_len(raw.len()),
                human_len(decoded.len())
            );
            (Arc::new(decoded), Some(note))
        }
        Err(_) => (Arc::new(raw.to_vec()), None),
    }
}

/// Результат off-thread загрузки (переезжает в UI одним значением).
struct PreparedLoad {
    view: BodyView,
    raw_view: Option<BodyView>,
    note: Option<String>,
}

impl PreparedLoad {
    /// Тело не сохранилось (cap/стриминг): байтов нет вовсе.
    fn not_captured() -> Self {
        Self {
            view: BodyView::NotCaptured,
            raw_view: None,
            note: None,
        }
    }

    /// Тело всё ещё закодировано (кодировка неизвестна или данные битые):
    /// показываем бинарное hex-превью и честную причину — не текст из сжатых
    /// байт (раньше «decoded from zstd» рисовал мусор как windows-1251).
    fn still_encoded(raw: &[u8], note: String) -> Self {
        Self {
            view: BodyView::Ready(Box::new(RenderKind::Binary {
                len: raw.len(),
                preview: hex_preview(raw, 32),
            })),
            raw_view: None,
            note: Some(note),
        }
    }

    /// Захвачено 0 байт.
    fn empty() -> Self {
        Self {
            view: BodyView::Empty,
            raw_view: None,
            note: None,
        }
    }
}

// ---------------------------------------------------------------------------
// JSON-дерево → TreeItem (id = JSON Pointer, RFC 6901)
// ---------------------------------------------------------------------------

use gpui_kit::base::{Tree, TreeEntry, TreeEntryState, TreeItem, TreeState};

// ---------------------------------------------------------------------------
// Рендер
// ---------------------------------------------------------------------------

/// Моноширинный системный шрифт (macOS).
const MONO_FONT: &str = ".AppleSystemUIFontMonospaced";

/// Сегмент-переключатель (Request/Response, Decoded/Raw): маленькая
/// кликабельная «кнопка»-div. Цвета посчитаны снаружи (тема в скоупе).
// UI-хелпер с готовым набором параметров стиля; группа в структуру
// усложнит вызовы без выигрыша.
#[allow(clippy::too_many_arguments)]
fn segment(
    id: &'static str,
    label: &str,
    active: bool,
    enabled: bool,
    fg: gpui_kit::Hsla,
    bg_active: gpui_kit::Hsla,
    bg_hover: gpui_kit::Hsla,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .text_size(px(11.))
        .rounded_sm()
        .line_height(gpui_kit::relative(1.3))
        .text_color(if active { fg } else { fg.opacity(0.65) })
        .map(|this| {
            if active {
                this.bg(bg_active)
            } else if enabled {
                this.hover(|s| s.bg(bg_hover))
            } else {
                this.opacity(0.35)
            }
        })
        .when(enabled, |this| {
            this.cursor_pointer().on_click(move |event, window, cx| {
                on_click(event, window, cx);
            })
        })
        .child(label.to_string())
}

/// Цвет значения-листа JSON (подсветка синтаксиса в цветах палитры).
fn json_value_color(v: &serde_json::Value, p: &Palette) -> gpui_kit::Hsla {
    match v {
        serde_json::Value::String(_) => p.tone(ToneName::Green).fg,
        serde_json::Value::Number(_) => p.tone(ToneName::Orange).fg,
        serde_json::Value::Bool(_) => p.tone(ToneName::Purple).fg,
        serde_json::Value::Null => p.text_tertiary,
        _ => p.text_secondary,
    }
}

impl gpui_kit::Render for BodyTabState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Ленивая загрузка активной стороны (идемпотентно: guard в стороне).
        self.ensure_loaded(self.direction, cx);
        self.ensure_text_editor(window, cx);
        let theme = cx.theme();
        let entity = cx.entity();
        let direction = self.direction;
        let side = self.side(direction);

        let (fg, active_bg, hover_bg) = (
            theme.foreground,
            theme.secondary_hover,
            theme.foreground.opacity(0.06),
        );

        // --- Мета: mime + note о декодировании ---
        let mime = side
            .spec
            .as_ref()
            .and_then(|s| s.content_type.as_deref())
            .map(|ct| split_content_type(ct).0)
            .unwrap_or_else(|| "no content-type".into());
        let mut meta = mime;
        if let Some(note) = &side.decode_note {
            meta.push_str("  ·  ");
            meta.push_str(note);
        }

        // --- Тумблер Decoded/Raw (есть, только если тело было сжато) ---
        let raw_entity = entity.clone();
        let decoded_entity = entity.clone();
        let raw_toggle = side.raw_view.is_some().then(|| {
            gpui_kit::component::h_flex()
                .gap_1()
                .p_1()
                .rounded_md()
                .bg(theme.muted)
                .child(segment(
                    "body-show-decoded",
                    "Decoded",
                    !side.show_raw,
                    true,
                    fg,
                    active_bg,
                    hover_bg,
                    move |_, _, cx| {
                        raw_entity.update(cx, |state, cx| {
                            state.side_mut(direction).show_raw = false;
                            cx.notify();
                        });
                    },
                ))
                .child(segment(
                    "body-show-raw",
                    "Raw",
                    side.show_raw,
                    true,
                    fg,
                    active_bg,
                    hover_bg,
                    move |_, _, cx| {
                        decoded_entity.update(cx, |state, cx| {
                            state.side_mut(direction).show_raw = true;
                            cx.notify();
                        });
                    },
                ))
        });

        // --- Pretty/Text + Copy (есть, если у тела текстовое представление) ---
        let text_controls = side.text.clone().filter(|_| !side.show_raw).map(|text| {
            let (pretty_entity, text_entity) = (entity.clone(), entity.clone());
            let set_mode = |entity: Entity<Self>, text_mode: bool| {
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    entity.update(cx, |state, cx| {
                        state.side_mut(direction).text_mode = text_mode;
                        cx.notify();
                    });
                }
            };
            gpui_kit::component::h_flex()
                .gap_1()
                .p_1()
                .rounded_md()
                .bg(theme.muted)
                .child(segment(
                    "body-mode-pretty",
                    "Pretty",
                    !side.text_mode,
                    true,
                    fg,
                    active_bg,
                    hover_bg,
                    set_mode(pretty_entity, false),
                ))
                .child(segment(
                    "body-mode-text",
                    "Text",
                    side.text_mode,
                    true,
                    fg,
                    active_bg,
                    hover_bg,
                    set_mode(text_entity, true),
                ))
                .child(segment(
                    "body-copy",
                    "Copy",
                    false,
                    true,
                    fg,
                    active_bg,
                    hover_bg,
                    move |_, window, cx| super::kv::copy_text(&text, "Body", window, cx),
                ))
        });

        let toolbar = gpui_kit::component::h_flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.muted_foreground)
                    .child(meta),
            )
            .child(
                gpui_kit::component::h_flex()
                    .ml_auto()
                    .gap_2()
                    .children(text_controls)
                    .children(raw_toggle),
            );

        let content = match (&side.editor, side.text_mode && !side.show_raw) {
            (Some(editor), true) => body_text::render_text_editor(editor),
            _ => render_body_view(self.active_view(), side, theme),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h(px(0.))
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(content),
            )
    }
}

/// Центрированное сообщение (пустые состояния/отказы).
fn centered_message(msg: String, color: gpui_kit::Hsla) -> gpui_kit::AnyElement {
    gpui_kit::component::h_flex()
        .size_full()
        .flex_1()
        .justify_center()
        .text_size(px(12.))
        .text_color(color)
        .child(msg)
        .into_any_element()
}

/// Рендер активного вида тела.
fn render_body_view(
    view: &BodyView,
    side: &BodySide,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    match view {
        BodyView::Loading => {
            centered_message("Decoding body…".into(), theme.foreground.opacity(0.5))
        }
        BodyView::Empty => {
            centered_message("Empty body (0 bytes)".into(), theme.foreground.opacity(0.5))
        }
        BodyView::NotCaptured => centered_message(
            "Body not captured — size cap exceeded or streamed".into(),
            theme.foreground.opacity(0.5),
        ),
        BodyView::Failed(e) => centered_message(format!("Failed to load body: {e}"), theme.danger),
        BodyView::TooBig { len } => centered_message(
            format!(
                "{} — too large for pretty view. See the Hex tab.",
                human_len(*len)
            ),
            theme.foreground.opacity(0.6),
        ),
        BodyView::Ready(kind) => match kind.as_ref() {
            RenderKind::JsonTree { value } => match &side.tree {
                // list_style обязателен: базовый Tree отдаёт его внутреннему
                // uniform_list, у которого нет своей высоты — без него список
                // нулевой высоты и JSON выглядел пустым.
                Some(tree) => Tree::new(tree)
                    .item(json_row_renderer(value.clone()))
                    .list_style(
                        gpui_kit::StyleRefinement::default()
                            .flex_grow_1()
                            .size_full(),
                    )
                    .size_full()
                    .into_any_element(),
                None => centered_message(
                    "JSON tree unavailable".into(),
                    theme.foreground.opacity(0.5),
                ),
            },
            RenderKind::PrettyText {
                text,
                encoding_note,
            } => div()
                .id("body-pretty-text")
                .size_full()
                .overflow_y_scroll()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .font_family(MONO_FONT)
                .text_size(px(11.))
                .line_height(gpui_kit::relative(1.45))
                .text_color(theme.foreground.opacity(0.85))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(theme.muted_foreground)
                        .child(encoding_note.clone()),
                )
                .child(div().child(text.as_str().to_string()))
                .into_any_element(),
            RenderKind::Form { rows } => render_form(rows, theme),
            RenderKind::Image {
                width,
                height,
                format,
                original_len,
                decoded_len,
                image,
            } => render_image_view(
                *width,
                *height,
                format,
                *original_len,
                *decoded_len,
                image,
                theme,
            ),
            RenderKind::ImageTooBig { len } => centered_message(
                format!(
                    "Image {} — larger than the 20 MB preview limit. See the Hex tab.",
                    human_len(*len)
                ),
                theme.foreground.opacity(0.6),
            ),
            RenderKind::Binary { len, preview } => div()
                .id("body-binary")
                .size_full()
                .overflow_y_scroll()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.foreground.opacity(0.6))
                        .child(format!("Binary — {} — see the Hex tab", human_len(*len))),
                )
                .child(
                    div()
                        .font_family(MONO_FONT)
                        .text_size(px(11.))
                        .text_color(theme.muted_foreground)
                        .child(preview.clone()),
                )
                .into_any_element(),
        },
    }
}

/// Таблица key/value для form-urlencoded.
fn render_form(
    rows: &[(String, String)],
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    if rows.is_empty() {
        return centered_message("No form fields".into(), theme.foreground.opacity(0.5));
    }
    div()
        .id("body-form")
        .size_full()
        .overflow_y_scroll()
        .p(px(12.))
        .flex()
        .flex_col()
        .gap_1()
        .font_family(MONO_FONT)
        .text_size(px(11.))
        .children(rows.iter().map(|(k, v)| {
            gpui_kit::component::h_flex()
                .gap_2()
                .items_start()
                .child(
                    div()
                        .w(px(220.))
                        .flex_shrink_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_color(theme.blue)
                        .child(k.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_color(theme.foreground.opacity(0.85))
                        .child(v.clone()),
                )
        }))
        .into_any_element()
}

/// Image preview + подпись (размеры/формат/байты).
fn render_image_view(
    width: u32,
    height: u32,
    format: &str,
    original_len: usize,
    decoded_len: usize,
    image: &Arc<gpui_kit::RenderImage>,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::AnyElement {
    use gpui_kit::StyledImage as _;
    let caption = if original_len != decoded_len {
        format!(
            "{width}×{height} {format} · original {} · decoded {}",
            human_len(original_len),
            human_len(decoded_len)
        )
    } else {
        format!("{width}×{height} {format} · {}", human_len(original_len))
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .items_center()
                .justify_center()
                .p_3()
                .child(
                    gpui_kit::img(image.clone())
                        .max_w_full()
                        .max_h_full()
                        .object_fit(gpui_kit::ObjectFit::ScaleDown),
                ),
        )
        .child(
            div()
                .py_1p5()
                .text_size(px(11.))
                .text_color(theme.muted_foreground)
                .text_center()
                .child(caption),
        )
        .into_any_element()
}

// --- JSON-дерево ------------------------------------------------------------

/// Родительский указатель ("/a/0" → "/a", "/a" → "", "" → None).
fn parent_pointer(ptr: &str) -> Option<&str> {
    if ptr.is_empty() {
        None
    } else {
        ptr.rfind('/').map(|i| &ptr[..i])
    }
}

/// Последний сегмент указателя без экранирования ("" → None, корень).
fn last_segment_unescaped(ptr: &str) -> Option<String> {
    if ptr.is_empty() {
        return None;
    }
    let last = ptr.rsplit('/').next().unwrap_or_default();
    if last.is_empty() {
        None
    } else {
        Some(last.replace("~1", "/").replace("~0", "~"))
    }
}

/// Кастомный рендер строк JSON-дерева: отступ по глубине, стрелка
/// expand/collapse (клик ловит сам Tree), ключ синим, значение — по типу.
fn json_row_renderer(
    value: Arc<serde_json::Value>,
) -> impl Fn(usize, &TreeEntry, TreeEntryState, &mut Window, &mut App) -> gpui_kit::AnyElement + 'static
{
    move |_, entry, state, _, cx| {
        let p = glass(cx);
        let theme = cx.theme();
        let ptr = entry.item().id.clone();
        let depth = entry.depth();
        let node = value.pointer(&ptr);

        // Ключ узла: из указателя; родитель-объект → строковый ключ,
        // родитель-массив → индекс.
        let key_element = (|| -> Option<gpui_kit::Div> {
            let seg = last_segment_unescaped(&ptr)?;
            let in_object = parent_pointer(&ptr)
                .and_then(|parent| value.pointer(parent))
                .map(|p| matches!(p, serde_json::Value::Object(_)))
                .unwrap_or(false);
            let el = div().flex_shrink_0().text_color(if in_object {
                p.tone(ToneName::Blue).fg
            } else {
                theme.foreground.opacity(0.45)
            });
            Some(if in_object {
                el.child(format!("\"{seg}\":"))
            } else {
                el.child(format!("{seg}:"))
            })
        })();

        let value_element = match node {
            // Пустые — `{}`/`[]`: «…» обещало бы содержимое, которого нет.
            Some(serde_json::Value::Object(m)) => Some(
                div()
                    .text_color(theme.foreground.opacity(0.5))
                    .child(if m.is_empty() {
                        "{}".to_string()
                    } else {
                        format!("{{…}} {}", m.len())
                    }),
            ),
            Some(serde_json::Value::Array(a)) => Some(
                div()
                    .text_color(theme.foreground.opacity(0.5))
                    .child(if a.is_empty() {
                        "[]".to_string()
                    } else {
                        format!("[…] {}", a.len())
                    }),
            ),
            Some(serde_json::Value::String(s)) => Some(
                div()
                    .text_color(p.tone(ToneName::Green).fg)
                    .child(truncate_str(&format!("\"{s}\""), JSON_PREVIEW_LEN)),
            ),
            Some(
                v @ (serde_json::Value::Number(_)
                | serde_json::Value::Bool(_)
                | serde_json::Value::Null),
            ) => Some(
                div()
                    .text_color(json_value_color(v, &p))
                    .child(v.to_string()),
            ),
            None => Some(
                div()
                    .text_color(theme.foreground.opacity(0.4))
                    .child(entry.item().label.clone()),
            ),
        };

        let arrow = if entry.is_folder() {
            div()
                .w(px(12.))
                .flex_shrink_0()
                .text_color(theme.foreground.opacity(0.5))
                .child(if entry.is_expanded() { "▾" } else { "▸" })
        } else {
            div().w(px(12.)).flex_shrink_0()
        };

        div()
            .pl(px(4.0 + 14.0 * depth as f32))
            .h(px(20.))
            .flex()
            .items_center()
            .gap_1()
            .font_family(MONO_FONT)
            .text_size(px(11.))
            .overflow_hidden()
            .when(state.is_selected(), |row| {
                row.bg(theme.secondary_hover).rounded_sm()
            })
            .child(arrow)
            .when_some(key_element, |row, key| row.child(key))
            .when_some(value_element, |row, val| row.child(val))
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// BodyTab: тонкая RenderOnce-обёртка (пропс — ExchangeContext)
// ---------------------------------------------------------------------------

/// Body: pretty-view тел (request/response, decode, JSON/image/text).
#[derive(gpui_kit::IntoElement)]
pub struct BodyTab {
    ctx: ExchangeContext,
}

impl BodyTab {
    pub fn new(ctx: ExchangeContext) -> Self {
        Self { ctx }
    }
}

impl RenderOnce for BodyTab {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let detail = self.ctx.detail.clone();
        let exchange = detail.summary.id;
        let loader = BodyLoader {
            repo: self.ctx.body_repo.clone(),
            decoder: self.ctx.decoder.clone(),
            runtime: self.ctx.runtime.clone(),
        };
        let req_spec = body_spec(&detail, Direction::Request);
        let resp_spec = body_spec(&detail, Direction::Response);

        let direction = match self.ctx.side {
            Side::Request => Direction::Request,
            Side::Response => Direction::Response,
        };
        let entity = tab_state(exchange, self.ctx.side, cx, |cx| {
            cx.new(|_| BodyTabState {
                direction,
                ..BodyTabState::new()
            })
        });
        entity.update(cx, |state, _| {
            state.loader = Some(loader);
            state.req.spec = req_spec;
            state.resp.spec = resp_spec;
        });
        div().size_full().child(entity)
    }
}

/// Максимальная длина превью значения-листа в подписи узла.
const JSON_PREVIEW_LEN: usize = 96;

/// Корневые TreeItem для JSON-значения (id корня — "", указатель на документ).
pub fn json_tree_items(value: &serde_json::Value) -> Vec<TreeItem> {
    vec![json_node(String::new(), None, value, 0)]
}

/// Узел дерева: id = JSON Pointer (корень — ""), label — превью для aria.
fn json_node(pointer: String, key: Option<&str>, v: &serde_json::Value, depth: usize) -> TreeItem {
    let mut item = TreeItem::new(pointer.clone(), json_node_label(key, v));
    match v {
        serde_json::Value::Object(map) => {
            item = item.expanded(depth < 2);
            for (k, val) in map {
                let child = format!("{}/{}", pointer, pointer_escape(k));
                item = item.child(json_node(child, Some(k), val, depth + 1));
            }
        }
        serde_json::Value::Array(items) => {
            item = item.expanded(depth < 2);
            for (i, val) in items.iter().enumerate() {
                let child = format!("{pointer}/{i}");
                item = item.child(json_node(child, None, val, depth + 1));
            }
        }
        _ => {}
    }
    item
}

/// Подпись узла (aria/поиск; раскраску рисует кастомный рендерер).
fn json_node_label(key: Option<&str>, v: &serde_json::Value) -> String {
    let head = key.map(|k| format!("\"{k}\": ")).unwrap_or_default();
    let tail = match v {
        serde_json::Value::Object(m) => format!("Object({})", m.len()),
        serde_json::Value::Array(a) => format!("Array({})", a.len()),
        serde_json::Value::String(s) => truncate_str(&format!("\"{s}\""), JSON_PREVIEW_LEN),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".into(),
    };
    format!("{head}{tail}")
}

/// Урезание строки с многоточием (по char-границам).
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn still_encoded_body_is_binary_with_reason() {
        let load = PreparedLoad::still_encoded(&[0x28, 0xB5, 0x2F, 0xFD, 1, 2], "zstd: x".into());
        assert!(matches!(
            load.view,
            BodyView::Ready(ref kind) if matches!(**kind, RenderKind::Binary { len: 6, .. })
        ));
        assert_eq!(load.note.as_deref(), Some("zstd: x"));
    }

    // --- binary sniff ---

    #[test]
    fn binary_sniff_window() {
        assert!(is_binary(&[0x47, 0x49, 0x46, 0x00]));
        // NUL за пределами окна — не бинарный детект.
        let mut far = vec![b'x'; BINARY_SNIFF_WINDOW + 1];
        far.push(0);
        assert!(!is_binary(&far));
        assert!(!is_binary(b"plain text\nwith lines"));
    }

    #[test]
    fn sniff_compression_recognises_signatures() {
        assert_eq!(
            sniff_compression(&[0x78, 0x9c, 0x01]),
            Some(("deflate", "zlib"))
        );
        assert_eq!(
            sniff_compression(&[0x78, 0xda, 0x01]),
            Some(("deflate", "zlib"))
        );
        assert_eq!(
            sniff_compression(&[0x1f, 0x8b, 0x08, 0]),
            Some(("gzip", "gzip"))
        );
        assert_eq!(
            sniff_compression(&[0x28, 0xb5, 0x2f, 0xfd]),
            Some(("zstd", "zstd"))
        );
        // Обычный текст на «x» не проходит FCHECK.
        assert_eq!(sniff_compression(b"xyz"), None);
        assert_eq!(sniff_compression(b"{\"a\":1}"), None);
        assert_eq!(sniff_compression(&[0x78]), None);
    }

    #[test]
    fn garbage_is_judged_by_replacement_share() {
        assert!(looks_like_garbage("x\u{FFFD}\u{FFFD}T\u{FFFD}"));
        assert!(!looks_like_garbage(
            "привет, мир \u{FFFD} один битый байт на длинной строке"
        ));
        assert!(!looks_like_garbage(""));
    }

    #[test]
    fn lossy_garbage_text_renders_as_binary() {
        let bytes: Vec<u8> = (0..200u8).map(|b| b | 0x80).collect();
        let (view, _) = prepare_body(&bytes, Arc::new(bytes.clone()), Some("text/plain"), None);
        assert!(matches!(
            view,
            BodyView::Ready(kind) if matches!(*kind, RenderKind::Binary { len: 200, .. })
        ));
    }

    // --- form ---

    #[test]
    fn form_parse_plus_and_percent() {
        let rows = parse_form(b"a=1&b=hello+world&c=%D0%9F&empty=");
        assert_eq!(
            rows,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "hello world".into()),
                ("c".into(), "П".into()),
                ("empty".into(), String::new()),
            ]
        );
        assert!(parse_form(b"").is_empty());
    }

    // --- JSON helpers ---

    #[test]
    fn json_node_counting_and_guard() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"a":1,"b":[1,2,{"c":null}],"d":"x"}"#).unwrap();
        // root + a + b + 3 элемента массива + c + d = 8.
        assert_eq!(count_json_nodes(&v), 8);
        assert!(json_tree_fits(100, 9));
        assert!(!json_tree_fits(JSON_TREE_MAX_BYTES + 1, 9));
        assert!(!json_tree_fits(100, JSON_TREE_MAX_NODES + 1));
    }

    #[test]
    fn pointer_escaping() {
        assert_eq!(pointer_escape("a/b"), "a~1b");
        assert_eq!(pointer_escape("a~b"), "a~0b");
        assert_eq!(pointer_escape("plain"), "plain");
        // Обратно: last_segment_unescaped.
        assert_eq!(last_segment_unescaped("/a~1b~0c"), Some("a/b~c".into()));
        assert_eq!(last_segment_unescaped(""), None);
        assert_eq!(parent_pointer("/a/0"), Some("/a"));
        assert_eq!(parent_pointer("/a"), Some(""));
        assert_eq!(parent_pointer(""), None);
    }

    #[test]
    fn json_tree_items_pointers_and_expansion() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"arr":[1,{"k":"v"}],"s":"x"}"#).unwrap();
        let items = json_tree_items(&v);
        assert_eq!(items.len(), 1);
        let root = &items[0];
        assert_eq!(root.id.as_ref(), "");
        assert!(root.is_expanded()); // корень раскрыт
        let child_ids: Vec<&str> = root.children.iter().map(|c| c.id.as_ref()).collect();
        assert_eq!(child_ids, vec!["/arr", "/s"]);
        // Вложенность: /arr/1/k
        let arr = &root.children[0];
        assert_eq!(arr.children[1].children[0].id.as_ref(), "/arr/1/k");
        assert_eq!(arr.children[1].children[0].label.as_ref(), "\"k\": \"v\"");
    }

    // --- hex preview ---

    #[test]
    fn hex_preview_cuts() {
        assert_eq!(hex_preview(&[0xDE, 0xAD, 0xBE, 0xEF], 8), "DE AD BE EF");
        assert_eq!(hex_preview(&[1, 2, 3], 2), "01 02 …");
    }

    // --- classify ---

    #[test]
    fn classify_mime_matrix() {
        assert_eq!(classify("application/json"), BodyClass::Json);
        assert_eq!(classify("application/vnd.api+json"), BodyClass::Json);
        assert_eq!(classify("text/json"), BodyClass::Json);
        assert_eq!(classify("image/png"), BodyClass::Image);
        assert_eq!(classify("image/webp"), BodyClass::Image);
        assert_eq!(classify("text/html"), BodyClass::Html);
        assert_eq!(classify("application/xml"), BodyClass::Xml);
        assert_eq!(classify("application/atom+xml"), BodyClass::Xml);
        assert_eq!(
            classify("application/x-www-form-urlencoded"),
            BodyClass::Form
        );
        assert_eq!(classify("text/plain"), BodyClass::Text);
        assert_eq!(classify("application/javascript"), BodyClass::Text);
        assert_eq!(classify("text/csv"), BodyClass::Text);
        assert_eq!(classify("application/octet-stream"), BodyClass::Binary);
        assert_eq!(classify(""), BodyClass::Binary);
    }

    // --- pretty indent ---

    #[test]
    fn xml_indent_nested() {
        let src = r#"<?xml version="1.0"?><root><a>1</a><b/><x:y v="2">text</x:y></root>"#;
        assert_eq!(
            indent_xml(src),
            "<?xml version=\"1.0\"?>\n<root>\n  <a>\n    1\n  </a>\n  <b/>\n  <x:y v=\"2\">\n    text\n  </x:y>\n</root>\n"
        );
    }

    #[test]
    fn html_indent_void_and_script() {
        let src = "<html><head><meta charset=\"utf-8\"></head><body><script>if (a<b) c();</script><p>hi<br>there</p></body></html>";
        let out = indent_html(src);
        assert!(out.contains("  <meta charset=\"utf-8\">\n"));
        // script — содержимое сырым блоком, закрывающий тег на своей строке:
        assert!(out.contains("<script>if (a<b) c();\n"));
        assert!(out.contains("</script>\n"));
        assert!(out.contains("    <p>\n      hi\n      <br>\n      there\n    </p>\n"));
    }

    #[test]
    fn truncate_str_char_boundaries() {
        assert_eq!(truncate_str("hello", 10), "hello");
        let s = "Д".repeat(10);
        assert_eq!(truncate_str(&s, 5), format!("{}…", "Д".repeat(5)));
    }

    // --- prepare_body: полный конвейер ---

    #[test]
    fn prepare_json_small_tree() {
        let bytes = br#"{"ok":true,"n":3}"#.to_vec();
        let (view, raw_view) = prepare_body(
            &bytes,
            Arc::new(bytes.clone()),
            Some("application/json"),
            None,
        );
        assert!(raw_view.is_none());
        match view {
            BodyView::Ready(kind) => match kind.as_ref() {
                RenderKind::JsonTree { value } => {
                    assert_eq!(value.pointer("/n").unwrap(), &serde_json::json!(3))
                }
                _ => panic!("expected JsonTree — got another variant"),
            },
            _ => panic!("expected Ready — got another variant"),
        }
    }

    #[test]
    fn prepare_json_too_many_nodes_falls_back_to_pretty() {
        let value =
            serde_json::Value::Array(vec![serde_json::Value::Null; JSON_TREE_MAX_NODES + 1]);
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(bytes.len() <= JSON_TREE_MAX_BYTES); // гварды именно по узлам
        let (view, _) = prepare_body(
            &bytes,
            Arc::new(bytes.clone()),
            Some("application/json"),
            None,
        );
        match view {
            BodyView::Ready(kind) => match kind.as_ref() {
                RenderKind::PrettyText { text, .. } => {
                    assert!(text.starts_with("[\n"));
                }
                _ => panic!("expected PrettyText fallback — got another variant"),
            },
            _ => panic!("expected Ready — got another variant"),
        }
    }

    #[test]
    fn prepare_invalid_json_is_text() {
        let bytes = b"{not json".to_vec();
        let (view, _) = prepare_body(
            &bytes,
            Arc::new(bytes.clone()),
            Some("application/json"),
            None,
        );
        match view {
            BodyView::Ready(kind) => {
                assert!(matches!(kind.as_ref(), RenderKind::PrettyText { .. }));
            }
            _ => panic!("expected Ready — got another variant"),
        }
    }

    #[test]
    fn prepare_text_with_charset() {
        let cp1251: Vec<u8> = vec![0xCF, 0xF0, 0xE8];
        let (view, _) = prepare_body(
            &cp1251,
            Arc::new(cp1251.clone()),
            Some("text/plain; charset=windows-1251"),
            None,
        );
        match view {
            BodyView::Ready(kind) => match kind.as_ref() {
                RenderKind::PrettyText {
                    text,
                    encoding_note,
                } => {
                    assert_eq!(text.as_str(), "При");
                    assert_eq!(encoding_note, "windows-1251");
                }
                _ => panic!("expected PrettyText — got another variant"),
            },
            _ => panic!("expected Ready — got another variant"),
        }
    }

    #[test]
    fn prepare_form() {
        let bytes = b"a=1&b=two".to_vec();
        let (view, _) = prepare_body(
            &bytes,
            Arc::new(bytes.clone()),
            Some("application/x-www-form-urlencoded"),
            None,
        );
        match view {
            BodyView::Ready(kind) => match kind.as_ref() {
                RenderKind::Form { rows } => {
                    assert_eq!(
                        rows.as_slice(),
                        [("a".into(), "1".into()), ("b".into(), "two".into())]
                    );
                }
                _ => panic!("expected Form — got another variant"),
            },
            _ => panic!("expected Ready — got another variant"),
        }
    }

    #[test]
    fn prepare_binary_by_nul() {
        let bytes = vec![0x1f, 0x8b, 0x00, 0x10];
        let (view, _) = prepare_body(
            &bytes,
            Arc::new(bytes.clone()),
            Some("application/octet-stream"),
            None,
        );
        match view {
            BodyView::Ready(kind) => match kind.as_ref() {
                RenderKind::Binary { len, preview } => {
                    assert_eq!(*len, 4);
                    assert_eq!(preview, "1F 8B 00 10");
                }
                _ => panic!("expected Binary — got another variant"),
            },
            _ => panic!("expected Ready — got another variant"),
        }
    }

    #[test]
    fn prepare_unknown_mime_without_nul_is_text() {
        let bytes = b"just words".to_vec();
        let (view, _) = prepare_body(&bytes, Arc::new(bytes.clone()), None, None);
        match view {
            BodyView::Ready(kind) => {
                assert!(matches!(kind.as_ref(), RenderKind::PrettyText { .. }));
            }
            _ => panic!("expected Ready — got another variant"),
        }
    }

    #[test]
    fn prepare_too_big_and_empty() {
        let big = vec![b'x'; PRETTY_LIMIT + 1];
        let (view, _) = prepare_body(&big, Arc::new(big.clone()), Some("text/plain"), None);
        assert!(matches!(view, BodyView::TooBig { len } if len == PRETTY_LIMIT + 1));

        let empty: Vec<u8> = Vec::new();
        let (view, _) = prepare_body(&empty, Arc::new(empty.clone()), Some("text/plain"), None);
        assert!(matches!(view, BodyView::Empty));
    }

    #[test]
    fn prepare_encoded_note_gives_raw_toggle_side() {
        // Сжатое тело + note → raw-сторона с hex-превью сжатых байтов.
        let raw = vec![0x1f, 0x8b, 0x08, 0x00];
        let decoded = b"{\"a\":1}".to_vec();
        let (view, raw_view) = prepare_body(
            &raw,
            Arc::new(decoded),
            Some("application/json"),
            Some("decoded from gzip, 4 B → 7 B".into()),
        );
        assert!(matches!(view, BodyView::Ready(_)));
        match raw_view {
            Some(BodyView::Ready(kind)) => match kind.as_ref() {
                RenderKind::Binary { len, preview } => {
                    assert_eq!(*len, 4);
                    assert_eq!(preview, "1F 8B 08 00");
                }
                _ => panic!("expected raw Binary — got another variant"),
            },
            _ => panic!("expected raw_view Ready — got another variant"),
        }
    }

    #[test]
    fn prepare_image_and_too_big_image() {
        // Реальный 2×2 PNG.
        let mut png = Vec::new();
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let (view, _) = prepare_body(&png, Arc::new(png.clone()), Some("image/png"), None);
        match view {
            BodyView::Ready(kind) => match kind.as_ref() {
                RenderKind::Image {
                    width,
                    height,
                    format,
                    ..
                } => {
                    assert_eq!((*width, *height, *format), (2, 2, "png"));
                }
                _ => panic!("expected Image — got another variant"),
            },
            _ => panic!("expected Ready — got another variant"),
        }

        // >20 МиБ — отказ до декодирования.
        let huge = vec![0u8; IMAGE_LIMIT + 1];
        let (view, _) = prepare_body(&huge, Arc::new(huge.clone()), Some("image/png"), None);
        assert!(matches!(view, BodyView::Ready(k) if matches!(*k, RenderKind::ImageTooBig { .. })));
    }

    #[test]
    fn decode_image_swaps_to_bgra() {
        let mut png = Vec::new();
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255])); // красный
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let decoded = decode_image(&png).expect("png decodes");
        // RenderImage хранит BGRA: красный (255,0,0) → (0,0,255).
        let bytes = decoded.image.as_bytes(0).expect("frame 0");
        assert_eq!(bytes, &[0, 0, 255, 255][..]);
    }
}
