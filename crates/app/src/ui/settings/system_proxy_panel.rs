//! Раздел System Proxy: что сейчас с системным прокси macOS относительно
//! нашего листенера, тумблер и подробности (эффективные и прописанные
//! серверы, исключения).

use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Entity, FontWeight, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window,
    div, px,
};

use crate::ui::group_box::GroupBox;
use crate::ui::system_proxy::{ProxyIndicator, SystemProxyStore};
use crate::ui::theme::components::dot;
use crate::ui::theme::metrics::{TEXT, TEXT_LG, TEXT_SM};
use crate::ui::theme::{Palette, ToneName, glass};

/// Заголовок, пояснение и тон состояния.
pub fn indicator_headline(indicator: &ProxyIndicator) -> (&'static str, String, ToneName) {
    match indicator {
        ProxyIndicator::On => (
            "macOS traffic goes through crabtrap",
            "Browsers and other apps that honor the system proxy send HTTP and HTTPS through crabtrap.".into(),
            ToneName::Green,
        ),
        ProxyIndicator::Off => (
            "System proxy is off",
            "Apps connect directly. Turn it on to capture traffic from browsers and other apps.".into(),
            ToneName::Gray,
        ),
        ProxyIndicator::Stale => (
            "Set for crabtrap, but not active",
            "crabtrap is written into a network service that is not in use right now (another \
             service or a VPN is active). Turn it on again to cover the current network."
                .into(),
            ToneName::Orange,
        ),
        ProxyIndicator::Other(server) => (
            "Another proxy is active",
            format!("Apps go through {server}. Turn on to route them through crabtrap instead."),
            ToneName::Orange,
        ),
    }
}

#[derive(IntoElement)]
pub struct SystemProxyPanel {
    store: Entity<SystemProxyStore>,
    listen_addr: String,
}

impl SystemProxyPanel {
    pub fn new(store: Entity<SystemProxyStore>, listen_addr: String) -> Self {
        Self { store, listen_addr }
    }
}

fn kv(label: &'static str, value: impl Into<SharedString>, p: &Palette) -> impl IntoElement {
    div()
        .flex()
        .items_start()
        .gap(px(12.))
        .child(
            div()
                .w(px(170.))
                .flex_none()
                .text_size(px(TEXT_SM))
                .text_color(p.text_secondary)
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(TEXT_SM))
                .child(value.into()),
        )
}

fn list_or_dash(items: &[String]) -> String {
    if items.is_empty() {
        "—".into()
    } else {
        items.join(", ")
    }
}

impl RenderOnce for SystemProxyPanel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let store = self.store.read(cx);
        let indicator = store.indicator();
        let (headline, hint, tone) = indicator_headline(&indicator);
        let status = store.status.clone();
        let (busy, error) = (store.busy, store.error.clone());
        let on = indicator == ProxyIndicator::On;

        let (toggle_store, refresh_store) = (self.store.clone(), self.store.clone());
        let actions = div()
            .flex()
            .gap(px(8.))
            .child(
                Button::new("settings-sys-proxy-toggle")
                    .label(if on {
                        "Turn Off"
                    } else {
                        "Route macOS Traffic Through crabtrap"
                    })
                    .map(|b| if on { b.danger() } else { b.primary() })
                    .disabled(busy)
                    .on_click(move |_, _, cx| SystemProxyStore::toggle(&toggle_store, cx)),
            )
            .child(
                Button::new("settings-sys-proxy-refresh")
                    .label("Refresh")
                    .disabled(busy)
                    .on_click(move |_, _, cx| SystemProxyStore::refresh(&refresh_store, cx)),
            );

        let details = GroupBox::new("Details", p.hairline, p.text_secondary)
            .child(kv("crabtrap listens on", self.listen_addr.clone(), &p))
            .child(kv(
                "Effective HTTP proxy",
                status
                    .as_ref()
                    .and_then(|s| s.server.clone())
                    .unwrap_or_else(|| "—".into()),
                &p,
            ))
            .child(kv(
                "Effective HTTPS proxy",
                status
                    .as_ref()
                    .and_then(|s| s.secure_server.clone())
                    .unwrap_or_else(|| "—".into()),
                &p,
            ))
            .child(kv(
                "Configured in services",
                status
                    .as_ref()
                    .map(|s| list_or_dash(&s.configured_servers))
                    .unwrap_or_else(|| "—".into()),
                &p,
            ))
            .child(kv(
                "Bypass",
                status
                    .as_ref()
                    .map(|s| list_or_dash(&s.bypass))
                    .unwrap_or_else(|| "—".into()),
                &p,
            ));

        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .p(px(20.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(dot(p.tone(tone).fg, 10.))
                    .child(
                        div()
                            .text_size(px(TEXT_LG))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(headline),
                    ),
            )
            .child(
                div()
                    .text_size(px(TEXT))
                    .text_color(p.text_secondary)
                    .child(hint),
            )
            .when_some(error, |this, err| {
                this.child(
                    div()
                        .text_size(px(TEXT_SM))
                        .text_color(p.tone(ToneName::Red).fg)
                        .child(format!("networksetup failed: {err}")),
                )
            })
            .child(actions)
            .child(details)
            .child(
                div()
                    .text_size(px(TEXT_SM))
                    .text_color(p.text_tertiary)
                    .child(
                        "crabtrap writes its address into every network service with networksetup \
                         and removes it when it quits.",
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headline_per_indicator() {
        assert_eq!(indicator_headline(&ProxyIndicator::On).2, ToneName::Green);
        assert_eq!(indicator_headline(&ProxyIndicator::Off).2, ToneName::Gray);
        assert_eq!(
            indicator_headline(&ProxyIndicator::Stale).2,
            ToneName::Orange
        );
        let (_, hint, tone) = indicator_headline(&ProxyIndicator::Other("10.0.0.1:3128".into()));
        assert_eq!(tone, ToneName::Orange);
        assert!(hint.contains("10.0.0.1:3128"));
    }

    #[test]
    fn empty_lists_render_as_dash() {
        assert_eq!(list_or_dash(&[]), "—");
        assert_eq!(list_or_dash(&["a".into(), "b".into()]), "a, b");
    }
}
