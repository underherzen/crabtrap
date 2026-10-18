//! Иконка crabtrap в строке меню macOS (как у Charles): быстрые тумблеры
//! без открытия окна — Recording, SSL Proxying, macOS Proxy, Throttle,
//! Clear, показать окно, выход.
//!
//! GPUI не умеет NSStatusItem, поэтому иконка и меню — крейт `tray-icon`
//! (muda), работающий поверх уже запущенного NSApp. Клики приходят
//! глобальным обработчиком muda → async-channel → задача GPUI (главный
//! поток) → те же действия, что у тулбара. Галочки меню синхронизируются
//! наблюдателями за сторами.

use std::rc::Rc;

use application::use_cases::rules::edit::{
    THROTTLE_PRESETS, with_throttle_preset, without_throttle,
};
use gpui_kit::{AnyWindowHandle, App, Entity};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder};

use crate::app_state::AppState;
use crate::ui::rules::store::RulesStore;
use crate::ui::ssl_proxying::SslProxyingStore;
use crate::ui::system_proxy::SystemProxyStore;
use crate::ui::traffic::actions;
use crate::ui::traffic::store::TrafficStore;

const ID_SHOW: &str = "tray.show";
const ID_RECORDING: &str = "tray.recording";
const ID_SSL: &str = "tray.ssl";
const ID_SYSTEM_PROXY: &str = "tray.system-proxy";
const ID_THROTTLE_OFF: &str = "tray.throttle.off";
const ID_THROTTLE_PREFIX: &str = "tray.throttle.";
const ID_CLEAR: &str = "tray.clear";
const ID_TOOLS: &str = "tray.tools";
const ID_QUIT: &str = "tray.quit";

/// Действие пункта меню.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ShowWindow,
    ToggleRecording,
    ToggleSslProxying,
    ToggleSystemProxy,
    /// `None` — выключить throttle, `Some(ix)` — пресет `THROTTLE_PRESETS[ix]`.
    Throttle(Option<usize>),
    ClearTraffic,
    OpenTools,
    Quit,
}

/// id пункта → действие (чистая функция; нативное меню в тестах не нужно).
pub fn parse_action(id: &str) -> Option<TrayAction> {
    match id {
        ID_SHOW => Some(TrayAction::ShowWindow),
        ID_RECORDING => Some(TrayAction::ToggleRecording),
        ID_SSL => Some(TrayAction::ToggleSslProxying),
        ID_SYSTEM_PROXY => Some(TrayAction::ToggleSystemProxy),
        ID_THROTTLE_OFF => Some(TrayAction::Throttle(None)),
        ID_CLEAR => Some(TrayAction::ClearTraffic),
        ID_TOOLS => Some(TrayAction::OpenTools),
        ID_QUIT => Some(TrayAction::Quit),
        other => other
            .strip_prefix(ID_THROTTLE_PREFIX)
            .and_then(|ix| ix.parse::<usize>().ok())
            .filter(|ix| *ix < THROTTLE_PRESETS.len())
            .map(|ix| TrayAction::Throttle(Some(ix))),
    }
}

fn throttle_id(ix: usize) -> String {
    format!("{ID_THROTTLE_PREFIX}{ix}")
}

/// Сторы и порты, которыми управляет меню.
#[derive(Clone)]
pub struct TrayDeps {
    pub state: AppState,
    pub runtime: tokio::runtime::Handle,
    pub window: AnyWindowHandle,
    pub traffic: Entity<TrafficStore>,
    pub ssl_proxying: Entity<SslProxyingStore>,
    pub system_proxy: Entity<SystemProxyStore>,
    pub rules: Entity<RulesStore>,
}

/// Нативное меню (главный поток). Иконка живёт, пока жив этот объект.
struct TrayMenu {
    _icon: TrayIcon,
    recording: CheckMenuItem,
    ssl: CheckMenuItem,
    system_proxy: CheckMenuItem,
    throttle_off: CheckMenuItem,
    throttle_presets: Vec<CheckMenuItem>,
}

impl TrayMenu {
    fn build() -> Result<Self, Box<dyn std::error::Error>> {
        let check = |id: &str, text: &str| CheckMenuItem::with_id(id, text, true, false, None);
        let show = MenuItem::with_id(ID_SHOW, "Show crabtrap", true, None);
        let recording = check(ID_RECORDING, "Recording");
        let ssl = check(ID_SSL, "SSL Proxying");
        let system_proxy = check(ID_SYSTEM_PROXY, "macOS Proxy");
        let throttle_off = check(ID_THROTTLE_OFF, "Off");
        let throttle_presets: Vec<CheckMenuItem> = THROTTLE_PRESETS
            .iter()
            .enumerate()
            .map(|(ix, preset)| check(&throttle_id(ix), preset.name))
            .collect();
        let throttle = Submenu::new("Throttling", true);
        throttle.append(&throttle_off)?;
        throttle.append(&PredefinedMenuItem::separator())?;
        for item in &throttle_presets {
            throttle.append(item)?;
        }
        let clear = MenuItem::with_id(ID_CLEAR, "Clear Traffic", true, None);
        let tools = MenuItem::with_id(ID_TOOLS, "Tools…", true, None);
        let quit = MenuItem::with_id(ID_QUIT, "Quit crabtrap", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &recording,
            &ssl,
            &system_proxy,
            &throttle,
            &PredefinedMenuItem::separator(),
            &tools,
            &clear,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;
        let icon = TrayIconBuilder::new()
            .with_title("🦀")
            .with_tooltip("crabtrap")
            .with_menu(Box::new(menu))
            .build()?;
        Ok(Self {
            _icon: icon,
            recording,
            ssl,
            system_proxy,
            throttle_off,
            throttle_presets,
        })
    }

    /// Галочки по текущему состоянию сторов.
    fn sync(&self, deps: &TrayDeps, cx: &App) {
        self.recording.set_checked(deps.traffic.read(cx).recording);
        self.ssl.set_checked(deps.ssl_proxying.read(cx).enabled);
        self.system_proxy
            .set_checked(deps.system_proxy.read(cx).enabled());
        let active = deps
            .rules
            .read(cx)
            .rules
            .throttle
            .as_ref()
            .map(|t| t.name.clone());
        self.throttle_off.set_checked(active.is_none());
        for (item, preset) in self.throttle_presets.iter().zip(THROTTLE_PRESETS) {
            item.set_checked(active.as_deref() == Some(preset.name));
        }
    }
}

/// Создать иконку, подписаться на клики и на изменения сторов.
/// Ошибка (нет строки меню, отказ AppKit) — только лог: окно работает и так.
pub fn install(deps: TrayDeps, cx: &mut App) {
    let menu = match TrayMenu::build() {
        Ok(menu) => Rc::new(menu),
        Err(e) => {
            tracing::warn!(error = %e, "cannot create menu bar icon");
            return;
        }
    };
    menu.sync(&deps, cx);
    observe_stores(&menu, &deps, cx);
    // Иначе невидимые throttle-галочки не знают текущий профиль.
    RulesStore::refresh(&deps.rules, cx);

    let (tx, rx) = async_channel::unbounded::<String>();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = tx.try_send(event.id.0);
    }));
    cx.spawn(async move |cx| {
        // Меню держит задача: пока она жива — жива и иконка.
        let _menu = menu;
        while let Ok(id) = rx.recv().await {
            if let Some(action) = parse_action(&id) {
                cx.update(|cx| perform(action, &deps, cx));
            }
        }
    })
    .detach();
}

fn observe_stores(menu: &Rc<TrayMenu>, deps: &TrayDeps, cx: &mut App) {
    macro_rules! resync_on {
        ($entity:expr) => {{
            let (menu, deps) = (menu.clone(), deps.clone());
            cx.observe(&$entity, move |_, cx| menu.sync(&deps, cx))
                .detach();
        }};
    }
    resync_on!(deps.traffic);
    resync_on!(deps.ssl_proxying);
    resync_on!(deps.system_proxy);
    resync_on!(deps.rules);
}

fn perform(action: TrayAction, deps: &TrayDeps, cx: &mut App) {
    match action {
        TrayAction::ShowWindow => {
            cx.activate(true);
            let _ = deps
                .window
                .update(cx, |_, window, _| window.activate_window());
        }
        TrayAction::ToggleRecording => actions::toggle_recording(&deps.state, &deps.traffic, cx),
        TrayAction::ToggleSslProxying => SslProxyingStore::toggle(&deps.ssl_proxying, cx),
        TrayAction::ToggleSystemProxy => SystemProxyStore::toggle(&deps.system_proxy, cx),
        TrayAction::Throttle(None) => {
            RulesStore::apply(&deps.rules, cx, |rules| Ok(without_throttle(rules)));
        }
        TrayAction::Throttle(Some(ix)) => {
            RulesStore::apply(&deps.rules, cx, move |rules| {
                Ok(with_throttle_preset(rules, &THROTTLE_PRESETS[ix], None))
            });
        }
        TrayAction::ClearTraffic => {
            actions::clear_traffic(&deps.state, &deps.traffic, &deps.runtime, cx)
        }
        TrayAction::OpenTools => {
            crate::ui::rules::window::open(crate::ui::rules::RulesTab::SslProxying, None, cx)
        }
        TrayAction::Quit => cx.quit(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixed_actions() {
        assert_eq!(parse_action(ID_SHOW), Some(TrayAction::ShowWindow));
        assert_eq!(
            parse_action(ID_SYSTEM_PROXY),
            Some(TrayAction::ToggleSystemProxy)
        );
        assert_eq!(
            parse_action(ID_THROTTLE_OFF),
            Some(TrayAction::Throttle(None))
        );
        assert_eq!(parse_action(ID_QUIT), Some(TrayAction::Quit));
        assert_eq!(parse_action(ID_TOOLS), Some(TrayAction::OpenTools));
    }

    #[test]
    fn parses_throttle_presets_within_bounds() {
        assert_eq!(
            parse_action(&throttle_id(0)),
            Some(TrayAction::Throttle(Some(0)))
        );
        let last = THROTTLE_PRESETS.len() - 1;
        assert_eq!(
            parse_action(&throttle_id(last)),
            Some(TrayAction::Throttle(Some(last)))
        );
        assert_eq!(parse_action(&throttle_id(THROTTLE_PRESETS.len())), None);
        assert_eq!(parse_action("tray.throttle.x"), None);
        assert_eq!(parse_action("unknown"), None);
    }
}
