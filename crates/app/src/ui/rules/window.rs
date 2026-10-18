//! Отдельное окно «crabtrap — Tools» (как диалоги Charles): правила
//! открываются рядом с трафиком, а не вместо него. Окно одно: повторное
//! открытие активирует его и переключает вкладку. Закрытие окна Tools
//! приложение не завершает (см. `main`: выход — только по главному окну).

use gpui_kit::component::Root;
use gpui_kit::{App, AppContext as _, Entity, Global, WindowHandle};

use super::store::RulesStore;
use super::{RulesScreen, RulesTab};
use crate::ui::window_shell;
use application::use_cases::rules::location_form::LocationForm;

/// Размер окна Tools по умолчанию.
const TOOLS_WIDTH: f32 = 980.;
const TOOLS_HEIGHT: f32 = 640.;

/// Глобальное состояние окна Tools: общий store правил и (если открыто)
/// хендл окна с экраном.
struct ToolsWindow {
    store: Entity<RulesStore>,
    open: Option<(WindowHandle<Root>, Entity<RulesScreen>)>,
}

impl Global for ToolsWindow {}

/// Зарегистрировать store правил (один на приложение).
pub fn install(store: Entity<RulesStore>, cx: &mut App) {
    cx.set_global(ToolsWindow { store, open: None });
}

/// Store правил (для правок вне окна Tools — контекстное меню трафика).
pub fn store(cx: &App) -> Option<Entity<RulesStore>> {
    cx.try_global::<ToolsWindow>()
        .map(|tools| tools.store.clone())
}

/// Открыть Tools на вкладке; `prefill` — сразу открыть диалог добавления
/// с подставленными полями (правый клик в трафике).
pub fn open(tab: RulesTab, prefill: Option<LocationForm>, cx: &mut App) {
    if !cx.has_global::<ToolsWindow>() {
        tracing::warn!("tools window requested before install");
        return;
    }
    // Из иконки в строке меню приложение не на переднем плане.
    cx.activate(true);
    if let Some((handle, screen)) = cx.global::<ToolsWindow>().open.clone() {
        let shown = handle.update(cx, |_, window, cx| {
            window.activate_window();
            screen.update(cx, |screen, cx| {
                screen.open_tab(tab, prefill.clone(), window, cx)
            });
        });
        if shown.is_ok() {
            return;
        }
        // Окно закрыли — хендл протух, откроем заново.
    }
    let store = cx.global::<ToolsWindow>().store.clone();
    let options = window_shell::window_options("crabtrap — Tools", TOOLS_WIDTH, TOOLS_HEIGHT, cx);
    let mut created: Option<Entity<RulesScreen>> = None;
    let opened = cx.open_window(options, |window, cx| {
        let screen = cx.new(|cx| RulesScreen::new(store, window, cx));
        created = Some(screen.clone());
        window_shell::transparent_root(gpui_kit::AnyView::from(screen), window, cx)
    });
    let (Ok(handle), Some(screen)) = (opened, created) else {
        tracing::warn!("cannot open tools window");
        return;
    };
    let _ = handle.update(cx, |_, window, cx| {
        screen.update(cx, |screen, cx| screen.open_tab(tab, prefill, window, cx));
    });
    cx.global_mut::<ToolsWindow>().open = Some((handle, screen));
    // Окно закрыли — отпустить экран (иначе висит до следующего открытия).
    let tools_id = handle.window_id();
    cx.on_window_closed(move |cx, id| {
        // Слот чистим, только если закрыто именно окно из слота.
        let ours = cx
            .try_global::<ToolsWindow>()
            .and_then(|t| t.open.as_ref().map(|(h, _)| h.window_id() == id))
            .unwrap_or(false);
        if id == tools_id && ours {
            cx.global_mut::<ToolsWindow>().open = None;
        }
    })
    .detach();
}
