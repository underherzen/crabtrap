//! Окна Script Editor: по одному на скрипт (повторное открытие активирует
//! существующее), новый скрипт — своё окно с заранее назначенным id.
//! Закрытие окна приложение не завершает (как у Tools).

use std::collections::HashMap;

use application::use_cases::rules::location_form::LocationForm;
use application::use_cases::rules::script_edit::ScriptDraft;
use domain::value_objects::RuleId;
use gpui_kit::component::Root;
use gpui_kit::{App, AppContext as _, Entity, Global, KeyBinding, WindowHandle};

use super::console::ScriptConsoleStore;
use super::editor::{KEY_CONTEXT, SaveScript, ScriptEditor, ScriptTools};
use crate::ui::rules::store::RulesStore;
use crate::ui::window_shell;

const EDITOR_WIDTH: f32 = 1100.;
const EDITOR_HEIGHT: f32 = 760.;

struct ScriptEditorWindows {
    store: Entity<RulesStore>,
    console: Entity<ScriptConsoleStore>,
    tools: ScriptTools,
    open: HashMap<RuleId, WindowHandle<Root>>,
}

impl Global for ScriptEditorWindows {}

/// Зарегистрировать окна редакторов (один раз на приложение).
pub fn install(
    store: Entity<RulesStore>,
    console: Entity<ScriptConsoleStore>,
    tools: ScriptTools,
    cx: &mut App,
) {
    cx.bind_keys([KeyBinding::new("cmd-s", SaveScript, Some(KEY_CONTEXT))]);
    cx.set_global(ScriptEditorWindows {
        store,
        console,
        tools,
        open: HashMap::new(),
    });
}

/// Открыть редактор существующего скрипта.
pub fn open_rule(id: RuleId, cx: &mut App) {
    if activate(id, cx) {
        return;
    }
    let Some(windows) = cx.try_global::<ScriptEditorWindows>() else {
        tracing::warn!("script editor requested before install");
        return;
    };
    let rule = windows
        .store
        .read(cx)
        .rules
        .scripts
        .iter()
        .find(|r| r.id == id)
        .cloned();
    match rule {
        Some(rule) => {
            let draft = ScriptDraft::from_rule(&rule);
            open_window(id, Some(draft.clone()), draft, cx);
        }
        None => tracing::warn!(%id, "script to edit not found"),
    }
}

/// Новый скрипт (`location` — из трафика или пустой).
pub fn open_new(location: LocationForm, cx: &mut App) {
    open_window(RuleId::new(), None, ScriptDraft::new_for(location), cx);
}

/// Окно скрипта уже открыто — поднять его.
fn activate(id: RuleId, cx: &mut App) -> bool {
    let Some(handle) = cx
        .try_global::<ScriptEditorWindows>()
        .and_then(|w| w.open.get(&id).copied())
    else {
        return false;
    };
    cx.activate(true);
    handle
        .update(cx, |_, window, _| window.activate_window())
        .is_ok()
}

fn open_window(id: RuleId, saved: Option<ScriptDraft>, draft: ScriptDraft, cx: &mut App) {
    let Some(windows) = cx.try_global::<ScriptEditorWindows>() else {
        tracing::warn!("script editor requested before install");
        return;
    };
    let (store, console, tools) = (
        windows.store.clone(),
        windows.console.clone(),
        windows.tools.clone(),
    );
    cx.activate(true);
    let options =
        window_shell::window_options("crabtrap — Script Editor", EDITOR_WIDTH, EDITOR_HEIGHT, cx);
    let opened = cx.open_window(options, |window, cx| {
        let editor =
            cx.new(|cx| ScriptEditor::new(store, console, tools, id, saved, draft, window, cx));
        editor.update(cx, |editor, cx| editor.focus(window, cx));
        window_shell::transparent_root(gpui_kit::AnyView::from(editor), window, cx)
    });
    let Ok(handle) = opened else {
        tracing::warn!("cannot open script editor window");
        return;
    };
    cx.global_mut::<ScriptEditorWindows>()
        .open
        .insert(id, handle);
    let window_id = handle.window_id();
    cx.on_window_closed(move |cx, closed| {
        if closed == window_id && cx.has_global::<ScriptEditorWindows>() {
            cx.global_mut::<ScriptEditorWindows>().open.remove(&id);
        }
    })
    .detach();
}
