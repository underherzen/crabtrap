//! Вкладка «Raw»: re-serialized wire-вид обмена.
//!
//! До сырого wire-tee (D7) точные байты провода не пишутся — здесь они
//! **пересобираются** из decoded-полей (`HttpRequest`/`HttpResponse`):
//! стартовая строка + заголовки в порядке приёма + тело. Это помечено
//! заметкой «re-serialized (wire framing not captured)» — chunked-рамки,
//! HPACK и порядок байтов не воспроизводятся.
//!
//! Разделы Request/Response показываются **стопкой** в одном скролле (без
//! переключателя): raw-вид — это транскрипт диалога, порядок чтения важен,
//! а тела инлайн ограничены [`INLINE_BODY_LIMIT`], так что общий размер
//! безопасен. Hex-вкладке, наоборот, нужен переключатель — там постраничный
//! навигатор на одно тело.
//!
//! Тела грузятся лениво через [`crate::ui::async_bridge::spawn_ui_task`] в
//! `RawState`-Entity (кэш слабых ссылок по `ExchangeId`: родитель —
//! `RenderOnce` и пересоздаётся каждый рендер, состояние должно жить между
//! рендерами; мёртвые записи вычищаются при каждом обращении).

use application::dto::capture::HeaderEntryDto;
use application::dto::inspection::canonical_reason;
use domain::entities::body::BodyRef;
use domain::value_objects::BodyId;
use domain::value_objects::http_vo::HttpVersionVo;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::{InteractiveElement as _, StatefulInteractiveElement as _};
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, RenderOnce, Styled, Window,
    div, px,
};

use super::{ExchangeContext, Side, tab_state};
use crate::ui::async_bridge::spawn_ui_task;
use crate::ui::theme::glass;

/// Лимит инлайн-рендера тела: до — целиком, свыше — первые байты + пометка.
pub const INLINE_BODY_LIMIT: usize = 256 * 1024;

// ---------------------------------------------------------------------------
// Чистые помощники рендера (тестируются напрямую).
// ---------------------------------------------------------------------------

/// Как показать тело в wire-рендере.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyText {
    /// Валидный UTF-8 — текст как есть.
    Text(String),
    /// Байты не являются UTF-8 — вместо текста заметка (дамп — вкладка Hex).
    Binary { size: usize },
    /// Текст больше [`INLINE_BODY_LIMIT`] — голова + пометка об усечении.
    Truncated { head: String, total: usize },
}

/// Валидный ли UTF-8 (lossy-рендер не делаем: смешанные байты в UI — мусор).
pub fn is_valid_utf8(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok()
}

/// Наибольший индекс `<= idx`, стоящий на границе UTF-8-символа.
///
/// Усечение тела не должно рвать многобайтный символ пополам (иначе
/// `String::from_utf8` падает). Стандартного `floor_char_boundary` ещё нет
/// в stable — своя.
pub fn floor_char_boundary(bytes: &[u8], idx: usize) -> usize {
    let mut i = idx.min(bytes.len());
    while i > 0 && i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
        i -= 1;
    }
    i
}

/// Проекция байт тела в рендерибельный вид (UTF-8 → текст, иначе заметка;
/// свыше лимита — голова + пометка).
pub fn body_text(bytes: &[u8]) -> BodyText {
    if !is_valid_utf8(bytes) {
        return BodyText::Binary { size: bytes.len() };
    }
    if bytes.len() <= INLINE_BODY_LIMIT {
        return BodyText::Text(String::from_utf8_lossy(bytes).into_owned());
    }
    let cut = floor_char_boundary(bytes, INLINE_BODY_LIMIT);
    BodyText::Truncated {
        head: String::from_utf8_lossy(&bytes[..cut]).into_owned(),
        total: bytes.len(),
    }
}

/// Стартовая строка запроса: `GET /api HTTP/1.1`.
pub fn request_line(method: &str, uri: &str, version: &str) -> String {
    format!("{method} {uri} {version}")
}

/// Стартовая строка ответа: `HTTP/1.1 404 Not Found`.
///
/// Причина — как пришла (`reason` домена); если движок её не захватил,
/// подставляем каноническую фразу (`canonical_reason`), иначе пусто.
pub fn status_line(version: &str, status: u16, reason: Option<&str>) -> String {
    let reason = reason
        .map(str::to_owned)
        .or_else(|| canonical_reason(status).map(str::to_owned))
        .unwrap_or_default();
    if reason.is_empty() {
        format!("{version} {status}")
    } else {
        format!("{version} {status} {reason}")
    }
}

/// Заголовки `Name: value` — в порядке приёма (DTO сохраняет порядок,
/// «as received»; сортировка по алфавиту ломала бы wire-порядок).
pub fn header_lines(headers: &[HeaderEntryDto]) -> Vec<String> {
    headers
        .iter()
        .map(|h| format!("{}: {}", h.name, h.value))
        .collect()
}

/// Строки тела: `…truncated`/binary-заметки идут отдельной строкой;
/// пустое тело — пусто (разделитель blank-строку рисует секция).
pub fn body_lines(text: &BodyText) -> Vec<String> {
    match text {
        BodyText::Text(s) => s.lines().map(str::to_owned).collect(),
        BodyText::Truncated { head, total } => {
            let mut lines: Vec<String> = head.lines().map(str::to_owned).collect();
            lines.push(format!("…truncated, {total} bytes total"));
            lines
        }
        BodyText::Binary { size } => vec![format!("[{size} bytes binary — see Hex]")],
    }
}

// ---------------------------------------------------------------------------
// Состояние: Entity в кэше слабых ссылок (ключ — ExchangeId).
// ---------------------------------------------------------------------------

/// Статус загрузки одного тела.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum BodyStatus {
    /// Ничего не запрошено (тела нет / size == 0 / ещё не дошли).
    #[default]
    Idle,
    Loading,
    Loaded(Vec<u8>),
    /// `get` вернул None — тело выселено/не сохранено.
    Missing,
    Error(String),
}

/// Слот одного тела + seq-защита от устаревших ответов.
#[derive(Debug, Default)]
struct BodySlot {
    id: Option<BodyId>,
    seq: u64,
    status: BodyStatus,
}

impl BodySlot {
    /// Запросить тело, если ещё не грузим/не грузили для этого `BodyRef`.
    ///
    /// Ответ применяется только при совпадении `BodyId` и `seq` — защита от
    /// устаревших задач после смены выбора/пересоздания ссылки.
    fn request(
        &mut self,
        entity: &Entity<RawState>,
        side: Side,
        body_ref: &BodyRef,
        repo: std::sync::Arc<dyn domain::repositories::BodyRepository>,
        runtime: tokio::runtime::Handle,
        cx: &mut Context<RawState>,
    ) {
        if self.id == Some(body_ref.id)
            && !matches!(self.status, BodyStatus::Idle | BodyStatus::Missing)
        {
            return; // уже грузим/загружено (или ошибка — не спамим повторами)
        }
        self.id = Some(body_ref.id);
        self.seq += 1;
        self.status = BodyStatus::Loading;
        let seq = self.seq;
        let (id, fetch_ref) = (body_ref.id, body_ref.clone());
        spawn_ui_task(
            cx,
            entity,
            runtime,
            async move { repo.get(&fetch_ref).await },
            move |state, result, cx| {
                let slot = state.slot_mut(side);
                if slot.id != Some(id) || slot.seq != seq {
                    return; // ответ на прошлый запрос/другое тело
                }
                slot.status = match result {
                    Ok(Some(bytes)) => BodyStatus::Loaded(bytes),
                    Ok(None) => BodyStatus::Missing,
                    Err(e) => BodyStatus::Error(e.to_string()),
                };
                cx.notify();
            },
        );
    }

    /// Сброс слота, когда тело сменилось/пропало.
    fn reset_if_other(&mut self, id: Option<BodyId>) {
        if self.id != id {
            *self = Self::default();
        }
    }
}

/// Состояние вкладки Raw для одного обмена.
#[derive(Debug, Default)]
struct RawState {
    request: BodySlot,
    response: BodySlot,
}

impl RawState {
    fn slot_mut(&mut self, side: Side) -> &mut BodySlot {
        match side {
            Side::Request => &mut self.request,
            Side::Response => &mut self.response,
        }
    }
}

// ---------------------------------------------------------------------------
// Вкладка.
// ---------------------------------------------------------------------------

/// Raw: re-serialized wire-вид обмена.
#[derive(gpui_kit::IntoElement)]
pub struct RawTab {
    ctx: ExchangeContext,
}

impl RawTab {
    pub fn new(ctx: ExchangeContext) -> Self {
        Self { ctx }
    }
}

impl RenderOnce for RawTab {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        // Сначала мутабельная часть (запуск догрузок) — только потом
        // заимствовать `cx.theme()`/`entity.read` на время сборки вёрстки.
        let detail = self.ctx.detail.clone();
        let entity = tab_state(detail.summary.id, self.ctx.side, cx, |cx| {
            cx.new(|_| RawState::default())
        });
        ensure_bodies(&entity, &self.ctx, cx);
        let theme = cx.theme();

        let state = entity.read(cx);
        let mono = theme.mono_font_family.clone();
        let mono_size = theme.mono_font_size;

        // Заметка о происхождении байтов — заметно, но не кричит. h2:
        // HPACK/фреймы не захватываются — показан эквивалент в виде HTTP/1.
        let h2 = detail.request.version == HttpVersionVo::H2
            || detail
                .response
                .as_ref()
                .is_some_and(|r| r.version == HttpVersionVo::H2);
        let note = if h2 {
            "HTTP/2 shown as text: frames and HPACK are not captured"
        } else {
            "re-serialized (wire framing not captured)"
        };
        let banner = h_flex()
            .items_center()
            .gap_1p5()
            .px_2()
            .py_0p5()
            .text_size(px(11.))
            .text_color(theme.foreground.opacity(0.75))
            .child(note);

        let content = match self.ctx.side {
            Side::Request => section(
                "raw-req-block",
                request_line(
                    &detail.request.method,
                    &detail.request.uri,
                    &detail.request.version.to_string(),
                ),
                header_lines(&detail.request.headers),
                body_view(&state.request),
                &mono,
                mono_size,
            ),
            Side::Response => match &detail.response {
                Some(resp) => section(
                    "raw-resp-block",
                    status_line(
                        &resp.version.to_string(),
                        resp.status,
                        resp.reason.as_deref(),
                    ),
                    header_lines(&resp.headers),
                    body_view(&state.response),
                    &mono,
                    mono_size,
                ),
                None => v_flex().child(super::placeholder("Waiting for the response…", &glass(cx))),
            },
        };

        v_flex()
            .size_full()
            .min_h(px(0.))
            .p(px(8.))
            .gap_2()
            .child(banner)
            .child(
                v_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .gap_2()
                    .id(("raw-scroll", self.ctx.side as usize))
                    .overflow_y_scroll()
                    .child(content.flex_none()),
            )
    }
}

/// Запустить недостающие загрузки тел (идемпотентно, вызывается каждый рендер).
fn ensure_bodies(entity: &Entity<RawState>, ctx: &ExchangeContext, cx: &mut App) {
    let detail = ctx.detail.clone();
    let repo = ctx.body_repo.clone();
    let runtime = ctx.runtime.clone();
    let req = detail.request.body.clone().filter(|r| r.size > 0);
    let resp = detail
        .response
        .as_ref()
        .and_then(|r| r.body.clone())
        .filter(|r| r.size > 0);

    entity.update(cx, |state, cx| {
        state.request.reset_if_other(req.as_ref().map(|r| r.id));
        state.response.reset_if_other(resp.as_ref().map(|r| r.id));
        let this = cx.entity();
        if let Some(r) = &req {
            state
                .request
                .request(&this, Side::Request, r, repo.clone(), runtime.clone(), cx);
        }
        if let Some(r) = &resp {
            state
                .response
                .request(&this, Side::Response, r, repo.clone(), runtime.clone(), cx);
        }
    });
}

// ---------------------------------------------------------------------------
// Кусочки рендера.
// ---------------------------------------------------------------------------

/// Монопольный блок одной секции: стартовая строка, заголовки (as received),
/// пустая строка, тело. Длинные строки не переносятся — горизонтальный скролл.
///
/// Возвращает конкретный `Div` (не `impl IntoElement`): обе ветки match в
/// render обязаны совпадать по типу.
fn section(
    block_id: &'static str,
    start_line: String,
    headers: Vec<String>,
    body: Vec<String>,
    mono: &gpui_kit::SharedString,
    size: gpui_kit::Pixels,
) -> gpui_kit::Div {
    let mut lines = Vec::with_capacity(headers.len() + body.len() + 2);
    lines.push(start_line);
    lines.extend(headers);
    lines.push(String::new()); // пустая строка между заголовками и телом
    lines.extend(body);

    let block = v_flex()
        .id(block_id)
        .font_family(mono.clone())
        .text_size(size)
        .overflow_x_scroll()
        .children(lines.iter().map(|line| {
            div().whitespace_nowrap().child(if line.is_empty() {
                "\u{00a0}".to_owned()
            } else {
                line.clone()
            })
        }));

    v_flex().child(div().border_1().rounded_md().px_2().py_1().child(block))
}

/// Строки тела по статусу слота (Loading/Missing/Error — служебные строки).
fn body_view(slot: &BodySlot) -> Vec<String> {
    match &slot.status {
        BodyStatus::Idle => Vec::new(),
        BodyStatus::Loading => vec!["…loading body".to_owned()],
        BodyStatus::Missing => vec!["[body not captured]".to_owned()],
        BodyStatus::Error(e) => vec![format!("[failed to load body: {e}]")],
        BodyStatus::Loaded(bytes) => body_lines(&body_text(bytes)),
    }
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
    fn request_line_is_method_uri_version() {
        assert_eq!(
            request_line("GET", "/api/v1/x?y=1", "HTTP/1.1"),
            "GET /api/v1/x?y=1 HTTP/1.1",
        );
    }

    #[test]
    fn status_line_prefers_captured_reason_then_canonical() {
        assert_eq!(
            status_line("HTTP/1.1", 200, Some("Fine")),
            "HTTP/1.1 200 Fine"
        );
        assert_eq!(status_line("HTTP/1.1", 404, None), "HTTP/1.1 404 Not Found");
        // Нестандартный код без reason — без выдуманной фразы.
        assert_eq!(status_line("HTTP/2", 599, None), "HTTP/2 599");
    }

    #[test]
    fn header_lines_keep_received_order() {
        let hs = vec![header("b", "2"), header("A", "1"), header("b", "3")];
        assert_eq!(
            header_lines(&hs),
            vec!["b: 2".to_owned(), "A: 1".to_owned(), "b: 3".to_owned()],
        );
    }

    #[test]
    fn utf8_check_and_char_boundary() {
        assert!(is_valid_utf8(b"hello"));
        assert!(is_valid_utf8("привет".as_bytes()));
        assert!(!is_valid_utf8(&[0x66, 0x6f, 0x6f, 0xff]));

        // «é» = 0xC3 0xA9: граница внутри символа откатывается к его началу.
        assert_eq!(floor_char_boundary(&[0x61, 0xC3, 0xA9], 3), 3);
        assert_eq!(floor_char_boundary(&[0x61, 0xC3, 0xA9], 2), 1);
        assert_eq!(floor_char_boundary(&[0x61], 9), 1);
        assert_eq!(floor_char_boundary(b"", 0), 0);
    }

    #[test]
    fn body_text_small_utf8_passes_through() {
        assert_eq!(
            body_text("line1\nline2".as_bytes()),
            BodyText::Text("line1\nline2".to_owned()),
        );
    }

    #[test]
    fn body_text_binary_gets_note() {
        assert_eq!(body_text(&[0x00, 0x01, 0xff]), BodyText::Binary { size: 3 },);
    }

    #[test]
    fn body_text_truncates_over_limit() {
        let bytes = vec![b'a'; INLINE_BODY_LIMIT + 10];
        match body_text(&bytes) {
            BodyText::Truncated { head, total } => {
                assert_eq!(total, INLINE_BODY_LIMIT + 10);
                assert_eq!(head.len(), INLINE_BODY_LIMIT);
            }
            other => panic!("expected truncation, got {other:?}"),
        }
    }

    #[test]
    fn body_text_truncation_never_splits_multibyte_char() {
        // Лимит, режущий четырёхбайтный символ (эмодзи) посередине.
        let mut bytes = vec![b' '; INLINE_BODY_LIMIT - 2];
        bytes.extend_from_slice("😀".as_bytes()); // 4 байта
        let out = body_text(&bytes);
        let head = match &out {
            BodyText::Truncated { head, .. } => head.clone(),
            other => panic!("expected truncation, got {other:?}"),
        };
        assert!(head.len() <= INLINE_BODY_LIMIT);
        assert!(std::str::from_utf8(head.as_bytes()).is_ok());
        assert!(!head.ends_with(char::REPLACEMENT_CHARACTER));
    }

    #[test]
    fn body_lines_render_notes_as_lines() {
        assert_eq!(
            body_lines(&BodyText::Text("a\n\nb".into())),
            vec!["a", "", "b"]
        );
        assert_eq!(
            body_lines(&BodyText::Binary { size: 7 }),
            vec!["[7 bytes binary — see Hex]"],
        );
        let truncated = BodyText::Truncated {
            head: "x".into(),
            total: 300_000,
        };
        assert_eq!(
            body_lines(&truncated),
            vec!["x".to_owned(), "…truncated, 300000 bytes total".to_owned()],
        );
    }
}
