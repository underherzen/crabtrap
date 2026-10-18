//! Вкладка «Hex»: постраничный hex-дамп тел обмена.
//!
//! Большие тела НЕ грузятся целиком: страница 4 KiB читается через
//! `BodyRepository::get_range` лениво (мост [`crate::ui::async_bridge`] на
//! движковом рантайме), страницы кэшируются в состоянии вкладки. Вкладка
//! рисует одну сторону (запрос и ответ — в своих панелях инспектора).
//!
//! Состояние живёт в Entity (как и в `raw.rs`: `RenderOnce`-родитель
//! пересоздаёт вкладку каждый рендер, поэтому entity живут в
//! [`super::tab_state`] по (`ExchangeId`, сторона)).
//!
//! Streaming-грань: пока тело ещё пишется (`BodyProgress`), `BodyRef.size` —
//! «захвачено на сейчас»; если `Content-Length` больше — под дампом подсказка
//! «captured so far: N bytes».

use std::collections::HashMap;

use application::dto::capture::HeaderEntryDto;
use domain::value_objects::BodyId;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, h_flex, v_flex};
use gpui_kit::prelude::{
    FluentBuilder as _, InteractiveElement as _, StatefulInteractiveElement as _,
};
use gpui_kit::{
    App, AppContext as _, Entity, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px,
};

use super::{ExchangeContext, Side, tab_state};
use crate::ui::async_bridge::spawn_ui_task;
use crate::ui::theme::{ToneName, glass};

/// Размер страницы дампа (байт).
pub const PAGE_SIZE: u64 = 4096;
/// Байт в строке дампа.
pub const BYTES_PER_ROW: usize = 16;

// ---------------------------------------------------------------------------
// Чистые помощники (постраничная математика + формат строк) — тестируются.
// ---------------------------------------------------------------------------

/// Число страниц по полному размеру (0 байт → 0 страниц).
pub fn page_count(total: u64) -> u64 {
    if total == 0 {
        0
    } else {
        total.div_ceil(PAGE_SIZE)
    }
}

/// Ограничить номер страницы допустимым диапазоном для размера `total`.
pub fn clamp_page(page: u64, total: u64) -> u64 {
    page.min(page_count(total).saturating_sub(1))
}

/// Длина среза страницы `page` (последняя — короче).
pub fn page_slice_len(page: u64, total: u64) -> u64 {
    let start = page.saturating_mul(PAGE_SIZE);
    total.saturating_sub(start).min(PAGE_SIZE)
}

/// Offset-колонка: 8 hex-цифр (`00001f40`).
pub fn format_offset(offset: u64) -> String {
    format!("{offset:08x}")
}

/// Hex-часть строки: 16 групп `xx`, после 8-й дополнительный пробел;
/// недостающие байты — пробелы-заполнители, чтобы ASCII-колонка не ездила.
pub fn hex_grouped(bytes: &[u8]) -> String {
    let mut out = String::new();
    for i in 0..BYTES_PER_ROW {
        if i == 8 {
            out.push(' ');
        }
        if i > 0 {
            out.push(' ');
        }
        match bytes.get(i) {
            Some(b) => out.push_str(&format!("{b:02x}")),
            None => out.push_str("  "),
        }
    }
    out
}

/// ASCII-колонка: печатаемый байт как есть, иначе `·`.
pub fn ascii_column(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if (0x20..=0x7e).contains(&b) {
                b as char
            } else {
                '·'
            }
        })
        .collect()
}

/// Одна строка дампа: `offset  hex…  |ascii|`.
pub fn hex_row(offset: u64, bytes: &[u8]) -> String {
    format!(
        "{}  {}  |{}|",
        format_offset(offset),
        hex_grouped(bytes),
        ascii_column(bytes),
    )
}

/// `Content-Length` из заголовков (case-insensitive; мусор пропускаем).
pub fn content_length(headers: &[HeaderEntryDto]) -> Option<u64> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-length"))
        .and_then(|h| h.value.trim().parse().ok())
}

/// Захват ещё идёт: заявленная длина больше захваченного (`BodyRef.size`).
pub fn capture_in_flight(headers: &[HeaderEntryDto], captured: u64) -> bool {
    content_length(headers).is_some_and(|declared| declared > captured)
}

// ---------------------------------------------------------------------------
// Состояние.
// ---------------------------------------------------------------------------

/// Направление тела — сторона обмена.
type Direction = Side;

/// Слот одной страницы.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PageSlot {
    Loading,
    Loaded(Vec<u8>),
    /// Диапазон недоступен (тело выселено/не сохранено).
    Missing,
    Error(String),
}

/// Состояние вкладки Hex для одного обмена.
#[derive(Debug, Default)]
struct HexState {
    direction: Direction,
    /// Текущая страница (общая на оба направления; кэш страниц раздельный).
    page: u64,
    /// Полный размер активного тела (синхронизируется каждый рендер).
    total: u64,
    /// Кэш страниц по направлениям — ключ: номер страницы.
    req_pages: HashMap<u64, PageSlot>,
    resp_pages: HashMap<u64, PageSlot>,
    /// BodyRef, под который кэш собран (смена ссылки → сброс кэша).
    req_id: Option<BodyId>,
    resp_id: Option<BodyId>,
}

impl HexState {
    fn pages_mut(&mut self, dir: Direction) -> &mut HashMap<u64, PageSlot> {
        match dir {
            Direction::Request => &mut self.req_pages,
            Direction::Response => &mut self.resp_pages,
        }
    }

    fn pages(&self, dir: Direction) -> &HashMap<u64, PageSlot> {
        match dir {
            Direction::Request => &self.req_pages,
            Direction::Response => &self.resp_pages,
        }
    }

    /// Сброс кэша направления, если тело сменилось/пропало.
    fn sync_body(&mut self, dir: Direction, id: Option<BodyId>) {
        let current = match dir {
            Direction::Request => &mut self.req_id,
            Direction::Response => &mut self.resp_id,
        };
        if *current != id {
            *current = id;
            *self.pages_mut(dir) = HashMap::new();
        }
    }
}

/// Загрузить недостающую страницу активного направления (каждый рендер).
fn ensure_page(entity: &Entity<HexState>, ctx: &ExchangeContext, cx: &mut App) {
    let detail = ctx.detail.clone();
    let repo = ctx.body_repo.clone();
    let runtime = ctx.runtime.clone();

    entity.update(cx, |state, cx| {
        let req = detail.request.body.clone().filter(|r| r.size > 0);
        let resp = detail
            .response
            .as_ref()
            .and_then(|r| r.body.clone())
            .filter(|r| r.size > 0);
        state.sync_body(Direction::Request, req.as_ref().map(|r| r.id));
        state.sync_body(Direction::Response, resp.as_ref().map(|r| r.id));

        let (dir, body_ref) = match state.direction {
            Direction::Request => (Direction::Request, req),
            Direction::Response => (Direction::Response, resp),
        };
        let Some(body_ref) = body_ref else {
            state.total = 0;
            state.page = 0;
            return;
        };
        state.total = body_ref.size;
        state.page = clamp_page(state.page, body_ref.size);

        let page = state.page;
        if state.pages(dir).contains_key(&page) {
            return; // кэш или уже грузим
        }
        state.pages_mut(dir).insert(page, PageSlot::Loading);
        let offset = page.saturating_mul(PAGE_SIZE);
        let len = page_slice_len(page, body_ref.size);
        let fetch_ref = body_ref.clone();
        let this = cx.entity();
        spawn_ui_task(
            cx,
            &this,
            runtime,
            async move { repo.get_range(&fetch_ref, offset, len).await },
            move |state, result, cx| {
                let Some(slot) = state.pages_mut(dir).get_mut(&page) else {
                    return; // кэш сброшен (сменилось тело) — результат не нужен
                };
                if !matches!(slot, PageSlot::Loading) {
                    return;
                }
                *slot = match result {
                    Ok(Some(bytes)) => PageSlot::Loaded(bytes),
                    Ok(None) => PageSlot::Missing,
                    Err(e) => PageSlot::Error(e.to_string()),
                };
                cx.notify();
            },
        );
    });
}

// ---------------------------------------------------------------------------
// Вкладка.
// ---------------------------------------------------------------------------

/// Hex: постраничный hex-дамп тела (Request/Response).
#[derive(gpui_kit::IntoElement)]
pub struct HexTab {
    ctx: ExchangeContext,
}

impl HexTab {
    pub fn new(ctx: ExchangeContext) -> Self {
        Self { ctx }
    }
}

impl RenderOnce for HexTab {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        // Мутабельная часть (догрузка страницы) — до заимствований на рендер.
        let detail = self.ctx.detail.clone();
        let side = self.ctx.side;
        let entity = tab_state(detail.summary.id, side, cx, |cx| {
            cx.new(|_| HexState {
                direction: side,
                ..HexState::default()
            })
        });
        ensure_page(&entity, &self.ctx, cx);
        let theme = cx.theme();
        let p = glass(cx);

        let state = entity.read(cx);
        let req_ref = detail.request.body.clone();
        let resp_ref = detail.response.as_ref().and_then(|r| r.body.clone());
        let dir = state.direction;
        let (body_ref, pages) = match dir {
            Direction::Request => (req_ref.as_ref(), &state.req_pages),
            Direction::Response => (resp_ref.as_ref(), &state.resp_pages),
        };

        // --- Тело отсутствует / пусто ----------------------------------
        let Some(body_ref) = body_ref else {
            return super::placeholder("No body", &p).into_any_element();
        };
        let total = body_ref.size;
        if total == 0 {
            return super::placeholder("Body not captured (empty or over the capture cap)", &p)
                .into_any_element();
        }

        // --- Навигация --------------------------------------------------
        let pages_total = page_count(total);
        let page = state.page;
        let start = page.saturating_mul(PAGE_SIZE);
        let end = (start + page_slice_len(page, total)).saturating_sub(1);

        let nav = h_flex()
            .gap_1()
            .items_center()
            .child(nav_button(&entity, Nav::First, "|<", page == 0))
            .child(nav_button(&entity, Nav::Prev, "<", page == 0))
            .child(div().px_1().text_size(px(12.)).child(format!(
                "page {}/{}",
                page + 1,
                pages_total
            )))
            .child(nav_button(&entity, Nav::Next, ">", page + 1 >= pages_total))
            .child(nav_button(
                &entity,
                Nav::Last,
                ">|",
                page + 1 >= pages_total,
            ))
            .child(
                div()
                    .px_1()
                    .text_size(px(12.))
                    .text_color(theme.foreground.opacity(0.7))
                    .child(format!("bytes {start}–{end} of {total}")),
            );

        // --- Дамп страницы ----------------------------------------------
        let mono = theme.mono_font_family.clone();
        let mono_size = theme.mono_font_size;
        let slot = pages.get(&page);
        let block = v_flex()
            .flex_1()
            .min_h(px(0.))
            .id("hex-dump")
            .overflow_y_scroll()
            .font_family(mono.clone())
            .text_size(mono_size)
            .child(match slot {
                None => div().child(""),
                Some(PageSlot::Loading) => div()
                    .text_color(p.text_secondary)
                    .child(format!("loading page {}…", page + 1)),
                Some(PageSlot::Missing) => div()
                    .text_color(p.tone(ToneName::Orange).fg)
                    .child("page unavailable — body not captured"),
                Some(PageSlot::Error(e)) => div()
                    .text_color(p.tone(ToneName::Red).fg)
                    .child(format!("failed to load page: {e}")),
                Some(PageSlot::Loaded(bytes)) => {
                    div().children(bytes.chunks(BYTES_PER_ROW).enumerate().map(|(i, chunk)| {
                        div()
                            .whitespace_nowrap()
                            .child(hex_row(start + (i as u64) * BYTES_PER_ROW as u64, chunk))
                    }))
                }
            });

        // --- Футер: размеры + streaming-подсказка -----------------------
        let headers: &[HeaderEntryDto] = match dir {
            Direction::Request => detail.request.headers.as_slice(),
            Direction::Response => detail
                .response
                .as_ref()
                .map(|r| r.headers.as_slice())
                .unwrap_or(&[]),
        };
        let streaming_hint = capture_in_flight(headers, total).then(|| {
            div()
                .text_size(px(12.))
                .text_color(p.tone(ToneName::Orange).fg)
                .child(format!(
                    "body still streaming — captured so far: {total} bytes"
                ))
        });

        v_flex()
            .size_full()
            .min_h(px(0.))
            .p(px(8.))
            .gap_2()
            .child(nav)
            .child(
                div()
                    .border_1()
                    .rounded_md()
                    .px_2()
                    .py_1()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(block),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(theme.foreground.opacity(0.7))
                    .child(format!("total: {total} bytes"))
                    .child(format!("page size: {PAGE_SIZE} bytes"))
                    .child(format!("row: {BYTES_PER_ROW} bytes")),
            )
            .when_some(streaming_hint, |this, hint| this.child(hint))
            .into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Кусочки рендера.
// ---------------------------------------------------------------------------

/// Шаг навигатора.
#[derive(Clone, Copy)]
enum Nav {
    First,
    Prev,
    Next,
    Last,
}

fn nav_button(entity: &Entity<HexState>, nav: Nav, label: &str, disabled: bool) -> Button {
    let entity = entity.clone();
    // ElementId требует 'static — &str живёт только в теле функции.
    Button::new(label.to_owned())
        .label(label)
        .compact()
        .ghost()
        .disabled(disabled)
        .on_click(move |_, _, cx| {
            entity.update(cx, |s, cx| {
                let pages_total = page_count(s.total);
                let target = match nav {
                    Nav::First => 0,
                    Nav::Prev => s.page.saturating_sub(1),
                    Nav::Next => (s.page + 1).min(pages_total.saturating_sub(1)),
                    Nav::Last => pages_total.saturating_sub(1),
                };
                if target != s.page {
                    s.page = target;
                    cx.notify();
                }
            });
        })
}

// ---------------------------------------------------------------------------
// Тесты.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, value: &str) -> HeaderEntryDto {
        HeaderEntryDto {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    #[test]
    fn page_count_covers_partial_last_page() {
        assert_eq!(page_count(0), 0);
        assert_eq!(page_count(1), 1);
        assert_eq!(page_count(PAGE_SIZE), 1);
        assert_eq!(page_count(PAGE_SIZE + 1), 2);
        assert_eq!(page_count(3 * PAGE_SIZE), 3);
    }

    #[test]
    fn clamp_page_stays_in_range() {
        assert_eq!(clamp_page(0, 0), 0);
        assert_eq!(clamp_page(3, PAGE_SIZE), 0);
        assert_eq!(clamp_page(9, 2 * PAGE_SIZE), 1);
        assert_eq!(clamp_page(u64::MAX, PAGE_SIZE), 0);
    }

    #[test]
    fn page_slice_len_is_capped_by_total() {
        assert_eq!(page_slice_len(0, 10), 10);
        assert_eq!(page_slice_len(0, 2 * PAGE_SIZE), PAGE_SIZE);
        assert_eq!(page_slice_len(1, PAGE_SIZE + 5), 5);
        assert_eq!(page_slice_len(7, 10), 0); // за пределами
    }

    #[test]
    fn offsets_are_8_hex_digits() {
        assert_eq!(format_offset(0), "00000000");
        assert_eq!(format_offset(0x1f40), "00001f40");
        assert_eq!(format_offset(0xdead_beef), "deadbeef");
    }

    #[test]
    fn hex_grouping_is_8_plus_8_and_padded() {
        let bytes: Vec<u8> = (0x10..0x1fu8).collect(); // 10..1f (15 байт)
        let s = hex_grouped(&bytes);
        assert!(s.starts_with("10 11 12 13 14 15 16 17  18 19"));
        // Полная строка: 16 групп по 2 + 15 разделителей + 1 дополнительный
        // пробел между половинами.
        let full = hex_grouped(&[0u8; 16]);
        assert_eq!(full.chars().filter(|c| *c == ' ').count(), 16);
        // Хвост — заполнители пробелами, ширина как у полной строки.
        assert_eq!(s.len(), full.len());
        assert_eq!(s.chars().filter(|c| *c == ' ').count(), 18);
        assert!(s.ends_with("1e   ")); // последний байт + 2 заполнителя
    }

    #[test]
    fn ascii_column_marks_non_printables() {
        assert_eq!(ascii_column(b"AB~"), "AB~");
        assert_eq!(ascii_column(&[0x00, 0x41, 0x7f, 0xff]), "·A··");
        assert_eq!(ascii_column(&[0x20, 0x7e]), " ~");
    }

    #[test]
    fn hex_row_composes_all_columns() {
        let row = hex_row(0x1f40, b"AB");
        assert!(row.starts_with("00001f40  41 42"));
        assert!(row.ends_with("|AB|"));
    }

    #[test]
    fn content_length_is_case_insensitive_and_tolerant() {
        let hs = vec![header("content-LENGTH", " 12345 ")];
        assert_eq!(content_length(&hs), Some(12345));
        assert_eq!(content_length(&[header("content-length", "abc")]), None);
        assert_eq!(content_length(&[header("host", "x")]), None);
    }

    #[test]
    fn capture_in_flight_when_declared_exceeds_captured() {
        let hs = vec![header("Content-Length", "1000")];
        assert!(capture_in_flight(&hs, 40));
        assert!(!capture_in_flight(&hs, 1000));
        assert!(!capture_in_flight(&[header("Host", "x")], 40));
    }

    #[test]
    fn direction_default_is_request_with_labels() {
        assert_eq!(Direction::default(), Direction::Request);
        assert_eq!(Direction::Request.label(), "Request");
        assert_eq!(Direction::Response.label(), "Response");
    }
}
