//! Раздел Network: доступ к прокси с других устройств (телефон, планшет)
//! и адрес, который на них прописать.

use gpui_kit::component::switch::Switch;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Entity, FontWeight, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window,
    div, px,
};

use crate::ui::group_box::GroupBox;
use crate::ui::network::{NetworkStore, lan_ip, listen_port};
use crate::ui::theme::metrics::{TEXT, TEXT_LG, TEXT_SM};
use crate::ui::theme::{Palette, ToneName, glass};

#[derive(IntoElement)]
pub struct NetworkPanel {
    store: Entity<NetworkStore>,
}

impl NetworkPanel {
    pub fn new(store: Entity<NetworkStore>) -> Self {
        Self { store }
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

impl RenderOnce for NetworkPanel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let store = self.store.read(cx);
        let (listen_addr, lan, error) =
            (store.listen_addr(), store.lan_access(), store.error.clone());
        let port = listen_port(&listen_addr).to_string();
        let ip = lan_ip();

        let toggle_store = self.store.clone();
        let toggle = Switch::new("settings-lan-access")
            .checked(lan)
            .label("Allow devices on the local network")
            .on_click(move |allow, _, cx| NetworkStore::set_lan_access(&toggle_store, *allow, cx));

        let hint = if lan {
            "Phones and other devices on the same Wi-Fi can use crabtrap as their proxy. \
             Anyone on this network can, so turn it off on public networks."
        } else {
            "Only apps on this Mac can use the proxy. Turn it on to capture traffic from a phone."
        };

        let device = GroupBox::new("Device setup", p.hairline, p.text_secondary)
            .child(kv("crabtrap listens on", listen_addr.clone(), &p))
            .when(lan, |this| match &ip {
                Some(ip) => this
                    .child(kv(
                        "Wi-Fi proxy on the device",
                        format!("{ip} port {port} (Manual)"),
                        &p,
                    ))
                    .child(kv(
                        "Certificate page",
                        format!("http://{ip}:{port}/ssl"),
                        &p,
                    ))
                    .child(kv("…or, with the proxy set", "http://crabtrap.ssl", &p)),
                None => this.child(kv(
                    "Wi-Fi proxy on the device",
                    "No local network found",
                    &p,
                )),
            });

        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .p(px(20.))
            .child(
                div()
                    .text_size(px(TEXT_LG))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Access from other devices"),
            )
            .child(toggle)
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
                        .child(err),
                )
            })
            .child(device)
    }
}
