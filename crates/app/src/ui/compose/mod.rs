//! Окно Compose (Phase 7b): составить запрос с нуля или из захваченного
//! (Edit & Resend) и отправить через движок — правила и скрипты работают,
//! обмен появляется в трафике с тегом `composed`. Ответ — справа.
//! Repeat из контекстного меню отправляет обмен без окна.

use std::sync::Arc;

use application::dto::compose::SendResultDto;
use application::use_cases::compose::form::ComposeForm;
use application::use_cases::compose::{RepeatExchangeUseCase, SendComposedRequestUseCase};
use application::use_cases::rules::location_form::METHODS;
use domain::value_objects::ExchangeId;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Global,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement, Render,
    StatefulInteractiveElement as _, Styled, Window, div, px,
};

use crate::ui::async_bridge::spawn_ui_task;
use crate::ui::theme::{ToneName, glass};
use crate::ui::window_shell;

gpui_kit::actions!(compose, [SendComposed]);

const KEY_CONTEXT: &str = "Compose";
const WINDOW_WIDTH: f32 = 1100.;
const WINDOW_HEIGHT: f32 = 720.;
/// Сколько тела ответа показывать (полное — в трафике).
const SHOWN_BODY_BYTES: usize = 256 * 1024;

/// Сценарии и рантайм окна.
#[derive(Clone)]
pub struct ComposeDeps {
    pub repeat: Arc<RepeatExchangeUseCase>,
    pub send: Arc<SendComposedRequestUseCase>,
    pub runtime: tokio::runtime::Handle,
}

impl Global for ComposeDeps {}

/// Зарегистрировать окно (один раз на приложение).
pub fn install(deps: ComposeDeps, cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "cmd-enter",
        SendComposed,
        Some(KEY_CONTEXT),
    )]);
    cx.set_global(deps);
}

/// Новый запрос.
pub fn open_blank(cx: &mut App) {
    open_window(ComposeForm::blank(), cx);
}

/// Edit & Resend: форма из захваченного обмена.
pub fn open_edit(id: ExchangeId, cx: &mut App) {
    let Some(deps) = cx.try_global::<ComposeDeps>().cloned() else {
        return;
    };
    let (tx, rx) = async_channel::bounded(1);
    deps.runtime.spawn(async move {
        let _ = tx.send(deps.repeat.edit_form(&id).await).await;
    });
    cx.spawn(async move |cx| {
        if let Ok(result) = rx.recv().await {
            cx.update(|cx| match result {
                Ok(form) => open_window(form, cx),
                Err(error) => tracing::warn!(%error, "edit & resend: cannot load the request"),
            });
        }
    })
    .detach();
}

/// Repeat: отправить обмен ещё раз (результат — в трафике).
pub fn repeat(id: ExchangeId, cx: &mut App) {
    let Some(deps) = cx.try_global::<ComposeDeps>().cloned() else {
        return;
    };
    deps.runtime.spawn(async move {
        if let Err(error) = deps.repeat.repeat(&id).await {
            tracing::warn!(%error, "repeat failed");
        }
    });
}

fn open_window(form: ComposeForm, cx: &mut App) {
    let Some(deps) = cx.try_global::<ComposeDeps>().cloned() else {
        tracing::warn!("compose requested before install");
        return;
    };
    cx.activate(true);
    let options =
        window_shell::window_options("crabtrap — Compose", WINDOW_WIDTH, WINDOW_HEIGHT, cx);
    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| ComposeView::new(deps, form, window, cx));
        view.update(cx, |view, cx| view.focus.focus(window, cx));
        window_shell::transparent_root(gpui_kit::AnyView::from(view), window, cx)
    });
    if opened.is_err() {
        tracing::warn!("cannot open compose window");
    }
}

/// Состояние ответа.
enum Outcome {
    Idle,
    Sending,
    Done(SendResultDto),
    Failed(String),
}

pub struct ComposeView {
    deps: ComposeDeps,
    method: String,
    url: Entity<InputState>,
    headers: Entity<EditorState>,
    body: Entity<EditorState>,
    /// Двоичное/сжатое тело из захвата: уходит как есть, не редактируется.
    fixed_body: Option<Vec<u8>>,
    outcome: Outcome,
    focus: FocusHandle,
}

impl ComposeView {
    fn new(
        deps: ComposeDeps,
        form: ComposeForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url = cx.new(|cx| {
            let mut state =
                InputState::new(window, cx).placeholder("https://api.example.com/v1/items");
            state.set_value(form.url.clone(), window, cx);
            state
        });
        let editor = |text: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut state = EditorState::new(window, cx).line_number(true);
                state.set_value(text, window, cx);
                state
            })
        };
        let headers = editor(form.headers.clone(), window, cx);
        let body = editor(form.body.clone(), window, cx);
        Self {
            deps,
            method: form.method,
            url,
            headers,
            body,
            fixed_body: form.fixed_body,
            outcome: Outcome::Idle,
            focus: cx.focus_handle(),
        }
    }

    fn form(&self, cx: &App) -> ComposeForm {
        ComposeForm {
            method: self.method.clone(),
            url: self.url.read(cx).value().to_string(),
            headers: self.headers.read(cx).value().to_string(),
            body: self.body.read(cx).value().to_string(),
            fixed_body: self.fixed_body.clone(),
        }
    }

    fn send(&mut self, _: &SendComposed, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.outcome, Outcome::Sending) {
            return;
        }
        let form = self.form(cx);
        if let Err(error) = form.to_request(Default::default()) {
            self.outcome = Outcome::Failed(error);
            cx.notify();
            return;
        }
        self.outcome = Outcome::Sending;
        cx.notify();
        let send = self.deps.send.clone();
        let entity = cx.entity();
        spawn_ui_task(
            cx,
            &entity,
            self.deps.runtime.clone(),
            async move { send.execute(&form).await },
            |view: &mut Self, result, cx| {
                view.outcome = match result {
                    Ok(response) => Outcome::Done(response),
                    Err(error) => Outcome::Failed(error.to_string()),
                };
                cx.notify();
            },
        );
    }

    fn request_line(&self, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        let method = Button::new("compose-method")
            .label(format!("{} ▾", self.method))
            .dropdown_menu(move |menu, _, _| {
                METHODS.iter().fold(menu, |menu, method| {
                    let weak = weak.clone();
                    menu.item(PopupMenuItem::new(*method).on_click(move |_, _, cx| {
                        let _ = weak.update(cx, |view, cx| {
                            view.method = method.to_string();
                            cx.notify();
                        });
                    }))
                })
            });
        let sending = matches!(self.outcome, Outcome::Sending);
        let send = Button::new("compose-send")
            .label(if sending {
                "Sending…"
            } else {
                "Send  ⌘↩"
            })
            .primary()
            .disabled(sending)
            .on_click(cx.listener(|view, _, window, cx| view.send(&SendComposed, window, cx)));
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(method)
            .child(div().flex_1().child(Input::new(&self.url)))
            .child(send)
            .into_any_element()
    }

    fn response_panel(&self, cx: &App) -> AnyElement {
        let p = glass(cx);
        let mono = cx.theme().mono_font_family.clone();
        let content: AnyElement = match &self.outcome {
            Outcome::Idle => div()
                .text_color(p.text_secondary)
                .child("Send the request to see the response. It also appears in the traffic list.")
                .into_any_element(),
            Outcome::Sending => div()
                .text_color(p.text_secondary)
                .child("Sending…")
                .into_any_element(),
            Outcome::Failed(error) => div()
                .text_color(p.tone(ToneName::Red).fg)
                .child(format!("⚠ {error}"))
                .into_any_element(),
            Outcome::Done(response) => {
                let tone = match response.status {
                    200..=299 => ToneName::Green,
                    300..=399 => ToneName::Blue,
                    400..=499 => ToneName::Orange,
                    _ => ToneName::Red,
                };
                let headers: Vec<AnyElement> = response
                    .headers
                    .iter()
                    .map(|h| {
                        div()
                            .child(format!("{}: {}", h.name, h.value))
                            .into_any_element()
                    })
                    .collect();
                let body = response.body.as_deref().map_or_else(String::new, |b| {
                    let shown = &b[..b.len().min(SHOWN_BODY_BYTES)];
                    String::from_utf8_lossy(shown).into_owned()
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(p.tone(tone).fg)
                            .child(format!("HTTP {}", response.status)),
                    )
                    .when_some(response.error.clone(), |el, error| {
                        el.child(div().text_color(p.tone(ToneName::Red).fg).child(error))
                    })
                    .child(
                        div()
                            .font_family(mono.clone())
                            .text_size(px(11.))
                            .flex()
                            .flex_col()
                            .children(headers),
                    )
                    .child(div().border_t_1().border_color(p.hairline))
                    .child(div().font_family(mono).text_size(px(11.)).child(body))
                    .into_any_element()
            }
        };
        div()
            .id("compose-response")
            .w(px(440.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .p(px(12.))
            .border_l_1()
            .border_color(p.hairline)
            .text_size(px(12.))
            .child(content)
            .into_any_element()
    }
}

fn section(title: &str, editor: &Entity<EditorState>, height: Option<f32>, cx: &App) -> AnyElement {
    let p = glass(cx);
    let frame = div().flex().flex_col().gap(px(4.)).child(
        div()
            .text_size(px(12.))
            .text_color(p.text_secondary)
            .child(title.to_string()),
    );
    let field = div()
        .border_1()
        .border_color(p.hairline)
        .rounded(px(6.))
        .child(Editor::new(editor).size_full().bordered(false));
    match height {
        Some(height) => frame.flex_none().child(field.h(px(height))),
        None => frame
            .flex_1()
            .min_h(px(0.))
            .child(field.flex_1().min_h(px(0.))),
    }
    .into_any_element()
}

impl Render for ComposeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = glass(cx);
        let request_line = self.request_line(cx);
        let response = self.response_panel(cx);
        div()
            .id("compose")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::send))
            .size_full()
            .flex()
            .flex_col()
            .bg(p.window)
            .text_color(p.text)
            .child(window_shell::title_bar("Compose", cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .p(px(12.))
                            .child(request_line)
                            .child(section("Headers — one per line, Name: value", &self.headers, Some(180.), cx))
                            .child(match &self.fixed_body {
                                None => section("Body", &self.body, None, cx),
                                Some(bytes) => div()
                                    .text_size(px(12.))
                                    .text_color(p.text_secondary)
                                    .child(format!(
                                        "Body: {} bytes, binary or compressed — sent exactly as captured",
                                        bytes.len()
                                    ))
                                    .into_any_element(),
                            }),
                    )
                    .child(response),
            )
    }
}
