//! Диалог «Add / Edit Location» (Tools): поля Host / Path / Method вместо
//! одной строки с неочевидным синтаксисом, плюс поля инструмента (файл Map
//! Local с выбором через Finder, цель Map Remote по полям, действие Block).
//! Ошибки — под своими полями; Save не закрывает диалог, пока форма
//! невалидна. Модель и валидация — `application::rules::location_form`.

use std::path::PathBuf;

use application::use_cases::rules::edit::{
    upsert_block, upsert_breakpoint, upsert_map_local, upsert_map_remote, upsert_ssl_host,
    with_throttle_scope,
};
use application::use_cases::rules::location_form::{
    LocationErrors, LocationForm, METHODS, RemoteTargetForm,
};
use application::use_cases::rules::response_form::{ResponseErrors, ResponseForm};
use domain::entities::rule::{BlockAction, BreakPhase, RuleSet};
use domain::value_objects::RuleId;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogButtonProps, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::radio::Radio;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement,
    PathPromptOptions, Render, Styled, Subscription, Window, div, px,
};

use super::store::RulesStore;

/// Ширина подписи слева от поля.
const LABEL_WIDTH: f32 = 96.;
const DIALOG_WIDTH: f32 = 560.;

/// Какое правило редактирует диалог.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    SslHost,
    Block,
    MapLocal,
    MapRemote,
    Breakpoint,
    ThrottleScope,
}

impl DialogKind {
    fn title(self, editing: bool) -> &'static str {
        match (self, editing) {
            (DialogKind::SslHost, false) => "Add SSL Proxying host",
            (DialogKind::SslHost, true) => "Edit SSL Proxying host",
            (DialogKind::Block, false) => "Block location",
            (DialogKind::Block, true) => "Edit blocked location",
            (DialogKind::MapLocal, false) => "Map location to local file",
            (DialogKind::MapLocal, true) => "Edit Map Local",
            (DialogKind::MapRemote, false) => "Map location to another server",
            (DialogKind::MapRemote, true) => "Edit Map Remote",
            (DialogKind::Breakpoint, false) => "Add breakpoint",
            (DialogKind::Breakpoint, true) => "Edit breakpoint",
            (DialogKind::ThrottleScope, _) => "Throttle only this location",
        }
    }

    fn has_path(self) -> bool {
        self != DialogKind::SslHost
    }
}

impl Default for DialogInit {
    fn default() -> Self {
        Self {
            editing: None,
            location: LocationForm::default(),
            block_close: false,
            ssl_exclude: false,
            local_path: String::new(),
            response: ResponseForm::default(),
            remote: None,
            keep_host: false,
            break_phase: BreakPhase::Request,
        }
    }
}

/// Начальные значения диалога (новое правило, правка или из трафика).
#[derive(Debug, Clone)]
pub struct DialogInit {
    pub editing: Option<RuleId>,
    pub location: LocationForm,
    pub block_close: bool,
    /// SSL Proxying: «не расшифровывать» (исключение).
    pub ssl_exclude: bool,
    pub local_path: String,
    pub response: ResponseForm,
    pub remote: Option<RemoteTargetForm>,
    pub keep_host: bool,
    /// Фаза брейкпоинта.
    pub break_phase: BreakPhase,
}

#[derive(Default)]
struct Errors {
    location: LocationErrors,
    target: Option<String>,
    response: ResponseErrors,
    /// Сохранение не удалось (валидация use case, IO) — диалог открыт.
    save: Option<String>,
}

pub struct LocationDialog {
    kind: DialogKind,
    editing: Option<RuleId>,
    store: Entity<RulesStore>,
    host: Entity<InputState>,
    path: Entity<InputState>,
    method: Option<String>,
    block_close: bool,
    ssl_exclude: bool,
    local_path: Entity<InputState>,
    status: Entity<InputState>,
    content_type: Entity<InputState>,
    remote_https: bool,
    remote_host: Entity<InputState>,
    remote_port: Entity<InputState>,
    remote_path: Entity<InputState>,
    /// Query цели Map Remote: полей нет, но при правке не теряется.
    remote_query: String,
    keep_host: bool,
    break_phase: BreakPhase,
    errors: Errors,
    _subscriptions: Vec<Subscription>,
}

impl LocationDialog {
    fn new(
        kind: DialogKind,
        store: Entity<RulesStore>,
        init: DialogInit,
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
        let remote = init.remote.clone().unwrap_or(RemoteTargetForm {
            https: false,
            host: String::new(),
            port: String::new(),
            path: String::new(),
            query: String::new(),
        });
        let host = input(
            "api.example.com  or  *.example.com  (paste a URL to split it)",
            &init.location.host,
        );
        let path = input("/api/*   (empty — any path)", &init.location.path);
        let local_path = input("/Users/me/mocks/users.json", &init.local_path);
        let status = input("200", &init.response.status);
        let content_type = input("auto — from file extension", &init.response.content_type);
        let remote_host = input("localhost", &remote.host);
        let remote_port = input("3000", &remote.port);
        let remote_path = input("/  (empty — keep original path)", &remote.path);
        // Предпросмотр «Matches: …» обновляется при вводе.
        let subscriptions = [&host, &path]
            .into_iter()
            .map(|input| cx.subscribe(input, |_, _, _: &InputEvent, cx| cx.notify()))
            .collect();
        Self {
            kind,
            editing: init.editing,
            store,
            host,
            path,
            method: init.location.method,
            block_close: init.block_close,
            ssl_exclude: init.ssl_exclude,
            local_path,
            status,
            content_type,
            remote_https: remote.https,
            remote_host,
            remote_port,
            remote_path,
            remote_query: remote.query.clone(),
            keep_host: init.keep_host,
            break_phase: init.break_phase,
            errors: Errors::default(),
            _subscriptions: subscriptions,
        }
    }

    fn form(&self, cx: &App) -> LocationForm {
        LocationForm {
            host: self.host.read(cx).value().to_string(),
            path: self.path.read(cx).value().to_string(),
            method: self.method.clone(),
        }
    }

    /// Проверить форму и сохранить правило. Диалог закрывается только после
    /// успешного сохранения (колбэк store); при ошибке — остаётся открытым и
    /// показывает её. Возврат: `false` (закрытием управляет колбэк).
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = |input: &Entity<InputState>, cx: &App| input.read(cx).value().trim().to_string();
        let form = self.form(cx);
        let location = match form.to_match() {
            Ok(m) => m,
            Err(errors) => {
                self.errors = Errors {
                    location: errors,
                    ..Errors::default()
                };
                cx.notify();
                return false;
            }
        };
        let id = self.editing;
        let edit: Box<dyn FnOnce(RuleSet) -> Result<RuleSet, domain::DomainError> + Send> =
            match self.kind {
                DialogKind::SslHost => {
                    let host = form.normalized().host;
                    let exclude = self.ssl_exclude;
                    Box::new(move |rules| upsert_ssl_host(rules, id, &host, exclude))
                }
                DialogKind::Block => {
                    let action = if self.block_close {
                        BlockAction::Close
                    } else {
                        BlockAction::Status(403)
                    };
                    Box::new(move |rules| Ok(upsert_block(rules, id, location, action)))
                }
                DialogKind::MapLocal => {
                    let path = text(&self.local_path, cx);
                    let path_error = path
                        .is_empty()
                        .then(|| "Choose a file or folder to serve".to_string());
                    let response = ResponseForm {
                        status: text(&self.status, cx),
                        content_type: text(&self.content_type, cx),
                    }
                    .to_response();
                    let response = match (path_error, response) {
                        (None, Ok(response)) => response,
                        (target, response) => {
                            self.errors = Errors {
                                target,
                                response: response.err().unwrap_or_default(),
                                ..Errors::default()
                            };
                            cx.notify();
                            return false;
                        }
                    };
                    let path = expand_home(&path);
                    Box::new(move |rules| Ok(upsert_map_local(rules, id, location, path, response)))
                }
                DialogKind::MapRemote => {
                    let target = RemoteTargetForm {
                        https: self.remote_https,
                        host: text(&self.remote_host, cx),
                        port: text(&self.remote_port, cx),
                        path: text(&self.remote_path, cx),
                        query: self.remote_query.clone(),
                    };
                    let url = match target.to_url() {
                        Ok(url) => url,
                        Err(e) => {
                            self.errors = Errors {
                                target: Some(e),
                                ..Errors::default()
                            };
                            cx.notify();
                            return false;
                        }
                    };
                    let keep = self.keep_host;
                    Box::new(move |rules| Ok(upsert_map_remote(rules, id, location, url, keep)))
                }
                DialogKind::Breakpoint => {
                    let phase = self.break_phase;
                    Box::new(move |rules| Ok(upsert_breakpoint(rules, id, location, phase)))
                }
                DialogKind::ThrottleScope => {
                    Box::new(move |rules| with_throttle_scope(rules, Some(location)))
                }
            };
        self.errors = Errors::default();
        let window_handle = window.window_handle();
        let dialog = cx.entity().downgrade();
        RulesStore::apply_then(&self.store, cx, edit, move |result, cx| match result {
            Ok(()) => {
                let _ = window_handle.update(cx, |_, window, cx| window.close_dialog(cx));
            }
            Err(e) => {
                let _ = dialog.update(cx, |d, cx| {
                    d.errors.save = Some(e);
                    cx.notify();
                });
            }
        });
        false
    }

    fn choose_local_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("Serve".into()),
        });
        let input = self.local_path.clone();
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = input.update_in(cx, |state, window, cx| {
                    state.set_value(path.display().to_string(), window, cx);
                });
            }
        })
        .detach();
    }
}

impl Render for LocationDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (danger, muted) = (theme.danger, theme.muted_foreground);
        let weak = cx.entity().downgrade();
        let method_label = format!("{} ▾", self.method.as_deref().unwrap_or("Any method"));
        let method = Button::new("dialog-method")
            .label(method_label)
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

        let mut body = div().flex().flex_col().gap(px(10.));
        body = body.child(row(
            "Host",
            Input::new(&self.host).into_any_element(),
            self.errors.location.host.clone(),
            danger,
        ));
        if self.kind.has_path() {
            body = body
                .child(row(
                    "Path",
                    Input::new(&self.path).into_any_element(),
                    self.errors.location.path.clone(),
                    danger,
                ))
                .child(row("Method", method.into_any_element(), None, danger))
                .child(
                    div()
                        .pl(px(LABEL_WIDTH + 8.))
                        .text_size(px(12.))
                        .text_color(muted)
                        .child(format!("Matches: {}", self.form(cx).preview())),
                );
        }
        body = match self.kind {
            DialogKind::Block => body.child(row(
                "Action",
                div()
                    .flex()
                    .gap(px(16.))
                    .child(
                        Radio::new("block-403")
                            .label("Respond 403")
                            .checked(!self.block_close)
                            .on_click({
                                let weak = weak.clone();
                                move |_, _, cx| set_flag(&weak, cx, |d| d.block_close = false)
                            }),
                    )
                    .child(
                        Radio::new("block-drop")
                            .label("Drop connection")
                            .checked(self.block_close)
                            .on_click({
                                let weak = weak.clone();
                                move |_, _, cx| set_flag(&weak, cx, |d| d.block_close = true)
                            }),
                    )
                    .into_any_element(),
                None,
                danger,
            )),
            DialogKind::MapLocal => body
                .child(row(
                    "Local path",
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(div().flex_1().child(Input::new(&self.local_path)))
                        .child(Button::new("dialog-choose").label("Choose…").on_click({
                            let weak = weak.clone();
                            move |_, window, cx| {
                                let _ = weak.update(cx, |d, cx| d.choose_local_path(window, cx));
                            }
                        }))
                        .into_any_element(),
                    self.errors.target.clone(),
                    danger,
                ))
                .child(row(
                    "Status",
                    div()
                        .w(px(120.))
                        .child(Input::new(&self.status))
                        .into_any_element(),
                    self.errors.response.status.clone(),
                    danger,
                ))
                .child(row(
                    "Content-Type",
                    Input::new(&self.content_type).into_any_element(),
                    self.errors.response.content_type.clone(),
                    danger,
                )),
            DialogKind::MapRemote => body
                .child(
                    div()
                        .pt(px(6.))
                        .text_size(px(12.))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child("Map to"),
                )
                .child(row(
                    "Protocol",
                    div()
                        .flex()
                        .gap(px(16.))
                        .child(
                            Radio::new("remote-http")
                                .label("http")
                                .checked(!self.remote_https)
                                .on_click({
                                    let weak = weak.clone();
                                    move |_, _, cx| set_flag(&weak, cx, |d| d.remote_https = false)
                                }),
                        )
                        .child(
                            Radio::new("remote-https")
                                .label("https")
                                .checked(self.remote_https)
                                .on_click({
                                    let weak = weak.clone();
                                    move |_, _, cx| set_flag(&weak, cx, |d| d.remote_https = true)
                                }),
                        )
                        .into_any_element(),
                    None,
                    danger,
                ))
                .child(row(
                    "Host",
                    Input::new(&self.remote_host).into_any_element(),
                    self.errors.target.clone(),
                    danger,
                ))
                .child(row(
                    "Port",
                    div()
                        .w(px(120.))
                        .child(Input::new(&self.remote_port))
                        .into_any_element(),
                    None,
                    danger,
                ))
                .child(row(
                    "Path",
                    Input::new(&self.remote_path).into_any_element(),
                    None,
                    danger,
                ))
                .child(row(
                    "",
                    Checkbox::new("remote-keep-host")
                        .label("Keep original Host header")
                        .checked(self.keep_host)
                        .on_click({
                            let weak = weak.clone();
                            move |checked, _, cx| {
                                let checked = *checked;
                                set_flag(&weak, cx, move |d| d.keep_host = checked)
                            }
                        })
                        .into_any_element(),
                    None,
                    danger,
                )),
            DialogKind::Breakpoint => {
                let radio = |id: &'static str, label: &'static str, phase: BreakPhase| {
                    let weak = weak.clone();
                    Radio::new(id)
                        .label(label)
                        .checked(self.break_phase == phase)
                        .on_click(move |_, _, cx| {
                            set_flag(&weak, cx, move |d| d.break_phase = phase)
                        })
                };
                body.child(row(
                    "Stop on",
                    div()
                        .flex()
                        .gap(px(16.))
                        .child(radio("bp-request", "Request", BreakPhase::Request))
                        .child(radio("bp-response", "Response", BreakPhase::Response))
                        .child(radio("bp-both", "Both", BreakPhase::Both))
                        .into_any_element(),
                    None,
                    danger,
                ))
            }
            DialogKind::SslHost => body.child(row(
                "Action",
                div()
                    .flex()
                    .gap(px(16.))
                    .child(
                        Radio::new("ssl-decrypt")
                            .label("Decrypt")
                            .checked(!self.ssl_exclude)
                            .on_click({
                                let weak = weak.clone();
                                move |_, _, cx| set_flag(&weak, cx, |d| d.ssl_exclude = false)
                            }),
                    )
                    .child(
                        Radio::new("ssl-exclude")
                            .label("Don't decrypt (tunnel)")
                            .checked(self.ssl_exclude)
                            .on_click({
                                let weak = weak.clone();
                                move |_, _, cx| set_flag(&weak, cx, |d| d.ssl_exclude = true)
                            }),
                    )
                    .into_any_element(),
                None,
                danger,
            )),
            DialogKind::ThrottleScope => body,
        };
        body.when_some(self.errors.save.clone(), |el, err| {
            el.child(
                div()
                    .text_size(px(12.))
                    .text_color(danger)
                    .child(format!("⚠ {err}")),
            )
        })
    }
}

fn set_flag(
    weak: &gpui_kit::WeakEntity<LocationDialog>,
    cx: &mut App,
    f: impl FnOnce(&mut LocationDialog),
) {
    let _ = weak.update(cx, |d, cx| {
        f(d);
        cx.notify();
    });
}

/// Строка формы: подпись слева, поле, ошибка под полем.
fn row(
    label: &str,
    field: AnyElement,
    error: Option<String>,
    danger: gpui_kit::Hsla,
) -> AnyElement {
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

/// `~/x` → `$HOME/x`.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// Открыть диалог в текущем окне (Tools).
pub fn open_location_dialog(
    kind: DialogKind,
    store: Entity<RulesStore>,
    init: DialogInit,
    window: &mut Window,
    cx: &mut App,
) {
    let editing = init.editing.is_some();
    let dialog = cx.new(|cx| LocationDialog::new(kind, store, init, window, cx));
    window.open_dialog(cx, move |d, _, _| {
        let (dialog_for_ok, dialog_for_button) = (dialog.clone(), dialog.clone());
        // Кнопки — явным футером: без него gpui-component Dialog рисует
        // только содержимое (button_props управляют лишь Enter/Esc).
        let footer = DialogFooter::new()
            .child(
                Button::new("rule-dialog-cancel")
                    .label("Cancel")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(
                Button::new("rule-dialog-submit")
                    .label(if editing { "Save" } else { "Add" })
                    .primary()
                    .on_click(move |_, window, cx| {
                        if dialog_for_button.update(cx, |d, cx| d.submit(window, cx)) {
                            window.close_dialog(cx);
                        }
                    }),
            );
        d.title(kind.title(editing))
            .w(px(DIALOG_WIDTH))
            .child(dialog.clone())
            .footer(footer)
            .button_props(
                DialogButtonProps::default()
                    .ok_text(if editing { "Save" } else { "Add" })
                    .show_cancel(true),
            )
            .on_ok(move |_, window, cx| dialog_for_ok.update(cx, |d, cx| d.submit(window, cx)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_distinguish_add_and_edit() {
        assert_eq!(
            DialogKind::MapLocal.title(false),
            "Map location to local file"
        );
        assert_eq!(DialogKind::MapLocal.title(true), "Edit Map Local");
        assert!(!DialogKind::SslHost.has_path());
        assert!(DialogKind::Block.has_path());
    }

    #[test]
    fn expands_home() {
        assert_eq!(expand_home("/abs"), PathBuf::from("/abs"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expand_home("~/m.json"), PathBuf::from(home).join("m.json"));
        }
    }
}
