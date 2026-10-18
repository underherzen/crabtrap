//! Окна брейкпоинтов (Phase 7c): по окну на припаркованный обмен.
//! `InterceptWaiting` открывает окно с редактируемым сообщением; Execute
//! (⌘↩) продолжает обмен с правками, Abort обрывает. Закрытие окна без
//! решения — продолжить как было; `InterceptResolved` (таймаут, клиент
//! ушёл, решение принято) закрывает окно.

use std::collections::HashMap;
use std::sync::Arc;

use application::dto::capture::ProxyEvent;
use application::dto::rules::{InterceptPreviewDto, InterceptResolutionDto};
use application::services::InterceptionPipeline;
use application::use_cases::rules::breakpoint_form::BreakpointForm;
use domain::value_objects::InterceptId;
use gpui_kit::component::Root;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Global,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement, Render, Styled, Window,
    WindowHandle, div, px,
};

use crate::ui::theme::{ToneName, glass};
use crate::ui::window_shell;

gpui_kit::actions!(breakpoint, [ExecuteBreakpoint]);

const KEY_CONTEXT: &str = "Breakpoint";
const WINDOW_WIDTH: f32 = 860.;
const WINDOW_HEIGHT: f32 = 680.;

struct BreakpointWindows {
    /// Цепочка перехвата: решение уходит прямо в пайплайн (живёт всю
    /// сессию), а не через командный канал движка, который при
    /// переполнении или остановке молча теряет команды.
    pipeline: Arc<dyn InterceptionPipeline>,
    /// Окна ещё не решённых брейкпоинтов.
    open: HashMap<InterceptId, WindowHandle<Root>>,
}

impl Global for BreakpointWindows {}

/// Зарегистрировать до запуска pump'а событий: первое же InterceptWaiting
/// должно найти, куда открыть окно.
pub fn install(pipeline: Arc<dyn InterceptionPipeline>, cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "cmd-enter",
        ExecuteBreakpoint,
        Some(KEY_CONTEXT),
    )]);
    cx.set_global(BreakpointWindows {
        pipeline,
        open: HashMap::new(),
    });
}

/// События pump'а: открыть/закрыть окна.
pub fn on_events(events: &[ProxyEvent], cx: &mut App) {
    for event in events {
        match event {
            ProxyEvent::InterceptWaiting { preview, .. } => {
                let preview = preview.clone();
                cx.defer(move |cx| open(preview, cx));
            }
            ProxyEvent::InterceptResolved { id } => {
                let id = *id;
                cx.defer(move |cx| close(id, cx));
            }
            _ => {}
        }
    }
}

/// Решение принято (или окно закрыто): отправить в движок и забыть окно.
fn resolve(id: InterceptId, resolution: InterceptResolutionDto, cx: &mut App) {
    let Some(windows) = cx.try_global::<BreakpointWindows>() else {
        return;
    };
    windows.pipeline.resolve_intercept(id, resolution);
    cx.global_mut::<BreakpointWindows>().open.remove(&id);
}

/// Закрыть окно без решения (движок уже продолжил обмен сам).
fn close(id: InterceptId, cx: &mut App) {
    let handle = cx
        .try_global::<BreakpointWindows>()
        .and_then(|w| w.open.get(&id).copied());
    if let Some(handle) = handle {
        cx.global_mut::<BreakpointWindows>().open.remove(&id);
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

fn open(preview: InterceptPreviewDto, cx: &mut App) {
    if !cx.has_global::<BreakpointWindows>() {
        return;
    }
    let id = preview.intercept_id;
    cx.activate(true);
    let options =
        window_shell::window_options("crabtrap — Breakpoint", WINDOW_WIDTH, WINDOW_HEIGHT, cx);
    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| BreakpointView::new(preview, window, cx));
        // Закрыли крестиком, решения не было — продолжить без правок.
        window.on_window_should_close(cx, move |_, cx| {
            let pending = cx
                .try_global::<BreakpointWindows>()
                .is_some_and(|w| w.open.contains_key(&id));
            if pending {
                resolve(id, InterceptResolutionDto::Resume { edited: None }, cx);
            }
            true
        });
        view.update(cx, |view, cx| view.focus.focus(window, cx));
        window_shell::transparent_root(gpui_kit::AnyView::from(view), window, cx)
    });
    let Ok(handle) = opened else {
        tracing::warn!("cannot open breakpoint window, resuming");
        resolve(id, InterceptResolutionDto::Resume { edited: None }, cx);
        return;
    };
    cx.global_mut::<BreakpointWindows>().open.insert(id, handle);
}

pub struct BreakpointView {
    preview: InterceptPreviewDto,
    is_request: bool,
    method: Entity<InputState>,
    url: Entity<InputState>,
    status: Entity<InputState>,
    headers: Entity<EditorState>,
    body: Option<Entity<EditorState>>,
    error: Option<String>,
    focus: FocusHandle,
}

impl BreakpointView {
    fn new(preview: InterceptPreviewDto, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let form = BreakpointForm::from_preview(&preview);
        let mut input = |value: &str| {
            let value = value.to_string();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                state.set_value(value, window, cx);
                state
            })
        };
        let (method, url, status) = (input(&form.method), input(&form.url), input(&form.status));
        let mut editor = |text: &str| {
            let text = text.to_string();
            cx.new(|cx| {
                let mut state = EditorState::new(window, cx).line_number(true);
                state.set_value(text, window, cx);
                state
            })
        };
        let headers = editor(&form.headers);
        let body = form.body.as_deref().map(&mut editor);
        Self {
            preview,
            is_request: form.is_request,
            method,
            url,
            status,
            headers,
            body,
            error: None,
            focus: cx.focus_handle(),
        }
    }

    fn form(&self, cx: &App) -> BreakpointForm {
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        BreakpointForm {
            is_request: self.is_request,
            method: value(&self.method),
            url: value(&self.url),
            status: value(&self.status),
            headers: self.headers.read(cx).value().to_string(),
            body: self.body.as_ref().map(|b| b.read(cx).value().to_string()),
        }
    }

    fn execute(&mut self, _: &ExecuteBreakpoint, window: &mut Window, cx: &mut Context<Self>) {
        match self.form(cx).to_edited(&self.preview.message) {
            Ok(edited) => {
                let resolution = InterceptResolutionDto::Resume {
                    edited: Some(edited),
                };
                resolve(self.preview.intercept_id, resolution, cx);
                window.remove_window();
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    fn abort(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        resolve(self.preview.intercept_id, InterceptResolutionDto::Abort, cx);
        window.remove_window();
    }

    fn title(&self) -> String {
        let what = if self.is_request {
            "Request"
        } else {
            "Response"
        };
        format!("Breakpoint — {what} · {}", self.preview.url)
    }
}

fn labeled(label: &str, field: AnyElement, cx: &App) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(12.))
                .text_color(glass(cx).text_secondary)
                .child(label.to_string()),
        )
        .child(field)
        .into_any_element()
}

fn editor_box(editor: &Entity<EditorState>, cx: &App) -> AnyElement {
    div()
        .size_full()
        .border_1()
        .border_color(glass(cx).hairline)
        .rounded(px(6.))
        .child(Editor::new(editor).size_full().bordered(false))
        .into_any_element()
}

impl Render for BreakpointView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = glass(cx);
        let start_line: AnyElement = if self.is_request {
            div()
                .flex()
                .gap(px(8.))
                .child(div().w(px(110.)).child(labeled(
                    "Method",
                    Input::new(&self.method).into_any_element(),
                    cx,
                )))
                .child(div().flex_1().child(labeled(
                    "URL",
                    Input::new(&self.url).into_any_element(),
                    cx,
                )))
                .into_any_element()
        } else {
            div()
                .w(px(140.))
                .child(labeled(
                    "Status",
                    Input::new(&self.status).into_any_element(),
                    cx,
                ))
                .into_any_element()
        };
        let body: AnyElement = match &self.body {
            Some(body) => div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(p.text_secondary)
                        .child("Body"),
                )
                .child(div().flex_1().min_h(px(0.)).child(editor_box(body, cx)))
                .into_any_element(),
            None => div()
                .text_size(px(12.))
                .text_color(p.text_secondary)
                .child("Body is binary or was not loaded — it will be sent unchanged.")
                .into_any_element(),
        };
        let abort = Button::new("bp-abort")
            .label("Abort")
            .danger()
            .on_click(cx.listener(|view, _, window, cx| view.abort(window, cx)));
        let execute = Button::new("bp-execute")
            .label("Execute  ⌘↩")
            .primary()
            .on_click(
                cx.listener(|view, _, window, cx| view.execute(&ExecuteBreakpoint, window, cx)),
            );
        div()
            .id("breakpoint")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::execute))
            .size_full()
            .flex()
            .flex_col()
            .bg(p.window)
            .text_color(p.text)
            .child(window_shell::title_bar(self.title(), cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .p(px(12.))
                    .child(start_line)
                    .child(
                        div()
                            .flex_none()
                            .h(px(170.))
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(p.text_secondary)
                                    .child("Headers — one per line, Name: value"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(0.))
                                    .child(editor_box(&self.headers, cx)),
                            ),
                    )
                    .child(body),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(p.hairline)
                    .when_some(self.error.clone(), |el, error| {
                        el.child(
                            div()
                                .text_size(px(12.))
                                .text_color(p.tone(ToneName::Red).fg)
                                .child(format!("⚠ {error}")),
                        )
                    })
                    .child(div().flex_1())
                    .child(abort)
                    .child(execute),
            )
    }
}
