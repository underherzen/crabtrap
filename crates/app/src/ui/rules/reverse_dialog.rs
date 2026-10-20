//! Диалог Reverse Proxy (Tools → Reverse Proxy): локальный порт и цель
//! (протокол, хост, порт), «Keep original Host». Модель и проверка —
//! `application::rules::reverse_form`; Save не закрывает диалог, пока
//! форма или сохранение не прошли.

use application::use_cases::rules::edit::upsert_reverse_proxy;
use application::use_cases::rules::reverse_form::{ReverseProxyErrors, ReverseProxyForm};
use domain::value_objects::RuleId;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::radio::Radio;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, Render, Styled, WeakEntity,
    Window, div, px,
};

use super::match_fields::{LABEL_WIDTH, row};
use super::store::RulesStore;

const DIALOG_WIDTH: f32 = 520.;

pub struct ReverseProxyDialog {
    store: Entity<RulesStore>,
    editing: Option<RuleId>,
    listen_port: Entity<InputState>,
    https: bool,
    host: Entity<InputState>,
    port: Entity<InputState>,
    preserve_host: bool,
    errors: ReverseProxyErrors,
    /// Сохранение не удалось (валидация use case: занятый порт и т.п.).
    save_error: Option<String>,
}

impl ReverseProxyDialog {
    fn new(
        store: Entity<RulesStore>,
        editing: Option<RuleId>,
        form: ReverseProxyForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = |placeholder: &str, value: &str| {
            let (placeholder, value) = (placeholder.to_string(), value.to_string());
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                state.set_value(value, window, cx);
                state
            })
        };
        Self {
            listen_port: input("9090", &form.listen_port),
            host: input("api.example.com", &form.host),
            port: input("default", &form.port),
            https: form.https,
            preserve_host: form.preserve_host,
            store,
            editing,
            errors: ReverseProxyErrors::default(),
            save_error: None,
        }
    }

    fn form(&self, cx: &App) -> ReverseProxyForm {
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        ReverseProxyForm {
            listen_port: text(&self.listen_port),
            https: self.https,
            host: text(&self.host),
            port: text(&self.port),
            preserve_host: self.preserve_host,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values = match self.form(cx).to_values() {
            Ok(values) => values,
            Err(errors) => {
                self.errors = errors;
                self.save_error = None;
                cx.notify();
                return;
            }
        };
        self.errors = ReverseProxyErrors::default();
        let id = self.editing;
        let window_handle = window.window_handle();
        let dialog = cx.entity().downgrade();
        RulesStore::apply_then(
            &self.store,
            cx,
            move |rules| {
                Ok(upsert_reverse_proxy(
                    rules,
                    id,
                    values.listen_port,
                    values.target,
                    values.preserve_host,
                ))
            },
            move |result, cx| match result {
                Ok(()) => {
                    let _ = window_handle.update(cx, |_, window, cx| window.close_dialog(cx));
                }
                Err(e) => {
                    let _ = dialog.update(cx, |d, cx| {
                        d.save_error = Some(e);
                        cx.notify();
                    });
                }
            },
        );
    }
}

fn set_flag(
    weak: &WeakEntity<ReverseProxyDialog>,
    cx: &mut App,
    f: impl FnOnce(&mut ReverseProxyDialog),
) {
    let _ = weak.update(cx, |d, cx| {
        f(d);
        cx.notify();
    });
}

impl Render for ReverseProxyDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (danger, muted) = (theme.danger, theme.muted_foreground);
        let weak = cx.entity().downgrade();
        let protocol = |id: &'static str, label: &'static str, https: bool| {
            let weak = weak.clone();
            Radio::new(id)
                .label(label)
                .checked(self.https == https)
                .on_click(move |_, _, cx| set_flag(&weak, cx, move |d| d.https = https))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(row(
                "Local port",
                div()
                    .w(px(120.))
                    .child(Input::new(&self.listen_port))
                    .into_any_element(),
                self.errors.listen_port.clone(),
                danger,
            ))
            .child(
                div()
                    .pt(px(6.))
                    .text_size(px(12.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("Forward to"),
            )
            .child(row(
                "Protocol",
                div()
                    .flex()
                    .gap(px(16.))
                    .child(protocol("reverse-http", "http", false))
                    .child(protocol("reverse-https", "https", true))
                    .into_any_element(),
                None,
                danger,
            ))
            .child(row(
                "Host",
                Input::new(&self.host).into_any_element(),
                self.errors.target.clone(),
                danger,
            ))
            .child(row(
                "Port",
                div()
                    .w(px(120.))
                    .child(Input::new(&self.port))
                    .into_any_element(),
                None,
                danger,
            ))
            .child(row(
                "",
                Checkbox::new("reverse-keep-host")
                    .label("Keep original Host header")
                    .checked(self.preserve_host)
                    .on_click({
                        let weak = weak.clone();
                        move |checked, _, cx| {
                            let checked = *checked;
                            set_flag(&weak, cx, move |d| d.preserve_host = checked)
                        }
                    })
                    .into_any_element(),
                None,
                danger,
            ))
            .child(
                div()
                    .pl(px(LABEL_WIDTH + 8.))
                    .text_size(px(12.))
                    .text_color(muted)
                    .child(
                        "Point the client at http://localhost:<local port> — requests keep their \
                         path and query and are recorded like proxied traffic.",
                    ),
            )
            .when_some(self.save_error.clone(), |el, err| {
                el.child(
                    div()
                        .text_size(px(12.))
                        .text_color(danger)
                        .child(format!("⚠ {err}")),
                )
            })
    }
}

/// Открыть диалог в текущем окне (Tools): новая запись или правка.
pub fn open_reverse_dialog(
    store: Entity<RulesStore>,
    editing: Option<RuleId>,
    form: ReverseProxyForm,
    window: &mut Window,
    cx: &mut App,
) {
    let dialog = cx.new(|cx| ReverseProxyDialog::new(store, editing, form, window, cx));
    window.open_dialog(cx, move |d, _, _| {
        let submit = dialog.clone();
        let footer = DialogFooter::new()
            .child(
                Button::new("reverse-cancel")
                    .label("Cancel")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(
                Button::new("reverse-submit")
                    .label(if editing.is_some() { "Save" } else { "Add" })
                    .primary()
                    .on_click(move |_, window, cx| submit.update(cx, |d, cx| d.submit(window, cx))),
            );
        d.title(if editing.is_some() {
            "Edit Reverse Proxy"
        } else {
            "Add Reverse Proxy"
        })
        .w(px(DIALOG_WIDTH))
        .child(dialog.clone())
        .footer(footer)
    });
}
