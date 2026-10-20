//! Делегат таблицы обменов (gpui-kit `DataTable`, виртуализация UniformList).
//!
//! Колонки: точка состояния · Client (иконка + приложение) · Method
//! (плашка) · Status (плашка) · Host (замок у HTTPS) · Path · Type (тип
//! содержимого) · Size · Time · Tags (плашки).
//! Цвета — из палитры ([`crate::ui::theme`]); у выделенной строки текст
//! белый на акценте. Данные читаются из [`TrafficStore`] по индексам
//! видимых строк; клик по строке выделяет обмен.

use gpui_kit::AppContext as _;
use gpui_kit::InteractiveElement as _;
use gpui_kit::StatefulInteractiveElement as _;
use gpui_kit::base::actions::{SelectDown, SelectPageDown, SelectPageUp, SelectUp};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, App, Context, Div, Entity, Hsla, IntoElement, ParentElement, SharedString, Styled,
    Window, div, img, px,
};

use super::apps::ClientApp;
use super::quick_filter::row_kind;
use super::store::{TrafficRow, TrafficStore};
use crate::ui::clients::ClientsStore;
use crate::ui::theme::components::{dot, pill, tone_pill};
use crate::ui::theme::icons::AppIcon;
use crate::ui::theme::metrics::{TEXT, TEXT_SM};
use crate::ui::theme::semantic::{ContentKind, ExchangeState, method_tone, tag_tone};
use crate::ui::theme::{Palette, Tone, glass};

/// Индексы колонок (порядок в таблице).
pub const COL_STATE: usize = 0;
pub const COL_CLIENT: usize = 1;
pub const COL_METHOD: usize = 2;
pub const COL_STATUS: usize = 3;
pub const COL_HOST: usize = 4;
pub const COL_PATH: usize = 5;
pub const COL_TYPE: usize = 6;
pub const COL_SIZE: usize = 7;
pub const COL_TIME: usize = 8;
pub const COL_TAGS: usize = 9;
pub const COLS: usize = 10;

/// Цвет статуса в палитре (инспектор): по классу ответа.
pub fn status_color(status: Option<u16>, failed: bool, p: &Palette) -> Hsla {
    p.tone(ExchangeState::of(status, failed).tone()).fg
}

/// Человекочитаемый размер (Б/КБ/МБ).
pub fn human_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// Подпись плашки статуса: код, `ERR` (провал) или `…` (ждём ответ).
pub fn status_label(row: &TrafficRow) -> String {
    if row.failed.is_some() {
        "ERR".into()
    } else {
        row.summary
            .status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "…".into())
    }
}

/// Длительность: `…` пока идёт, `ms` до секунды, дальше секунды.
pub fn duration_label(ms: f64) -> String {
    if ms <= 0.0 {
        "…".into()
    } else if ms < 1000.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

/// Делегат: тонкая проекция TrafficStore в API таблицы.
pub struct ExchangeTableDelegate {
    store: Entity<TrafficStore>,
}

impl ExchangeTableDelegate {
    pub fn new(store: Entity<TrafficStore>) -> Self {
        Self { store }
    }
}

/// Цвета ячеек строки: на выделенной — белый текст и белые плашки.
struct RowColors {
    selected: bool,
    text: Hsla,
    secondary: Hsla,
}

impl RowColors {
    fn new(selected: bool, p: &Palette) -> Self {
        if selected {
            Self {
                selected,
                text: p.selection_fg,
                secondary: p.selection_fg.opacity(0.85),
            }
        } else {
            Self {
                selected,
                text: p.text,
                secondary: p.text_secondary,
            }
        }
    }

    fn tone(&self, tone: Tone) -> Tone {
        if self.selected {
            Tone {
                fg: self.text,
                bg: self.text.opacity(0.22),
            }
        } else {
            tone
        }
    }
}

impl TableDelegate for ExchangeTableDelegate {
    /// Правый клик: правило для этого запроса (Map Local / Map Remote /
    /// Block / SSL Proxying) с подставленными хостом и путём.
    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: gpui_kit::component::menu::PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui_kit::component::menu::PopupMenu {
        let store = self.store.read(cx);
        let Some(row) = store.visible_row(row_ix) else {
            return menu;
        };
        let pinned = store.pinned.contains(&row.summary.host);
        let summary = row.summary.clone();
        super::context_menu::exchange_menu(menu, &summary, pinned, window, cx)
    }

    /// Подсветка выбранной строки по id обмена — на случай, когда TableState
    /// забыл индекс (см. синхронизацию в [`build_traffic_table`]).
    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui_kit::Stateful<Div> {
        let store = self.store.read(cx);
        let selected = store
            .visible_row(row_ix)
            .is_some_and(|row| store.selected == Some(row.summary.id));
        let row = div().id(("row", row_ix));
        if selected {
            row.bg(glass(cx).selection)
        } else {
            row
        }
    }

    fn columns_count(&self, _: &App) -> usize {
        COLS
    }

    fn rows_count(&self, cx: &App) -> usize {
        self.store.read(cx).visible_len()
    }

    fn column(&self, ix: usize, _: &App) -> Column {
        match ix {
            COL_STATE => Column::new("state", "").width(px(26.)).resizable(false),
            COL_CLIENT => Column::new("client", "Client").width(px(150.)),
            COL_METHOD => Column::new("method", "Method").width(px(78.)),
            COL_STATUS => Column::new("status", "Status").width(px(64.)),
            COL_HOST => Column::new("host", "Host").width(px(210.)),
            COL_PATH => Column::new("path", "Path").width(px(380.)),
            COL_TYPE => Column::new("type", "Type").width(px(86.)),
            COL_SIZE => Column::new("size", "Size").width(px(82.)).text_right(),
            COL_TIME => Column::new("time", "Time").width(px(76.)).text_right(),
            COL_TAGS => Column::new("tags", "Tags").width(px(180.)),
            _ => Column::new("x", "").width(px(24.)),
        }
    }

    fn render_td(
        &mut self,
        view_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let p = glass(cx);
        let store = self.store.read(cx);
        let Some(row) = store.visible_row(view_ix) else {
            return div().into_any_element();
        };
        let colors = RowColors::new(store.selected == Some(row.summary.id), &p);
        if col_ix == COL_CLIENT {
            return client_cell(row, &colors, cx);
        }
        render_cell(view_ix, row, col_ix, &colors, &p)
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let p = glass(cx);
        let filtered = !self.store.read(cx).rows.is_empty();
        let (title, hint) = if filtered {
            (
                "No matching requests",
                "Clear the filter or pick All in the sidebar",
            )
        } else {
            (
                "No traffic yet",
                "Point a client at the proxy or turn on macOS Proxy",
            )
        };
        h_flex()
            .size_full()
            .justify_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(TEXT))
                            .text_color(p.text_secondary)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_SM))
                            .text_color(p.text_tertiary)
                            .child(hint),
                    ),
            )
            .into_any_element()
    }
}

fn render_cell(
    view_ix: usize,
    row: &TrafficRow,
    col_ix: usize,
    colors: &RowColors,
    p: &Palette,
) -> AnyElement {
    let s = &row.summary;
    let state = ExchangeState::of(s.status, row.failed.is_some());
    match col_ix {
        COL_STATE => cell()
            .justify_center()
            .child(dot(
                if colors.selected {
                    colors.text
                } else {
                    p.tone(state.tone()).fg
                },
                7.,
            ))
            .into_any_element(),
        COL_METHOD => cell()
            .child(tone_pill(
                s.method.clone(),
                colors.tone(p.tone(method_tone(&s.method))),
            ))
            .into_any_element(),
        COL_STATUS => cell()
            .child(tone_pill(
                status_label(row),
                colors.tone(p.tone(state.tone())),
            ))
            .into_any_element(),
        COL_HOST => {
            let lock = s.tls_mitm.map(|mitm| {
                let (icon, color) = if mitm {
                    (AppIcon::Lock, colors.secondary)
                } else {
                    (AppIcon::LockOpen, p.text_tertiary)
                };
                Icon::new(icon).xsmall().text_color(color)
            });
            cell()
                .gap(px(5.))
                .children(lock)
                .child(full_on_hover(
                    ("cell-host", view_ix),
                    s.host.clone(),
                    colors.text,
                ))
                .into_any_element()
        }
        COL_PATH => cell()
            .child(full_on_hover(
                ("cell-path", view_ix),
                s.path.clone(),
                colors.secondary,
            ))
            .into_any_element(),
        COL_TYPE => {
            let kind = row_kind(row);
            let label = if row.content_type.is_none() && kind == ContentKind::Other {
                String::new()
            } else {
                kind.label().to_string()
            };
            let color = if colors.selected {
                colors.text
            } else {
                p.tone(kind.tone()).fg
            };
            cell().child(text(label, color)).into_any_element()
        }
        COL_SIZE => cell()
            .justify_end()
            .child(text(
                if s.resp_size > 0 {
                    human_bytes(s.resp_size)
                } else {
                    "…".into()
                },
                colors.secondary,
            ))
            .into_any_element(),
        COL_TIME => cell()
            .justify_end()
            .child(text(duration_label(s.duration_ms), colors.secondary))
            .into_any_element(),
        COL_TAGS => cell()
            .gap(px(4.))
            .children(s.tags.iter().map(|tag| {
                let tone = colors.tone(p.tone(tag_tone(tag)));
                pill(tag.clone(), tone.fg, tone.bg)
            }))
            .into_any_element(),
        _ => div().into_any_element(),
    }
}

/// Клиент: иконка приложения (или значок устройства) и имя.
fn client_cell(row: &TrafficRow, colors: &RowColors, cx: &App) -> AnyElement {
    let Some(client) = &row.client else {
        return cell().into_any_element();
    };
    cell()
        .gap(px(6.))
        .child(client_icon(client, colors.secondary, cx))
        .child(text(client.name.clone(), colors.secondary))
        .into_any_element()
}

/// Иконка клиента 14×14: картинка приложения, иначе значок.
pub fn client_icon(client: &ClientApp, color: Hsla, cx: &App) -> AnyElement {
    let image = client
        .icon_key
        .as_deref()
        .and_then(|key| ClientsStore::icon(key, cx));
    match image {
        Some(image) => img(image).flex_none().size(px(14.)).into_any_element(),
        None => Icon::new(if client.remote {
            IconName::Globe
        } else {
            IconName::SquareTerminal
        })
        .xsmall()
        .text_color(color)
        .into_any_element(),
    }
}

fn cell() -> Div {
    div()
        .h_full()
        .w_full()
        .flex()
        .items_center()
        .overflow_hidden()
}

fn text(label: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .min_w(px(0.))
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .text_size(px(TEXT_SM))
        .text_color(color)
        .child(label.into())
}

/// Текст ячейки, который может не влезть: полностью — в подсказке.
fn full_on_hover(
    id: impl Into<gpui_kit::ElementId>,
    label: impl Into<SharedString>,
    color: Hsla,
) -> gpui_kit::Stateful<Div> {
    let label: SharedString = label.into();
    let full = label.clone();
    text(label, color).id(id).tooltip(move |window, cx| {
        gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx)
    })
}

/// Сборка таблицы: состояние + подписка на выбор строки.
///
/// Вызывается из `open_window` (TableState требует `&mut Window`); клик по
/// строке пишет `TrafficStore.selected` — инспектор грузит детали.
pub fn build_traffic_table(
    store: &Entity<TrafficStore>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TableState<ExchangeTableDelegate>> {
    let table = cx.new(|cx| {
        TableState::new(ExchangeTableDelegate::new(store.clone()), window, cx)
            .row_selectable(true)
            .col_movable(false)
    });
    cx.subscribe(&table, {
        let store = store.clone();
        move |_, event: &TableEvent, cx| {
            if let TableEvent::SelectRow(view_ix) = event {
                store.update(cx, |state, cx| {
                    state.selected = state.visible_row(*view_ix).map(|r| r.summary.id);
                    cx.notify();
                });
            }
        }
    })
    .detach();
    // TableState держит ИНДЕКС выбранной строки (подсветка, клавиши ↑↓), а
    // фильтр/чипы/панель и вытеснение старых строк сдвигают видимые строки.
    // Синхронизация с выбранным обменом:
    // - смена вида — переносим выбор (таблица докрутит до строки) или
    //   снимаем, если обмен скрыт;
    // - сдвиг от новых строк — переносим, только если строка на экране
    //   (без прокрутки), иначе TableState забывает индекс: подсветку по id
    //   всё равно рисует `render_tr`, а вид пользователя не дёргается.
    let synced_generation = std::rc::Rc::new(std::cell::Cell::new(0u64));
    cx.observe(store, {
        let table = table.downgrade();
        move |store, cx| {
            let (generation, desired) = {
                let store = store.read(cx);
                (store.view_generation, store.selected_view_ix())
            };
            let view_changed = synced_generation.replace(generation) != generation;
            let _ = table.update(cx, |table, cx| {
                if table.selected_row() == desired {
                    return;
                }
                let on_screen =
                    desired.is_some_and(|ix| table.visible_range().rows().contains(&ix));
                match desired {
                    Some(ix) if view_changed || on_screen => table.set_selected_row(ix, cx),
                    _ if table.selected_row().is_some() => table.clear_selection(cx),
                    _ => {}
                }
            });
        }
    })
    .detach();
    table
}

/// Куда сдвинуть выбор клавишей: от позиции выбранного обмена (`selected`,
/// по id — верна и после вытеснения), иначе от индекса TableState; без
/// выбора — первая строка. Результат в пределах `rows`.
pub fn nav_target(
    selected: Option<usize>,
    table_row: Option<usize>,
    delta: isize,
    rows: usize,
) -> Option<usize> {
    let last = rows.checked_sub(1)?;
    Some(match selected.or(table_row) {
        Some(from) => from.saturating_add_signed(delta).min(last),
        None => 0,
    })
}

/// Сдвиг выбора на `delta` строк (−1/+1, ±страница).
fn move_selection(
    table: &Entity<TableState<ExchangeTableDelegate>>,
    store: &Entity<TrafficStore>,
    delta: impl Fn(usize) -> isize,
    cx: &mut App,
) {
    let (selected, rows) = {
        let store = store.read(cx);
        (store.selected_view_ix(), store.visible_len())
    };
    table.update(cx, |table, cx| {
        let page = table.visible_range().rows().len().max(1);
        if let Some(target) = nav_target(selected, table.selected_row(), delta(page), rows) {
            table.set_selected_row(target, cx);
        }
    });
}

/// Рендер таблицы: DataTable + собственная навигация ↑↓/PageUp/PageDown.
///
/// Свои клавиши TableState ведёт от СВОЕГО индекса, а тот после вытеснения
/// старых строк или смены вида бывает сброшен (см. синхронизацию в
/// [`build_traffic_table`]) — тогда ↓ прыгал на первую строку. Действия
/// перехватываются на фазе capture и считаются от выбранного обмена.
pub fn traffic_table(
    table: &Entity<TableState<ExchangeTableDelegate>>,
    store: &Entity<TrafficStore>,
) -> impl IntoElement {
    macro_rules! nav {
        ($el:expr, $action:ty, $delta:expr) => {{
            let (table, store) = (table.clone(), store.clone());
            $el.capture_action(move |_: &$action, _, cx| {
                cx.stop_propagation();
                move_selection(&table, &store, $delta, cx);
            })
        }};
    }
    let view = div().size_full();
    let view = nav!(view, SelectUp, |_| -1);
    let view = nav!(view, SelectDown, |_| 1);
    let view = nav!(view, SelectPageUp, |page| -(page as isize));
    let view = nav!(view, SelectPageDown, |page| page as isize);
    view.child(DataTable::new(table).stripe(true).bordered(false).xsmall())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::ToneName;
    use crate::ui::traffic::store::test_row;

    #[test]
    fn status_label_states() {
        let mut row = test_row("a.test", "/");
        assert_eq!(status_label(&row), "…");
        row.summary.status = Some(404);
        assert_eq!(status_label(&row), "404");
        row.failed = Some("reset".into());
        assert_eq!(status_label(&row), "ERR");
    }

    #[test]
    fn navigation_starts_from_the_selected_exchange() {
        // TableState забыл индекс (None), обмен на 500-й строке: ↓ → 501.
        assert_eq!(nav_target(Some(500), None, 1, 10_000), Some(501));
        // Индекс TableState устарел (сдвиг после вытеснения) — верим id.
        assert_eq!(nav_target(Some(480), Some(500), -1, 10_000), Some(479));
        assert_eq!(
            nav_target(None, Some(3), 1, 10),
            Some(4),
            "selection hidden by filter"
        );
        assert_eq!(nav_target(None, None, 1, 10), Some(0));
        assert_eq!(
            nav_target(Some(9), None, 1, 10),
            Some(9),
            "clamped at the end"
        );
        assert_eq!(
            nav_target(Some(2), None, -20, 10),
            Some(0),
            "page up clamps at 0"
        );
        assert_eq!(nav_target(Some(0), None, 1, 0), None, "empty table");
    }

    #[test]
    fn duration_formats() {
        assert_eq!(duration_label(0.0), "…");
        assert_eq!(duration_label(12.4), "12 ms");
        assert_eq!(duration_label(1530.0), "1.53 s");
    }

    #[test]
    fn status_color_follows_class() {
        let p = Palette::light();
        assert_eq!(
            status_color(Some(200), false, &p),
            p.tone(ToneName::Green).fg
        );
        assert_eq!(status_color(Some(500), false, &p), p.tone(ToneName::Red).fg);
        assert_eq!(status_color(Some(200), true, &p), p.tone(ToneName::Red).fg);
    }
}
