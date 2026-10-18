//! Тулбар в тайтл-баре окна (unified toolbar): запись · пауза · очистка ·
//! плашка состояния (прокси / SSL Proxying / macOS Proxy) · Tools · поиск.
//!
//! Действия — те же, что у строки меню и иконки в трее
//! ([`crate::ui::traffic::actions`], сторы прокси); подписи и цвета плашки —
//! чистые функции [`proxy_segment`] / [`system_proxy_segment`].

use application::dto::proxy_config::EngineCommand;
use config::InspectorLayout;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Icon, IconName, Sizable as _};
use gpui_kit::{
    App, Entity, InteractiveElement as _, IntoElement, MouseButton, ParentElement, RenderOnce,
    Styled, Window, div, px,
};

use crate::app_state::AppState;
use crate::ui::detail::DetailStore;
use crate::ui::rules::RulesTab;
use crate::ui::rules::window as rules_window;
use crate::ui::sidebar::SidebarStore;
use crate::ui::ssl_proxying::SslProxyingStore;
use crate::ui::status_bar::EngineStatusStore;
use crate::ui::system_proxy::{ProxyIndicator, SystemProxyStore};
use crate::ui::theme::components::{StatusPill, StatusSegment, ToolbarButton};
use crate::ui::theme::icons::AppIcon;
use crate::ui::theme::{Palette, ToneName, glass};
use crate::ui::traffic::actions;
use crate::ui::traffic::search::SearchStore;
use crate::ui::traffic::store::TrafficStore;

/// Подпись и тон сегмента «прокси» (он же точка плашки).
pub fn proxy_segment(status: &EngineStatusStore) -> (String, ToneName, &'static str) {
    if let Some(error) = &status.error {
        (
            format!("Proxy error: {error}"),
            ToneName::Red,
            "Click to restart the proxy",
        )
    } else if let Some(addr) = &status.listening {
        (
            format!("Proxy {addr}"),
            ToneName::Green,
            "Click to stop the proxy",
        )
    } else {
        (
            "Proxy stopped".to_string(),
            ToneName::Gray,
            "Click to start the proxy",
        )
    }
}

/// Подпись, цвет (если особый) и подсказка сегмента «macOS Proxy».
pub fn system_proxy_segment(
    indicator: &ProxyIndicator,
    busy: bool,
    error: Option<&str>,
) -> (String, Option<ToneName>, String) {
    if let Some(error) = error {
        return (
            "macOS Proxy ⚠".into(),
            Some(ToneName::Red),
            format!("networksetup failed: {error}"),
        );
    }
    if busy {
        return (
            "macOS Proxy…".into(),
            None,
            "Applying system proxy settings…".into(),
        );
    }
    match indicator {
        ProxyIndicator::On => (
            "macOS Proxy".into(),
            None,
            "Apps go through crabtrap — click to turn off".into(),
        ),
        ProxyIndicator::Off => (
            "macOS Proxy".into(),
            None,
            "Click to route macOS traffic through crabtrap".into(),
        ),
        ProxyIndicator::Stale => (
            "macOS Proxy ⚠".into(),
            Some(ToneName::Orange),
            "Set for crabtrap, but not active on the current network".into(),
        ),
        ProxyIndicator::Other(server) => (
            "macOS Proxy: other".into(),
            Some(ToneName::Orange),
            format!("Apps go through another proxy ({server})"),
        ),
    }
}

/// Контрол в тайтл-баре: mouse-down не доходит до TitleBar (иначе он
/// начинает перенос окна от малейшего сдвига мыши во время клика).
pub fn no_drag(child: impl IntoElement) -> impl IntoElement {
    div()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(child)
}

/// Тулбар главного окна (содержимое `TitleBar`).
#[derive(IntoElement)]
pub struct MainToolbar {
    pub sidebar: Entity<SidebarStore>,
    pub detail: Entity<DetailStore>,
    pub state: AppState,
    pub runtime: tokio::runtime::Handle,
    pub status: Entity<EngineStatusStore>,
    pub traffic: Entity<TrafficStore>,
    pub ssl_proxying: Entity<SslProxyingStore>,
    pub sys_proxy: Entity<SystemProxyStore>,
    pub search: Entity<SearchStore>,
    pub search_input: Entity<InputState>,
}

impl MainToolbar {
    fn sidebar_toggle(&self, cx: &App) -> impl IntoElement {
        let sidebar = self.sidebar.clone();
        let visible = self.sidebar.read(cx).visible;
        ToolbarButton::new("toolbar-sidebar")
            .icon(Icon::new(IconName::PanelLeft))
            .tooltip(if visible {
                "Hide sidebar (⌃⌘S)"
            } else {
                "Show sidebar (⌃⌘S)"
            })
            .on_click(move |_, _, cx| SidebarStore::toggle_visible(&sidebar, cx))
    }

    fn capture_controls(&self, p: &Palette, cx: &App) -> impl IntoElement {
        let traffic = self.traffic.read(cx);
        let (recording, paused) = (traffic.recording, traffic.paused);
        let (state, store) = (self.state.clone(), self.traffic.clone());
        let record = ToolbarButton::new("toolbar-record")
            .icon(Icon::new(AppIcon::CircleDot))
            .icon_color(if recording {
                p.tone(ToneName::Red).fg
            } else {
                p.text_tertiary
            })
            .tooltip(if recording {
                "Recording bodies (⌘R)"
            } else {
                "Not recording bodies (⌘R)"
            })
            .on_click(move |_, _, cx| actions::toggle_recording(&state, &store, cx));
        let store = self.traffic.clone();
        let pause = ToolbarButton::new("toolbar-pause")
            .icon(Icon::new(if paused {
                IconName::Play
            } else {
                IconName::Pause
            }))
            .active(paused)
            .tooltip(if paused {
                "Resume table updates"
            } else {
                "Pause table updates"
            })
            .on_click(move |_, _, cx| actions::toggle_pause(&store, cx));
        let (state, store, runtime) = (
            self.state.clone(),
            self.traffic.clone(),
            self.runtime.clone(),
        );
        let clear = ToolbarButton::new("toolbar-clear")
            .icon(Icon::new(AppIcon::Trash))
            .tooltip("Clear traffic (⌘K)")
            .on_click(move |_, _, cx| actions::clear_traffic(&state, &store, &runtime, cx));
        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .child(record)
            .child(pause)
            .child(clear)
    }

    fn status_pill(&self, p: &Palette, cx: &App) -> impl IntoElement {
        let status = self.status.read(cx);
        let running = status.listening.is_some();
        let (label, tone, hint) = proxy_segment(status);
        let engine = self.state.engine_command.clone();
        let state = self.state.clone();
        let proxy = StatusSegment::text(label)
            .tooltip(hint)
            .on_click(move |_, _, _| {
                if running {
                    engine.send(EngineCommand::Stop);
                } else {
                    engine.send(EngineCommand::Start {
                        settings: state.proxy_settings(),
                    });
                }
            });

        let ssl = self.ssl_proxying.read(cx);
        let ssl_store = self.ssl_proxying.clone();
        let ssl_segment = StatusSegment::toggle(
            if ssl.busy {
                "SSL Proxying…"
            } else {
                "SSL Proxying"
            },
            ssl.enabled,
        )
        .tooltip(if ssl.enabled {
            "HTTPS is decrypted — click to tunnel it instead"
        } else {
            "HTTPS is tunneled — click to decrypt it"
        })
        .on_click(move |_, _, cx| SslProxyingStore::toggle(&ssl_store, cx));

        let sys = self.sys_proxy.read(cx);
        let indicator = sys.indicator();
        let (label, color, hint) = system_proxy_segment(&indicator, sys.busy, sys.error.as_deref());
        let sys_store = self.sys_proxy.clone();
        let mut sys_segment = StatusSegment::toggle(label, indicator == ProxyIndicator::On)
            .tooltip(hint)
            .on_click(move |_, _, cx| SystemProxyStore::toggle(&sys_store, cx));
        if let Some(color) = color {
            sys_segment = sys_segment.color(p.tone(color).fg);
        }

        StatusPill::new("toolbar-status", p.tone(tone).fg)
            .segment(proxy)
            .segment(ssl_segment)
            .segment(sys_segment)
    }

    fn layout_toggle(&self, cx: &App) -> impl IntoElement {
        let detail = self.detail.clone();
        let right = self.detail.read(cx).layout == InspectorLayout::Right;
        ToolbarButton::new("toolbar-layout")
            .icon(Icon::new(if right {
                IconName::PanelRight
            } else {
                IconName::PanelBottom
            }))
            .tooltip(if right {
                "Move inspector below the table"
            } else {
                "Move inspector to the right"
            })
            .on_click(move |_, _, cx| {
                let next = if right {
                    InspectorLayout::Bottom
                } else {
                    InspectorLayout::Right
                };
                DetailStore::set_layout(&detail, next, cx);
            })
    }

    fn tools_menu(&self) -> impl IntoElement {
        Button::new("toolbar-tools")
            .icon(Icon::new(AppIcon::Wrench))
            .label("Tools")
            .ghost()
            .small()
            .dropdown_menu(|menu, _, _| {
                RulesTab::ALL.iter().fold(menu, |menu, &tab| {
                    menu.item(
                        PopupMenuItem::new(format!("{}…", tab.label()))
                            .on_click(move |_, _, cx| rules_window::open(tab, None, cx)),
                    )
                })
            })
    }
}

impl RenderOnce for MainToolbar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let searching = self.search.read(cx).loading;
        let search = Input::new(&self.search_input)
            .h(px(28.))
            .w(px(220.))
            .prefix(
                Icon::new(IconName::Search)
                    .small()
                    .text_color(p.text_tertiary),
            )
            .cleanable(true)
            .disabled(searching);
        div()
            .flex()
            .flex_row()
            .items_center()
            .size_full()
            .gap(px(8.))
            .pr(px(10.))
            .child(self.sidebar_toggle(cx))
            .child(self.capture_controls(&p, cx))
            .child(div().flex_1())
            .child(self.status_pill(&p, cx))
            .child(div().flex_1())
            .child(self.layout_toggle(cx))
            .child(no_drag(self.tools_menu()))
            .child(no_drag(search))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_segment_follows_engine_state() {
        let running = EngineStatusStore {
            listening: Some("127.0.0.1:8080".into()),
            ..Default::default()
        };
        assert_eq!(proxy_segment(&running).0, "Proxy 127.0.0.1:8080");
        assert_eq!(proxy_segment(&running).1, ToneName::Green);
        let stopped = EngineStatusStore::default();
        assert_eq!(proxy_segment(&stopped).1, ToneName::Gray);
        let failed = EngineStatusStore {
            error: Some("address in use".into()),
            ..Default::default()
        };
        let (label, tone, _) = proxy_segment(&failed);
        assert!(label.contains("address in use"));
        assert_eq!(tone, ToneName::Red);
    }

    #[test]
    fn system_proxy_segment_explains_odd_states() {
        let (_, color, _) = system_proxy_segment(&ProxyIndicator::On, false, None);
        assert_eq!(color, None);
        let (label, color, _) = system_proxy_segment(&ProxyIndicator::Stale, false, None);
        assert!(label.contains('⚠'));
        assert_eq!(color, Some(ToneName::Orange));
        let (_, _, hint) =
            system_proxy_segment(&ProxyIndicator::Other("10.0.0.1:3128".into()), false, None);
        assert!(hint.contains("10.0.0.1:3128"));
        let (label, _, _) = system_proxy_segment(&ProxyIndicator::Off, true, None);
        assert!(label.ends_with('…'));
        let (_, color, hint) = system_proxy_segment(&ProxyIndicator::Off, false, Some("denied"));
        assert_eq!(color, Some(ToneName::Red));
        assert!(hint.contains("denied"));
    }
}
