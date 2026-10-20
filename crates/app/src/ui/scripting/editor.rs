//! Окно Script Editor (как в Proxyman): Matching Rule (Name, Host, Path,
//! Method), галочки Request / Response / Run as Mock API, статус
//! сохранения, редактор JS и консоль справа, Save & Activate (⌘S).
//!
//! Черновик и сохранение — `application::rules::script_edit`; правило
//! пишется в rules.json и в движок через общий [`RulesStore`].

use application::dto::scripting::ScriptLogLevel;
use application::use_cases::rules::script_edit::{ScriptDraft, upsert_script};
use domain::value_objects::RuleId;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement, Render, Styled, Subscription, Window, div, px,
};

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use application::services::ScriptRuntime;
use application::use_cases::scripting::TestScriptUseCase;
use application::use_cases::scripting::snippets::SNIPPETS;

use super::console::{ConsoleLine, ScriptConsoleStore};
use crate::ui::async_bridge::spawn_ui_task;
use crate::ui::inspector::kv::copy_text;
use crate::ui::prefs;
use crate::ui::rules::match_fields::{MatchFields, MatchFieldsKind, row};
use crate::ui::rules::store::RulesStore;
use crate::ui::theme::{ToneName, glass};
use crate::ui::window_shell;
use gpui_kit::base::{h_resizable, resizable_panel};
use gpui_kit::component::Disableable as _;

gpui_kit::actions!(script_editor, [SaveScript]);

/// Контекст клавиш окна (⌘S — только здесь).
pub const KEY_CONTEXT: &str = "ScriptEditor";
/// Пределы ширины консоли при перетаскивании разделителя, px.
const CONSOLE_MIN_WIDTH: f32 = 200.;
const CONSOLE_MAX_WIDTH: f32 = 1400.;

/// Сервисы окна: пробный запуск, сброс `$sharedState`, tokio-рантайм.
#[derive(Clone)]
pub struct ScriptTools {
    pub test: Arc<TestScriptUseCase>,
    pub script_runtime: Arc<dyn ScriptRuntime>,
    pub runtime: tokio::runtime::Handle,
    /// Ширина консоли (общая для окон; биты f32 — сохраняется в `[ui]`).
    pub console_width: Arc<AtomicU32>,
}

impl ScriptTools {
    fn console_width(&self) -> f32 {
        f32::from_bits(self.console_width.load(Ordering::Relaxed))
    }
}

pub struct ScriptEditor {
    store: Entity<RulesStore>,
    console: Entity<ScriptConsoleStore>,
    tools: ScriptTools,
    /// Id назначен сразу: новое правило вставляется с ним при первом
    /// сохранении, консоль и окно привязаны к нему.
    rule_id: RuleId,
    /// Последнее сохранённое состояние (`None` — ещё не сохранялся).
    saved: Option<ScriptDraft>,
    name: Entity<InputState>,
    /// Matching Rule: Host / Path / Regex / Method / GraphQL / Test URL.
    location: Entity<MatchFields>,
    run_on_request: bool,
    run_on_response: bool,
    mock_api: bool,
    code: Entity<EditorState>,
    /// Консоль — read-only редактор: выделение мышью, ⌘A / ⌘C.
    console_view: Entity<EditorState>,
    /// Новые записи пришли, пока пользователь выделял текст: обновить,
    /// когда выделение снято (иначе оно пропадало бы на каждой записи).
    console_stale: bool,
    save_error: Option<String>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl ScriptEditor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Entity<RulesStore>,
        console: Entity<ScriptConsoleStore>,
        tools: ScriptTools,
        rule_id: RuleId,
        saved: Option<ScriptDraft>,
        draft: ScriptDraft,
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
        let name = input("Script name", &draft.name);
        let code = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("javascript")
                .line_number(true);
            state.set_value(draft.source.clone(), window, cx);
            state
        });
        // Вывод консоли чаще всего JSON — подсветка JSON.
        let console_text = console.read(cx).text(&rule_id);
        let console_view = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("json")
                .soft_wrap(true);
            state.set_value(console_text, window, cx);
            state
        });
        // Статус «Unsaved changes» и превью Matches — на каждый ввод.
        let location =
            cx.new(|cx| MatchFields::new(MatchFieldsKind::Full, &draft.location, window, cx));
        let mut subscriptions = vec![
            cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.observe(&location, |_, _, cx| cx.notify()),
        ];
        subscriptions.push(cx.subscribe(&code, |_, _, _: &InputEvent, cx| cx.notify()));
        subscriptions.push(cx.observe_in(&console, window, |editor, _, window, cx| {
            editor.sync_console(window, cx)
        }));
        subscriptions.push(cx.observe(&store, |_, _, cx| cx.notify()));
        Self {
            store,
            console,
            tools,
            rule_id,
            saved,
            name,
            location,
            run_on_request: draft.run_on_request,
            run_on_response: draft.run_on_response,
            mock_api: draft.mock_api,
            code,
            console_view,
            console_stale: false,
            save_error: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    fn draft(&self, cx: &App) -> ScriptDraft {
        ScriptDraft {
            name: self.name.read(cx).value().to_string(),
            location: self.location.read(cx).form(cx),
            run_on_request: self.run_on_request,
            run_on_response: self.run_on_response,
            mock_api: self.mock_api,
            source: self.code.read(cx).value().to_string(),
        }
    }

    /// Save & Activate: проверить location, сохранить правило (новое —
    /// вставить с назначенным id), горячая перезагрузка движка — в store.
    fn save(&mut self, _: &SaveScript, _: &mut Window, cx: &mut Context<Self>) {
        let draft = self.draft(cx);
        self.save_error = None;
        if !self.location_valid(&draft, cx) {
            return;
        }
        let id = self.rule_id;
        let editor = cx.entity().downgrade();
        let saving = draft.clone();
        RulesStore::apply_then(
            &self.store,
            cx,
            move |rules| upsert_script(rules, Some(id), saving).map(|(rules, _)| rules),
            move |result, cx| {
                let _ = editor.update(cx, |editor, cx| {
                    match result {
                        Ok(()) => editor.saved = Some(draft),
                        Err(error) => editor.save_error = Some(error),
                    }
                    cx.notify();
                });
            },
        );
    }

    /// Показать ошибки условия под полями; `true` — условие в порядке.
    fn location_valid(&mut self, draft: &ScriptDraft, cx: &mut Context<Self>) -> bool {
        let errors = draft.location.to_match().err().unwrap_or_default();
        let valid = errors.is_empty();
        self.location
            .update(cx, |fields, cx| fields.set_errors(errors, cx));
        cx.notify();
        valid
    }

    /// Вставить сниппет в место курсора.
    fn insert_snippet(&mut self, code: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.code
            .update(cx, |state, cx| state.insert(code, window, cx));
        cx.notify();
    }

    /// Test: черновик (без сохранения) на последнем подходящем обмене —
    /// итог и вывод скрипта в консоль.
    fn run_test(&mut self, cx: &mut Context<Self>) {
        let draft = self.draft(cx);
        if !self.location_valid(&draft, cx) {
            return;
        }
        let (test, rule) = (self.tools.test.clone(), self.rule_id);
        self.log(ScriptLogLevel::Debug, "Testing…".into(), cx);
        let entity = cx.entity();
        spawn_ui_task(
            cx,
            &entity,
            self.tools.runtime.clone(),
            async move { test.execute(rule, &draft).await },
            |editor: &mut Self, result, cx| match result {
                Ok(lines) => {
                    for line in lines {
                        editor.log(line.level, line.message, cx);
                    }
                }
                Err(error) => editor.log(ScriptLogLevel::Error, error.to_string(), cx),
            },
        );
    }

    fn log(&self, level: ScriptLogLevel, message: String, cx: &mut App) {
        let rule = self.rule_id;
        self.console.update(cx, |console, cx| {
            console.push(
                rule,
                ConsoleLine {
                    at: chrono::Local::now(),
                    level,
                    message,
                },
            );
            cx.notify();
        });
    }

    fn bottom_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        let snippets = Button::new("script-snippets")
            .label("Snippets ▾")
            .dropdown_menu({
                let weak = weak.clone();
                move |menu, _, _| {
                    SNIPPETS.iter().fold(menu, |menu, snippet| {
                        let weak = weak.clone();
                        let code = snippet.code;
                        menu.item(PopupMenuItem::new(snippet.title).on_click(
                            move |_, window, cx| {
                                let _ = weak.update(cx, |editor, cx| {
                                    editor.insert_snippet(code, window, cx)
                                });
                            },
                        ))
                    })
                }
            });
        let test = Button::new("script-test")
            .label("Test on latest request")
            .on_click(cx.listener(|editor, _, _, cx| editor.run_test(cx)));
        let more = Button::new("script-more").label("More ▾").dropdown_menu({
            let tools = self.tools.clone();
            let weak = weak.clone();
            move |menu, _, _| {
                let tools = tools.clone();
                let weak = weak.clone();
                menu.item(
                    PopupMenuItem::new("Reset $sharedState").on_click(move |_, _, cx| {
                        tools.script_runtime.clear_shared_state();
                        let _ = weak.update(cx, |editor, cx| {
                            editor.log(ScriptLogLevel::Info, "$sharedState cleared".into(), cx)
                        });
                    }),
                )
            }
        });
        let save = Button::new("script-save")
            .label("Save & Activate  ⌘S")
            .primary()
            .on_click(cx.listener(|editor, _, window, cx| editor.save(&SaveScript, window, cx)));
        let p = glass(cx);
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .border_t_1()
            .border_color(p.hairline)
            .child(snippets)
            .child(test)
            .child(more)
            .child(div().flex_1())
            .child(save)
            .into_any_element()
    }

    /// Строка статуса под галочками: текст и тон.
    fn status(&self, cx: &App) -> (String, ToneName) {
        if let Some(error) = &self.save_error {
            return (format!("⚠ {error}"), ToneName::Red);
        }
        let Some(saved) = &self.saved else {
            return (
                "Not saved yet — Save & Activate (⌘S)".into(),
                ToneName::Gray,
            );
        };
        if *saved != self.draft(cx) {
            return ("● Unsaved changes".into(), ToneName::Orange);
        }
        let rules = &self.store.read(cx).rules;
        let active = rules.scripts_enabled
            && rules
                .scripts
                .iter()
                .any(|r| r.id == self.rule_id && r.enabled);
        if active {
            ("● Saved and Active".into(), ToneName::Green)
        } else {
            (
                "● Saved, not active (disabled in Tools → Scripting)".into(),
                ToneName::Gray,
            )
        }
    }

    fn matching_rule(&self, cx: &mut Context<Self>) -> AnyElement {
        let danger = cx.theme().danger;
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(row(
                "Name",
                Input::new(&self.name).into_any_element(),
                None,
                danger,
            ))
            .child(self.location.clone())
            .into_any_element()
    }

    fn run_on(&self, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        let toggle =
            |id: &'static str, label: &'static str, checked: bool, set: fn(&mut Self, bool)| {
                let weak = weak.clone();
                Checkbox::new(id)
                    .label(label)
                    .checked(checked)
                    .on_click(move |checked, _, cx| {
                        let checked = *checked;
                        let _ = weak.update(cx, |editor, cx| {
                            set(editor, checked);
                            cx.notify();
                        });
                    })
            };
        let (status, tone) = self.status(cx);
        let tone = glass(cx).tone(tone).fg;
        div()
            .flex()
            .items_center()
            .gap(px(16.))
            .child(div().text_size(px(13.)).child("Run Script on:"))
            .child(toggle(
                "script-on-request",
                "Request",
                self.run_on_request,
                |e, v| e.run_on_request = v,
            ))
            .child(toggle(
                "script-on-response",
                "Response",
                self.run_on_response,
                |e, v| e.run_on_response = v,
            ))
            .child(toggle(
                "script-mock",
                "Run as Mock API",
                self.mock_api,
                |e, v| e.mock_api = v,
            ))
            .child(div().flex_1())
            .child(div().text_size(px(12.)).text_color(tone).child(status))
            .into_any_element()
    }

    /// Обновить текст консоли из store. Пока в ней выделен текст — отложить
    /// (до снятия выделения); после обновления — прокрутка к последней записи.
    fn sync_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.console.read(cx).text(&self.rule_id);
        let view = self.console_view.read(cx);
        if view.value().as_ref() == text {
            self.console_stale = false;
            return;
        }
        if !view.selected_range().is_empty() {
            self.console_stale = true;
            return;
        }
        let end = text.len();
        self.console_view.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_selected_range(end..end, cx);
        });
        self.console_stale = false;
        cx.notify();
    }

    fn console_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = glass(cx);
        let empty = self.console.read(cx).lines(&self.rule_id).next().is_none();
        let (console, rule) = (self.console.clone(), self.rule_id);
        let copy_all = {
            let console = self.console.clone();
            Button::new("script-console-copy")
                .label("Copy all")
                .small()
                .disabled(empty)
                .on_click(move |_, window, cx| {
                    let text = console.read(cx).text(&rule);
                    copy_text(&text, "Console", window, cx);
                })
        };
        let clear = Button::new("script-console-clear")
            .label("Clear")
            .small()
            .on_click(move |_, _, cx| {
                console.update(cx, |store, cx| {
                    store.clear(&rule);
                    cx.notify();
                })
            });
        let body = match empty {
            true => div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .child(div().text_size(px(13.)).child("Empty Console"))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(p.text_secondary)
                        .child("Use console.log() to log events"),
                )
                .into_any_element(),
            false => div()
                .flex_1()
                .min_h(px(0.))
                .child(
                    Editor::new(&self.console_view)
                        .readonly(true)
                        .size_full()
                        .bordered(false),
                )
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(p.hairline)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(10.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(p.hairline)
                    .child(div().flex_1().text_size(px(12.)).child("Console"))
                    .child(copy_all)
                    .child(clear),
            )
            .child(body)
            .into_any_element()
    }

    /// Редактор кода и консоль с перетаскиваемой границей; ширина консоли
    /// запоминается (общая для окон и между запусками).
    fn workspace(&self, console: AnyElement) -> AnyElement {
        let width = self.tools.console_width();
        let shared = self.tools.console_width.clone();
        h_resizable("script-editor-split")
            .on_resize(move |state, _, cx| {
                let Some(width) = state.read(cx).sizes().last().map(|w| f32::from(*w)) else {
                    return;
                };
                shared.store(width.to_bits(), Ordering::Relaxed);
                prefs::save_ui(move |ui| config::UiConfig {
                    script_console_width: width,
                    ..ui
                });
            })
            .child(resizable_panel().child(Editor::new(&self.code).size_full().bordered(false)))
            .child(
                resizable_panel()
                    .size(px(width))
                    .size_range(px(CONSOLE_MIN_WIDTH)..px(CONSOLE_MAX_WIDTH))
                    .child(console),
            )
            .into_any_element()
    }
}

impl Render for ScriptEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.console_stale {
            self.sync_console(window, cx);
        }
        let p = glass(cx);
        let title = format!("Script Editor — {}", self.name.read(cx).value());
        let matching = self.matching_rule(cx);
        let run_on = self.run_on(cx);
        let console = self.console_panel(cx);
        let workspace = self.workspace(console);
        let bottom = self.bottom_bar(cx);
        div()
            .id("script-editor")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::save))
            .size_full()
            .flex()
            .flex_col()
            .bg(p.window)
            .text_color(p.text)
            .child(window_shell::title_bar(title, cx))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(16.))
                    .pb(px(10.))
                    .border_b_1()
                    .border_color(p.hairline)
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(p.text_secondary)
                            .child("Matching Rule"),
                    )
                    .child(matching)
                    .child(run_on),
            )
            .child(div().flex_1().min_h(px(0.)).child(workspace))
            .child(bottom)
    }
}
