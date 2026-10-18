//! Диалог «Add / Edit Rewrite» (Tools → Rewrite): имя, к чему применять
//! (Request / Response), Host / Path / Method (пусто — всё) и список
//! операций — вид, поля, regex. Модель и проверка —
//! `application::rules::rewrite_form`; ошибки — у своих полей и операций.

use application::use_cases::rules::edit::upsert_rewrite;
use application::use_cases::rules::location_form::{LocationForm, METHODS};
use application::use_cases::rules::rewrite_form::{OpForm, OpKind, RewriteErrors, RewriteForm};
use domain::entities::rule::{RewriteLocation, RewriteRule};
use domain::value_objects::RuleId;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::radio::Radio;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Hsla, IntoElement, ParentElement, Render,
    Styled, WeakEntity, Window, div, px,
};

use super::store::RulesStore;

const LABEL_WIDTH: f32 = 96.;
const DIALOG_WIDTH: f32 = 720.;
const FIRST_FIELD_WIDTH: f32 = 200.;

/// Строка операции: вид и его поля.
struct OpRow {
    kind: OpKind,
    first: Entity<InputState>,
    second: Entity<InputState>,
    regex: bool,
}

pub struct RewriteDialog {
    editing: Option<RuleId>,
    store: Entity<RulesStore>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    path: Entity<InputState>,
    method: Option<String>,
    location: RewriteLocation,
    ops: Vec<OpRow>,
    errors: RewriteErrors,
    save_error: Option<String>,
}

impl RewriteDialog {
    fn new(
        store: Entity<RulesStore>,
        editing: Option<RuleId>,
        form: RewriteForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = text_input("e.g. Disable cache", &form.name, window, cx);
        let host = text_input(
            "api.example.com  (empty — all hosts)",
            &form.matching.host,
            window,
            cx,
        );
        let path = text_input(
            "/api/*   (empty — any path)",
            &form.matching.path,
            window,
            cx,
        );
        let ops = form.ops.iter().map(|op| op_row(op, window, cx)).collect();
        Self {
            editing,
            store,
            name,
            host,
            path,
            method: form.matching.method,
            location: form.location,
            ops,
            errors: RewriteErrors::default(),
            save_error: None,
        }
    }

    fn form(&self, cx: &App) -> RewriteForm {
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        RewriteForm {
            name: value(&self.name),
            location: self.location,
            matching: LocationForm {
                host: value(&self.host),
                path: value(&self.path),
                method: self.method.clone(),
            },
            ops: self
                .ops
                .iter()
                .map(|row| OpForm {
                    kind: row.kind,
                    first: value(&row.first),
                    second: value(&row.second),
                    regex: row.regex,
                })
                .collect(),
        }
    }

    fn add_op(&mut self, kind: OpKind, window: &mut Window, cx: &mut Context<Self>) {
        self.ops.push(op_row(&OpForm::new(kind), window, cx));
        cx.notify();
    }

    fn set_kind(
        &mut self,
        index: usize,
        kind: OpKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.ops.get_mut(index) else {
            return;
        };
        row.kind = kind;
        let (first, second) = kind.placeholders();
        row.first
            .update(cx, |state, cx| state.set_placeholder(first, window, cx));
        row.second.update(cx, |state, cx| {
            state.set_placeholder(second.unwrap_or_default(), window, cx)
        });
        cx.notify();
    }

    /// Проверить и сохранить. Диалог закрывает колбэк store после успеха.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parts = match self.form(cx).validate() {
            Ok(parts) => parts,
            Err(errors) => {
                self.errors = errors;
                cx.notify();
                return;
            }
        };
        self.errors = RewriteErrors::default();
        self.save_error = None;
        let id = self.editing;
        let window_handle = window.window_handle();
        let dialog = cx.entity().downgrade();
        RulesStore::apply_then(
            &self.store,
            cx,
            move |rules| {
                Ok(upsert_rewrite(
                    rules,
                    id,
                    parts.name,
                    parts.location,
                    parts.r#match,
                    parts.ops,
                ))
            },
            move |result, cx| match result {
                Ok(()) => {
                    let _ = window_handle.update(cx, |_, window, cx| window.close_dialog(cx));
                }
                Err(error) => {
                    let _ = dialog.update(cx, |d, cx| {
                        d.save_error = Some(error);
                        cx.notify();
                    });
                }
            },
        );
    }

    fn op_rows(&self, weak: &WeakEntity<Self>, danger: Hsla) -> Vec<AnyElement> {
        self.ops
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let kind_menu = {
                    let weak = weak.clone();
                    Button::new(("rewrite-op-kind", index))
                        .label(format!("{} ▾", row.kind.label()))
                        .dropdown_menu(move |menu, _, _| {
                            OpKind::ALL.iter().fold(menu, |menu, &kind| {
                                let weak = weak.clone();
                                menu.item(PopupMenuItem::new(kind.label()).on_click(
                                    move |_, window, cx| {
                                        let _ = weak.update(cx, |d, cx| {
                                            d.set_kind(index, kind, window, cx)
                                        });
                                    },
                                ))
                            })
                        })
                };
                let remove = {
                    let weak = weak.clone();
                    Button::new(("rewrite-op-remove", index))
                        .label("✕")
                        .on_click(move |_, _, cx| {
                            let _ = weak.update(cx, |d, cx| {
                                if index < d.ops.len() {
                                    d.ops.remove(index);
                                    d.errors = RewriteErrors::default();
                                }
                                cx.notify();
                            });
                        })
                };
                let regex = row.kind.has_regex().then(|| {
                    let weak = weak.clone();
                    Checkbox::new(("rewrite-op-regex", index))
                        .label("Regex")
                        .checked(row.regex)
                        .on_click(move |checked, _, cx| {
                            let checked = *checked;
                            let _ = weak.update(cx, |d, cx| {
                                if let Some(row) = d.ops.get_mut(index) {
                                    row.regex = checked;
                                }
                                cx.notify();
                            });
                        })
                });
                let second = row.kind.placeholders().1.is_some();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(kind_menu)
                            .child(div().w(px(FIRST_FIELD_WIDTH)).child(Input::new(&row.first)))
                            .when(second, |el| {
                                el.child(div().flex_1().child(Input::new(&row.second)))
                            })
                            .when(!second, |el| el.child(div().flex_1()))
                            .children(regex)
                            .child(remove),
                    )
                    .when_some(self.errors.op(index).map(str::to_string), |el, err| {
                        el.child(div().text_size(px(12.)).text_color(danger).child(err))
                    })
                    .into_any_element()
            })
            .collect()
    }
}

fn text_input(
    placeholder: &str,
    value: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputState> {
    let (placeholder, value) = (placeholder.to_string(), value.to_string());
    cx.new(|cx| {
        let mut state = InputState::new(window, cx).placeholder(placeholder);
        state.set_value(value, window, cx);
        state
    })
}

fn op_row(op: &OpForm, window: &mut Window, cx: &mut App) -> OpRow {
    let (first, second) = op.kind.placeholders();
    OpRow {
        kind: op.kind,
        first: text_input(first, &op.first, window, cx),
        second: text_input(second.unwrap_or_default(), &op.second, window, cx),
        regex: op.regex,
    }
}

fn row(label: &str, field: AnyElement, error: Option<String>, danger: Hsla) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .child(
            div()
                .w(px(LABEL_WIDTH))
                .flex_none()
                .pt(px(6.))
                .text_size(px(13.))
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(field)
                .when_some(error, |el, err| {
                    el.child(div().text_size(px(12.)).text_color(danger).child(err))
                }),
        )
        .into_any_element()
}

impl Render for RewriteDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (danger, muted) = (cx.theme().danger, cx.theme().muted_foreground);
        let weak = cx.entity().downgrade();
        let method = Button::new("rewrite-method")
            .label(format!(
                "{} ▾",
                self.method.as_deref().unwrap_or("Any method")
            ))
            .dropdown_menu({
                let weak = weak.clone();
                move |menu, _, _| {
                    let choices =
                        std::iter::once(None).chain(METHODS.iter().map(|m| Some(m.to_string())));
                    choices.fold(menu, |menu, choice| {
                        let weak = weak.clone();
                        let label = choice.clone().unwrap_or_else(|| "Any method".into());
                        menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                            let _ = weak.update(cx, |d, cx| {
                                d.method = choice.clone();
                                cx.notify();
                            });
                        }))
                    })
                }
            });
        let location_radio = |id: &'static str, label: &'static str, value: RewriteLocation| {
            let weak = weak.clone();
            Radio::new(id)
                .label(label)
                .checked(self.location == value)
                .on_click(move |_, _, cx| {
                    let _ = weak.update(cx, |d, cx| {
                        d.location = value;
                        cx.notify();
                    });
                })
        };
        let add_op = Button::new("rewrite-add-op")
            .label("+ Add operation ▾")
            .dropdown_menu({
                let weak = weak.clone();
                move |menu, _, _| {
                    OpKind::ALL.iter().fold(menu, |menu, &kind| {
                        let weak = weak.clone();
                        menu.item(PopupMenuItem::new(kind.label()).on_click(
                            move |_, window, cx| {
                                let _ = weak.update(cx, |d, cx| d.add_op(kind, window, cx));
                            },
                        ))
                    })
                }
            });
        let preview = match self.form(cx).matching {
            m if m.host.trim().is_empty() && m.path.trim().is_empty() => {
                "Matches: everything".to_string()
            }
            m => format!("Matches: {}", m.preview()),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(row(
                "Name",
                Input::new(&self.name).into_any_element(),
                None,
                danger,
            ))
            .child(row(
                "Apply to",
                div()
                    .flex()
                    .gap(px(16.))
                    .child(location_radio(
                        "rewrite-request",
                        "Request",
                        RewriteLocation::Request,
                    ))
                    .child(location_radio(
                        "rewrite-response",
                        "Response",
                        RewriteLocation::Response,
                    ))
                    .into_any_element(),
                None,
                danger,
            ))
            .child(row(
                "Host",
                Input::new(&self.host).into_any_element(),
                self.errors.host.clone(),
                danger,
            ))
            .child(row(
                "Path",
                Input::new(&self.path).into_any_element(),
                self.errors.path.clone(),
                danger,
            ))
            .child(row("Method", method.into_any_element(), None, danger))
            .child(
                div()
                    .pl(px(LABEL_WIDTH + 8.))
                    .text_size(px(12.))
                    .text_color(muted)
                    .child(preview),
            )
            .child(
                div()
                    .pt(px(6.))
                    .text_size(px(12.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("Operations (applied in order)"),
            )
            .children(self.op_rows(&weak, danger))
            .child(div().child(add_op))
            .when_some(
                self.errors.form.clone().or(self.save_error.clone()),
                |el, err| {
                    el.child(
                        div()
                            .text_size(px(12.))
                            .text_color(danger)
                            .child(format!("⚠ {err}")),
                    )
                },
            )
    }
}

/// Открыть диалог: `rule` — правка, иначе новое правило с `prefill`.
pub fn open_rewrite_dialog(
    store: Entity<RulesStore>,
    rule: Option<&RewriteRule>,
    prefill: LocationForm,
    window: &mut Window,
    cx: &mut App,
) {
    let editing = rule.map(|r| r.id);
    let form = match rule {
        Some(rule) => RewriteForm::from_rule(rule),
        None => RewriteForm {
            name: String::new(),
            location: RewriteLocation::Response,
            matching: prefill,
            ops: vec![OpForm::new(OpKind::ReplaceHeader)],
        },
    };
    let dialog = cx.new(|cx| RewriteDialog::new(store, editing, form, window, cx));
    window.open_dialog(cx, move |d, _, _| {
        let submit = dialog.clone();
        let footer = DialogFooter::new()
            .child(
                Button::new("rewrite-cancel")
                    .label("Cancel")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(
                Button::new("rewrite-submit")
                    .label(if editing.is_some() { "Save" } else { "Add" })
                    .primary()
                    .on_click(move |_, window, cx| submit.update(cx, |d, cx| d.submit(window, cx))),
            );
        d.title(if editing.is_some() {
            "Edit Rewrite"
        } else {
            "Add Rewrite"
        })
        .w(px(DIALOG_WIDTH))
        .child(dialog.clone())
        .footer(footer)
    });
}
