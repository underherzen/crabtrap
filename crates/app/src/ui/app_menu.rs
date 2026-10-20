//! Строка меню macOS (NSMenu через `cx.set_menus`): crabtrap · Edit · Proxy ·
//! View · Tools · Window · Help. Settings… (⌘,) открывает окно настроек.
//!
//! Пункты — GPUI-действия: горячие клавиши из `bind_keys` показываются в
//! меню сами, Edit-действия (Copy/Paste/…) доходят до поля ввода в фокусе.
//! Состав меню — чистая функция [`menus`] от [`MenuState`] (галочки,
//! Start/Stop); при изменении сторов меню пересобирается, но только если
//! состояние реально поменялось (TrafficStore шумит на каждый батч).

use std::cell::Cell;
use std::rc::Rc;

use application::dto::proxy_config::{EngineCommand, ProxySettingsDto};
use config::InspectorLayout;
use gpui_kit::component::Root;
use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::component::notification::Notification;
use gpui_kit::{Action, App, Entity, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType};

use crate::app_state::AppState;
use crate::ui::detail::DetailStore;
use crate::ui::rules::RulesTab;
use crate::ui::rules::window as rules_window;
use crate::ui::settings::{self, SettingsSection};
use crate::ui::sidebar::SidebarStore;
use crate::ui::ssl_proxying::SslProxyingStore;
use crate::ui::status_bar::EngineStatusStore;
use crate::ui::system_proxy::SystemProxyStore;
use crate::ui::traffic::filter_bar::FocusFilter;
use crate::ui::traffic::search::FocusSearch;
use crate::ui::traffic::store::TrafficStore;
use crate::ui::traffic::{actions, context_menu};
use crate::ui::updates;

gpui_kit::actions!(
    crabtrap,
    [
        About,
        CheckForUpdates,
        OpenUserGuide,
        HideApp,
        HideOthers,
        ShowAll,
        ToggleEngine,
        ToggleRecording,
        TogglePause,
        ToggleSslProxying,
        ToggleSystemProxy,
        ClearTraffic,
        OpenSettings,
        OpenCertificateSettings,
        ToggleSidebar,
        InspectorBottom,
        InspectorRight,
        OpenSslProxyingTool,
        OpenBlockList,
        OpenMapLocal,
        OpenMapRemote,
        OpenThrottle,
        OpenRewrite,
        OpenBreakpoints,
        OpenCompose,
        OpenScripting,
        RepeatSelected,
        BreakpointSelected,
        MinimizeWindow,
        ZoomWindow,
    ]
);

/// Всё, от чего зависят галочки и подписи меню.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuState {
    pub running: bool,
    pub recording: bool,
    pub paused: bool,
    pub ssl_proxying: bool,
    pub system_proxy: bool,
    pub sidebar: bool,
    pub inspector_right: bool,
}

/// Действие пункта Tools для вкладки окна правил.
fn tool_action(tab: RulesTab) -> Box<dyn Action> {
    match tab {
        RulesTab::SslProxying => Box::new(OpenSslProxyingTool),
        RulesTab::BlockList => Box::new(OpenBlockList),
        RulesTab::MapLocal => Box::new(OpenMapLocal),
        RulesTab::MapRemote => Box::new(OpenMapRemote),
        RulesTab::Throttle => Box::new(OpenThrottle),
        RulesTab::Rewrite => Box::new(OpenRewrite),
        RulesTab::Breakpoints => Box::new(OpenBreakpoints),
        RulesTab::Scripting => Box::new(OpenScripting),
    }
}

fn check(name: &'static str, action: impl Action, checked: bool) -> MenuItem {
    MenuItem::Action {
        name: name.into(),
        action: Box::new(action),
        os_action: None,
        checked,
        disabled: false,
    }
}

/// Состав строки меню для текущего состояния.
pub fn menus(state: &MenuState) -> Vec<Menu> {
    let engine_label = if state.running {
        "Stop Proxy"
    } else {
        "Start Proxy"
    };
    vec![
        Menu::new("crabtrap").items([
            MenuItem::action("About crabtrap", About),
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide crabtrap", HideApp),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit crabtrap", crate::shutdown::Quit),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find in Headers and Bodies…", FocusSearch),
            MenuItem::action("Filter Requests…", FocusFilter),
        ]),
        Menu::new("Proxy").items([
            MenuItem::action(engine_label, ToggleEngine),
            MenuItem::separator(),
            check("Recording", ToggleRecording, state.recording),
            check("Pause Updates", TogglePause, state.paused),
            MenuItem::separator(),
            check("SSL Proxying", ToggleSslProxying, state.ssl_proxying),
            check("macOS Proxy", ToggleSystemProxy, state.system_proxy),
            MenuItem::separator(),
            MenuItem::action("Clear Traffic", ClearTraffic),
        ]),
        Menu::new("View").items([
            check("Show Sidebar", ToggleSidebar, state.sidebar),
            MenuItem::separator(),
            check(
                "Inspector at Bottom",
                InspectorBottom,
                !state.inspector_right,
            ),
            check("Inspector on Right", InspectorRight, state.inspector_right),
        ]),
        Menu::new("Tools").items(
            RulesTab::ALL
                .iter()
                .map(|&tab| MenuItem::Action {
                    name: format!("{}…", tab.label()).into(),
                    action: tool_action(tab),
                    os_action: None,
                    checked: false,
                    disabled: false,
                })
                .chain([
                    MenuItem::separator(),
                    MenuItem::action("Compose Request…", OpenCompose),
                    MenuItem::action("Repeat Selected Request", RepeatSelected),
                    MenuItem::action("Breakpoint for Selected Request…", BreakpointSelected),
                ]),
        ),
        Menu::new("Window").items([
            MenuItem::action("Minimize", MinimizeWindow),
            MenuItem::action("Zoom", ZoomWindow),
        ]),
        Menu::new("Help").items([
            MenuItem::action("User Guide", OpenUserGuide),
            MenuItem::action("Certificate Setup…", OpenCertificateSettings),
        ]),
    ]
}

/// Key context корня главного окна (для его горячих клавиш).
pub const MAIN_WINDOW_CONTEXT: &str = "CrabtrapMain";

/// Горячие клавиши пунктов меню.
fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-h", HideApp, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-l", FocusFilter, None),
        // Очистка и запись — только в главном окне (не в Tools/Settings).
        KeyBinding::new("cmd-r", ToggleRecording, Some(MAIN_WINDOW_CONTEXT)),
        KeyBinding::new("cmd-k", ClearTraffic, Some(MAIN_WINDOW_CONTEXT)),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("ctrl-cmd-s", ToggleSidebar, None),
        KeyBinding::new("alt-cmd-p", OpenSslProxyingTool, None),
        KeyBinding::new("alt-cmd-b", OpenBlockList, None),
        KeyBinding::new("alt-cmd-l", OpenMapLocal, None),
        KeyBinding::new("alt-cmd-r", OpenMapRemote, None),
        KeyBinding::new("alt-cmd-t", OpenThrottle, None),
        KeyBinding::new("alt-cmd-e", OpenRewrite, None),
        KeyBinding::new("alt-cmd-k", OpenBreakpoints, None),
        KeyBinding::new("cmd-shift-n", OpenCompose, None),
        // ⌥⌘I — как Scripting в Proxyman.
        KeyBinding::new("alt-cmd-i", OpenScripting, None),
        // Над выбранной строкой трафика — как в Charles (⌘R занят записью).
        KeyBinding::new("ctrl-r", RepeatSelected, Some(MAIN_WINDOW_CONTEXT)),
        KeyBinding::new("ctrl-alt-b", BreakpointSelected, Some(MAIN_WINDOW_CONTEXT)),
        KeyBinding::new("cmd-m", MinimizeWindow, None),
    ]
}

/// Сторы и порты, которыми управляет меню.
#[derive(Clone)]
pub struct AppMenuDeps {
    pub state: AppState,
    pub runtime: tokio::runtime::Handle,
    pub status: Entity<EngineStatusStore>,
    pub traffic: Entity<TrafficStore>,
    pub ssl_proxying: Entity<SslProxyingStore>,
    pub system_proxy: Entity<SystemProxyStore>,
    pub sidebar: Entity<SidebarStore>,
    pub detail: Entity<DetailStore>,
}

impl AppMenuDeps {
    fn menu_state(&self, cx: &App) -> MenuState {
        let traffic = self.traffic.read(cx);
        MenuState {
            running: self.status.read(cx).listening.is_some(),
            recording: traffic.recording,
            paused: traffic.paused,
            ssl_proxying: self.ssl_proxying.read(cx).enabled,
            system_proxy: self.system_proxy.read(cx).enabled(),
            sidebar: self.sidebar.read(cx).visible,
            inspector_right: self.detail.read(cx).layout == InspectorLayout::Right,
        }
    }

    fn settings(&self) -> ProxySettingsDto {
        self.state.proxy_settings()
    }
}

/// Поставить меню, зарегистрировать обработчики и пересборку по сторам.
pub fn install(deps: AppMenuDeps, cx: &mut App) {
    cx.bind_keys(key_bindings());
    register_handlers(&deps, cx);

    let last = Rc::new(Cell::new(deps.menu_state(cx)));
    cx.set_menus(menus(&last.get()));

    macro_rules! resync_on {
        ($entity:expr) => {{
            let (deps, last) = (deps.clone(), last.clone());
            cx.observe(&$entity, move |_, cx| resync(&deps, &last, cx))
                .detach();
        }};
    }
    resync_on!(deps.status);
    resync_on!(deps.traffic);
    resync_on!(deps.ssl_proxying);
    resync_on!(deps.system_proxy);
    resync_on!(deps.sidebar);
    resync_on!(deps.detail);
}

fn resync(deps: &AppMenuDeps, last: &Rc<Cell<MenuState>>, cx: &mut App) {
    let state = deps.menu_state(cx);
    if state != last.get() {
        last.set(state);
        cx.set_menus(menus(&state));
    }
}

fn register_handlers(deps: &AppMenuDeps, cx: &mut App) {
    macro_rules! on {
        ($action:ty, |$deps:ident, $cx:ident| $body:expr) => {{
            let $deps = deps.clone();
            cx.on_action(move |_: &$action, $cx: &mut App| $body);
        }};
    }
    // Действия, трогающие окна: глобальный обработчик вызывается ВНУТРИ
    // update окна, из которого пришло действие, и `handle.update` этого же
    // окна вернул бы ошибку («window not found»). Откладываем на после.
    macro_rules! on_deferred {
        ($action:ty, |$cx:ident| $body:expr) => {{
            cx.on_action(move |_: &$action, cx: &mut App| cx.defer(move |$cx| $body));
        }};
    }
    on_deferred!(About, |cx| show_about(cx));
    on_deferred!(OpenUserGuide, |cx| cx
        .open_url(&user_guide_url(env!("CARGO_PKG_REPOSITORY"))));
    on!(CheckForUpdates, |deps, cx| updates::check(
        &deps.state,
        &deps.runtime,
        updates::Trigger::Manual,
        cx
    ));
    on!(HideApp, |_deps, cx| cx.hide());
    on!(HideOthers, |_deps, cx| cx.hide_other_apps());
    on!(ShowAll, |_deps, cx| cx.unhide_other_apps());
    on!(ToggleEngine, |deps, cx| {
        if deps.status.read(cx).listening.is_some() {
            deps.state.engine_command.send(EngineCommand::Stop);
        } else {
            deps.state.engine_command.send(EngineCommand::Start {
                settings: deps.settings(),
            });
        }
    });
    on!(ToggleRecording, |deps, cx| actions::toggle_recording(
        &deps.state,
        &deps.traffic,
        cx
    ));
    on!(TogglePause, |deps, cx| actions::toggle_pause(
        &deps.traffic,
        cx
    ));
    on!(ToggleSslProxying, |deps, cx| SslProxyingStore::toggle(
        &deps.ssl_proxying,
        cx
    ));
    on!(ToggleSystemProxy, |deps, cx| SystemProxyStore::toggle(
        &deps.system_proxy,
        cx
    ));
    on!(ClearTraffic, |deps, cx| actions::clear_traffic(
        &deps.state,
        &deps.traffic,
        &deps.runtime,
        cx
    ));
    on_deferred!(OpenSettings, |cx| settings::open(
        SettingsSection::Certificates,
        cx
    ));
    on_deferred!(OpenCertificateSettings, |cx| settings::open(
        SettingsSection::Certificates,
        cx
    ));
    on!(ToggleSidebar, |deps, cx| SidebarStore::toggle_visible(
        &deps.sidebar,
        cx
    ));
    on!(InspectorBottom, |deps, cx| DetailStore::set_layout(
        &deps.detail,
        InspectorLayout::Bottom,
        cx
    ));
    on!(InspectorRight, |deps, cx| DetailStore::set_layout(
        &deps.detail,
        InspectorLayout::Right,
        cx
    ));
    on_deferred!(OpenSslProxyingTool, |cx| rules_window::open(
        RulesTab::SslProxying,
        None,
        cx
    ));
    on_deferred!(OpenBlockList, |cx| rules_window::open(
        RulesTab::BlockList,
        None,
        cx
    ));
    on_deferred!(OpenMapLocal, |cx| rules_window::open(
        RulesTab::MapLocal,
        None,
        cx
    ));
    on_deferred!(OpenMapRemote, |cx| rules_window::open(
        RulesTab::MapRemote,
        None,
        cx
    ));
    on_deferred!(OpenThrottle, |cx| rules_window::open(
        RulesTab::Throttle,
        None,
        cx
    ));
    on_deferred!(OpenCompose, |cx| crate::ui::compose::open_blank(cx));
    on_deferred!(OpenBreakpoints, |cx| rules_window::open(
        RulesTab::Breakpoints,
        None,
        cx
    ));
    on_deferred!(OpenRewrite, |cx| rules_window::open(
        RulesTab::Rewrite,
        None,
        cx
    ));
    on_deferred!(OpenScripting, |cx| rules_window::open(
        RulesTab::Scripting,
        None,
        cx
    ));
    on!(RepeatSelected, |deps, cx| {
        if let Some(id) = deps.traffic.read(cx).selected {
            crate::ui::compose::repeat(id, cx);
        }
    });
    {
        let deps = deps.clone();
        cx.on_action(move |_: &BreakpointSelected, cx: &mut App| {
            let traffic = deps.traffic.read(cx);
            let Some(row) = traffic.selected.and_then(|id| traffic.row(&id)) else {
                return;
            };
            let prefill = context_menu::rule_prefill(&row.summary);
            cx.defer(move |cx| rules_window::open(RulesTab::Breakpoints, Some(prefill), cx));
        });
    }
    on_deferred!(MinimizeWindow, |cx| with_active_window(cx, |window| {
        window.minimize_window()
    }));
    on_deferred!(ZoomWindow, |cx| with_active_window(cx, |window| window
        .zoom_window()));
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut gpui_kit::Window)) {
    if let Some(handle) = cx.active_window() {
        let _ = handle.update(cx, |_, window, _| f(window));
    }
}

/// Руководство пользователя (`docs/guide.md`) в репозитории из Cargo.toml.
fn user_guide_url(repository: &str) -> String {
    let repo = repository.trim_end_matches('/').trim_end_matches(".git");
    format!("{repo}/blob/main/docs/guide.md")
}

fn show_about(cx: &mut App) {
    let Some(handle) = cx.active_window() else {
        return;
    };
    let _ = handle.update(cx, |_, window, cx| {
        Root::update(window, cx, |root, window, cx| {
            root.push_notification(
                Notification::new()
                    .title("crabtrap")
                    .message(format!("MITM HTTP proxy · v{}", env!("CARGO_PKG_VERSION"))),
                window,
                cx,
            );
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MenuState {
        MenuState {
            running: true,
            recording: true,
            paused: false,
            ssl_proxying: false,
            system_proxy: true,
            sidebar: true,
            inspector_right: false,
        }
    }

    fn find<'a>(menus: &'a [Menu], menu: &str, item: &str) -> &'a MenuItem {
        menus
            .iter()
            .find(|m| m.name.as_ref() == menu)
            .and_then(|m| {
                m.items
                    .iter()
                    .find(|i| matches!(i, MenuItem::Action { name, .. } if name.as_ref() == item))
            })
            .unwrap_or_else(|| panic!("no {menu} → {item}"))
    }

    fn is_checked(item: &MenuItem) -> bool {
        matches!(item, MenuItem::Action { checked: true, .. })
    }

    #[test]
    fn user_guide_points_at_docs_in_the_repository() {
        assert_eq!(
            user_guide_url("https://github.com/o/crabtrap"),
            "https://github.com/o/crabtrap/blob/main/docs/guide.md"
        );
        assert_eq!(
            user_guide_url("https://github.com/o/crabtrap.git/"),
            "https://github.com/o/crabtrap/blob/main/docs/guide.md"
        );
    }

    #[test]
    fn top_level_order_follows_macos_conventions() {
        let names: Vec<String> = menus(&state()).iter().map(|m| m.name.to_string()).collect();
        assert_eq!(
            names,
            [
                "crabtrap", "Edit", "Proxy", "View", "Tools", "Window", "Help"
            ]
        );
    }

    #[test]
    fn checkmarks_follow_state() {
        let m = menus(&state());
        assert!(is_checked(find(&m, "Proxy", "Recording")));
        assert!(!is_checked(find(&m, "Proxy", "Pause Updates")));
        assert!(!is_checked(find(&m, "Proxy", "SSL Proxying")));
        assert!(is_checked(find(&m, "Proxy", "macOS Proxy")));
        find(&m, "crabtrap", "Settings…");
        find(&m, "crabtrap", "Check for Updates…");
        find(&m, "Help", "User Guide");
        assert!(is_checked(find(&m, "View", "Show Sidebar")));
        assert!(is_checked(find(&m, "View", "Inspector at Bottom")));
        assert!(!is_checked(find(&m, "View", "Inspector on Right")));
    }

    #[test]
    fn engine_item_label_reflects_running_state() {
        let running = menus(&state());
        find(&running, "Proxy", "Stop Proxy");
        let stopped = menus(&MenuState {
            running: false,
            ..state()
        });
        find(&stopped, "Proxy", "Start Proxy");
    }

    #[test]
    fn tools_menu_lists_every_rules_tab() {
        let m = menus(&state());
        let tools = m.iter().find(|m| m.name.as_ref() == "Tools").unwrap();
        // Вкладки правил + разделитель + Compose + 2 действия над выбранным.
        assert_eq!(tools.items.len(), RulesTab::ALL.len() + 4);
        for tab in RulesTab::ALL {
            find(&m, "Tools", &format!("{}…", tab.label()));
        }
        find(&m, "Tools", "Compose Request…");
        find(&m, "Tools", "Repeat Selected Request");
        find(&m, "Tools", "Breakpoint for Selected Request…");
    }

    #[test]
    fn selection_shortcuts_are_bound_in_main_window() {
        let bindings = key_bindings();
        let bound = |action: &dyn Action| bindings.iter().any(|b| b.action().partial_eq(action));
        assert!(bound(&RepeatSelected));
        assert!(bound(&BreakpointSelected));
    }
}
