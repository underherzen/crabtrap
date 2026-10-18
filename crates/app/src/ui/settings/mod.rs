//! Окно Settings (⌘,): Network (доступ с других устройств), Certificates
//! (CA, доверие, раздача на устройства) и System Proxy (системный прокси
//! macOS). Окно одно: повторное открытие
//! активирует его и переключает раздел; закрытие приложение не завершает.

mod network_panel;
mod system_proxy_panel;

use std::sync::Arc;

use application::services::CertificateService;
use gpui_kit::component::Root;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Global, InteractiveElement as _,
    IntoElement, ParentElement, Render, StatefulInteractiveElement as _, Styled, Window,
    WindowHandle, div,
};

use crate::app_state::AppState;
use crate::ui::certificates::{CertificatePanel, CertificatesStore};
use crate::ui::network::NetworkStore;
use crate::ui::status_bar::EngineStatusStore;
use crate::ui::system_proxy::{ProxyIndicator, SystemProxyStore};
use crate::ui::theme::components::NavItem;
use crate::ui::theme::icons::AppIcon;
use crate::ui::theme::{ToneName, glass};
use crate::ui::window_shell;
use network_panel::NetworkPanel;
use system_proxy_panel::SystemProxyPanel;

const SETTINGS_WIDTH: f32 = 900.;
const SETTINGS_HEIGHT: f32 = 600.;

/// Раздел окна настроек.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Network,
    Certificates,
    SystemProxy,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 3] = [Self::Network, Self::Certificates, Self::SystemProxy];

    pub fn label(self) -> &'static str {
        match self {
            Self::Network => "Network",
            Self::Certificates => "Certificates",
            Self::SystemProxy => "System Proxy",
        }
    }
}

/// Всё, что нужно окну настроек.
#[derive(Clone)]
pub struct SettingsDeps {
    pub runtime: tokio::runtime::Handle,
    pub cert_service: Arc<dyn CertificateService>,
    pub state: AppState,
    pub status: Entity<EngineStatusStore>,
    pub sys_proxy: Entity<SystemProxyStore>,
}

struct SettingsWindow {
    deps: SettingsDeps,
    open: Option<(WindowHandle<Root>, Entity<SettingsScreen>)>,
}

impl Global for SettingsWindow {}

/// Зарегистрировать окно настроек (одно на приложение).
pub fn install(deps: SettingsDeps, cx: &mut App) {
    cx.set_global(SettingsWindow { deps, open: None });
}

/// Открыть настройки на разделе (или активировать уже открытое окно).
pub fn open(section: SettingsSection, cx: &mut App) {
    let Some((deps, open)) = cx
        .try_global::<SettingsWindow>()
        .map(|settings| (settings.deps.clone(), settings.open.clone()))
    else {
        tracing::warn!("settings window requested before install");
        return;
    };
    cx.activate(true);
    if let Some((handle, screen)) = open {
        let shown = handle.update(cx, |_, window, cx| {
            window.activate_window();
            screen.update(cx, |screen, cx| screen.show(section, cx));
        });
        if shown.is_ok() {
            return;
        }
    }
    let options =
        window_shell::window_options("crabtrap — Settings", SETTINGS_WIDTH, SETTINGS_HEIGHT, cx);
    let mut created: Option<Entity<SettingsScreen>> = None;
    let opened = cx.open_window(options, |window, cx| {
        let screen = cx.new(|cx| SettingsScreen::new(deps, section, window, cx));
        created = Some(screen.clone());
        window_shell::transparent_root(gpui_kit::AnyView::from(screen), window, cx)
    });
    let (Ok(handle), Some(screen)) = (opened, created) else {
        tracing::warn!("cannot open settings window");
        return;
    };
    let certificates = screen.read(cx).certificates.clone();
    CertificatesStore::refresh(&certificates, cx);
    cx.global_mut::<SettingsWindow>().open = Some((handle, screen));
    let settings_id = handle.window_id();
    cx.on_window_closed(move |cx, id| {
        // Слот чистим, только если закрыто именно окно из слота.
        let ours = cx
            .try_global::<SettingsWindow>()
            .and_then(|s| s.open.as_ref().map(|(h, _)| h.window_id() == id))
            .unwrap_or(false);
        if id == settings_id && ours {
            cx.global_mut::<SettingsWindow>().open = None;
        }
    })
    .detach();
}

pub struct SettingsScreen {
    section: SettingsSection,
    certificates: Entity<CertificatesStore>,
    network: Entity<NetworkStore>,
    sys_proxy: Entity<SystemProxyStore>,
    state: AppState,
}

impl SettingsScreen {
    fn new(
        deps: SettingsDeps,
        section: SettingsSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let certificates = cx.new(|cx| {
            CertificatesStore::new(
                window,
                cx,
                deps.runtime.clone(),
                deps.cert_service.clone(),
                deps.state.listen_addr.clone(),
            )
        });
        let network = cx.new(|cx| NetworkStore::new(deps.state.clone(), deps.status.clone(), cx));
        cx.observe(&certificates, |_, _, cx| cx.notify()).detach();
        cx.observe(&network, |_, _, cx| cx.notify()).detach();
        cx.observe(&deps.sys_proxy, |_, _, cx| cx.notify()).detach();
        Self {
            section,
            certificates,
            network,
            sys_proxy: deps.sys_proxy,
            state: deps.state,
        }
    }

    fn show(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        if section == SettingsSection::Certificates {
            CertificatesStore::refresh(&self.certificates, cx);
        }
        if section == SettingsSection::SystemProxy {
            SystemProxyStore::refresh(&self.sys_proxy, cx);
        }
        self.section = section;
        cx.notify();
    }

    fn nav(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = glass(cx);
        let trusted = self
            .certificates
            .read(cx)
            .trust
            .as_ref()
            .is_some_and(|t| t.trusted);
        let proxy_on = self.sys_proxy.read(cx).indicator() == ProxyIndicator::On;
        SettingsSection::ALL
            .iter()
            .map(|&section| {
                let weak = cx.entity().downgrade();
                let icon = match section {
                    SettingsSection::Network => Icon::new(AppIcon::Network),
                    SettingsSection::Certificates => Icon::new(AppIcon::ShieldCheck),
                    SettingsSection::SystemProxy => Icon::new(IconName::Globe),
                };
                let badge = match section {
                    SettingsSection::Certificates if !trusted => {
                        Some(("Not trusted", ToneName::Orange))
                    }
                    SettingsSection::SystemProxy if proxy_on => Some(("On", ToneName::Green)),
                    _ => None,
                };
                let item = NavItem::new(("settings-nav", section as usize), icon, section.label())
                    .selected(section == self.section)
                    .on_click(move |_, _, cx| {
                        let _ = weak.update(cx, |screen, cx| screen.show(section, cx));
                    });
                match badge {
                    Some((label, tone)) => item.badge(label, p.tone(tone)).into_any_element(),
                    None => item.into_any_element(),
                }
            })
            .collect()
    }
}

impl Render for SettingsScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = self.nav(cx);
        let content = match self.section {
            SettingsSection::Network => NetworkPanel::new(self.network.clone()).into_any_element(),
            SettingsSection::Certificates => div()
                .id("settings-certificates")
                .size_full()
                .overflow_y_scroll()
                .child(CertificatePanel::new(self.certificates.clone()))
                .into_any_element(),
            SettingsSection::SystemProxy => {
                SystemProxyPanel::new(self.sys_proxy.clone(), self.state.listen_addr.get())
                    .into_any_element()
            }
        };
        div()
            .size_full()
            .child(window_shell::shell(
                format!("Settings — {}", self.section.label()),
                nav,
                content,
                cx,
            ))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}
