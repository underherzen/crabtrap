//! Таблица правил вкладки Tools (как в Charles): галочка Enable инструмента,
//! таблица [☑ | Location | Target], кнопки Add… / Edit… / Duplicate / Remove.
//!
//! Строка: клик — выделить, двойной клик — правка, Checkbox — вкл/выкл
//! правила. Edit / Duplicate / Remove работают с выделенной строкой.

use domain::value_objects::RuleId;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement,
    StatefulInteractiveElement as _, Styled, Window, div, px,
};

use crate::ui::theme::glass;
use crate::ui::theme::metrics::{RADIUS_LG, TEXT, TEXT_SM};

/// Высота строки таблицы.
const ROW_HEIGHT: f32 = 28.;
/// Колонка галочки enabled.
const CHECK_COLUMN: f32 = 40.;

/// Строка таблицы правил.
#[derive(Debug, Clone)]
pub struct RuleRow {
    pub id: RuleId,
    pub enabled: bool,
    pub location: String,
    pub target: String,
}

/// Действие над строкой правила.
pub type RuleHandler = std::rc::Rc<dyn Fn(RuleId, &mut App)>;
/// Открытие редактора правила (нужно окно).
pub type RuleWindowHandler = std::rc::Rc<dyn Fn(RuleId, &mut Window, &mut App)>;

/// Обработчики таблицы (замыкания строятся вкладкой).
pub struct TableHandlers {
    pub select: RuleHandler,
    pub toggle: RuleHandler,
    pub open: RuleWindowHandler,
}

/// Галочка «Enable <инструмент>» над таблицей.
pub fn enable_checkbox(
    id: &'static str,
    label: &'static str,
    checked: bool,
    on_change: impl Fn(bool, &mut App) + 'static,
) -> AnyElement {
    Checkbox::new(id)
        .label(label)
        .checked(checked)
        .on_click(move |checked, _, cx| on_change(*checked, cx))
        .into_any_element()
}

/// Таблица правил на всю ширину.
pub fn rule_table(
    id: &'static str,
    columns: (&'static str, &'static str),
    rows: &[RuleRow],
    selected: Option<RuleId>,
    empty: &'static str,
    handlers: &TableHandlers,
    cx: &App,
) -> AnyElement {
    let p = glass(cx);
    let header = div()
        .flex()
        .items_center()
        .h(px(26.))
        .bg(p.header)
        .border_b_1()
        .border_color(p.hairline)
        .text_size(px(TEXT_SM))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(p.text_secondary)
        .child(div().w(px(CHECK_COLUMN)).flex_none())
        .child(div().flex_1().px_2().child(columns.0))
        .child(div().flex_1().px_2().child(columns.1));
    let table = div()
        .flex()
        .flex_col()
        .rounded(px(RADIUS_LG))
        .overflow_hidden()
        .border_1()
        .border_color(p.hairline)
        .bg(p.content)
        .child(header);
    if rows.is_empty() {
        return table
            .child(
                div()
                    .px_3()
                    .py(px(14.))
                    .text_size(px(TEXT))
                    .text_color(p.text_secondary)
                    .child(empty),
            )
            .into_any_element();
    }
    let (selection_bg, stripe) = (p.selection, p.stripe);
    table
        .children(rows.iter().enumerate().map(|(ix, row)| {
            let is_selected = selected == Some(row.id);
            let rule_id = row.id;
            let (select, open, toggle) = (
                handlers.select.clone(),
                handlers.open.clone(),
                handlers.toggle.clone(),
            );
            let text = if is_selected {
                p.selection_fg
            } else if row.enabled {
                p.text
            } else {
                p.text_tertiary
            };
            div()
                .id((id, ix))
                .flex()
                .items_center()
                .h(px(ROW_HEIGHT))
                .text_size(px(TEXT))
                .text_color(text)
                .when(ix % 2 == 1 && !is_selected, |r| r.bg(stripe))
                .when(!is_selected, |r| r.hover(|s| s.bg(p.row_hover)))
                .when(is_selected, |r| r.bg(selection_bg))
                .on_click(move |event: &ClickEvent, window, cx| {
                    select(rule_id, cx);
                    if event.click_count() >= 2 {
                        open(rule_id, window, cx);
                    }
                })
                .child(
                    div()
                        .w(px(CHECK_COLUMN))
                        .flex_none()
                        .flex()
                        .justify_center()
                        .child(
                            Checkbox::new((id, ix + 100_000))
                                .checked(row.enabled)
                                .on_click(move |_, _, cx| toggle(rule_id, cx)),
                        ),
                )
                .child(clipped(("rule-location", ix), row.location.clone()))
                .child(clipped(("rule-target", ix), row.target.clone()))
        }))
        .into_any_element()
}

/// Ячейка, которая может не влезть: полностью — в подсказке.
fn clipped(id: (&'static str, usize), text: String) -> impl IntoElement {
    let full: gpui_kit::SharedString = text.clone().into();
    div()
        .id(id)
        .flex_1()
        .min_w(px(0.))
        .px_2()
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx)
        })
        .child(text)
}

/// Кнопки под таблицей. Edit / Duplicate / Remove — только с выделением.
pub fn rule_actions(
    id: &'static str,
    has_selection: bool,
    on_add: impl Fn(&mut Window, &mut App) + 'static,
    on_edit: impl Fn(&mut Window, &mut App) + 'static,
    on_duplicate: impl Fn(&mut App) + 'static,
    on_remove: impl Fn(&mut App) + 'static,
) -> AnyElement {
    div()
        .flex()
        .gap(px(8.))
        .child(
            Button::new((id, 0usize))
                .label("Add…")
                .primary()
                .on_click(move |_, window, cx| on_add(window, cx)),
        )
        .child(
            Button::new((id, 1usize))
                .label("Edit…")
                .disabled(!has_selection)
                .on_click(move |_, window, cx| on_edit(window, cx)),
        )
        .child(
            Button::new((id, 2usize))
                .label("Duplicate")
                .disabled(!has_selection)
                .on_click(move |_, _, cx| on_duplicate(cx)),
        )
        .child(
            Button::new((id, 3usize))
                .label("Remove")
                .danger()
                .disabled(!has_selection)
                .on_click(move |_, _, cx| on_remove(cx)),
        )
        .into_any_element()
}
