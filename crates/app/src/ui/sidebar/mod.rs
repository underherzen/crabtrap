//! Боковая панель трафика (как у Proxyman): Library (All, Pinned) →
//! закреплённые домены → остальные домены; домен раскрывается в разделы
//! (первый сегмент пути). Клик выбирает, что показывает таблица
//! ([`Scope`]); правый клик по домену — Pin/Unpin, Copy Host.
//!
//! - [`SidebarStore`] — видимость и раскрытые домены; закреплённые домены
//!   живут в `TrafficStore.pinned` (по ним же фильтрует таблица) и
//!   сохраняются в config.toml ([`crate::ui::prefs`]).
//! - [`items`] — плоский список строк (чистая функция, тесты).

pub mod items;

use std::collections::{BTreeSet, HashSet};
use std::rc::Rc;

use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::{Icon, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClipboardItem, Entity, FontWeight, Global, InteractiveElement as _,
    IntoElement, ParentElement, RenderOnce, StatefulInteractiveElement as _, Styled, Window, div,
    px, uniform_list,
};

use crate::ui::prefs;
use crate::ui::theme::components::glass_sidebar;
use crate::ui::theme::icons::AppIcon;
use crate::ui::theme::metrics::{RADIUS, ROW_HEIGHT, TEXT, TEXT_XS};
use crate::ui::theme::{Palette, glass};
use crate::ui::traffic::apps::ClientApp;
use crate::ui::traffic::scope::Scope;
use crate::ui::traffic::store::TrafficStore;
use crate::ui::traffic::table::client_icon;
use items::{SidebarItem, sidebar_items};

/// Состояние боковой панели.
pub struct SidebarStore {
    pub visible: bool,
    expanded: HashSet<String>,
    traffic: Entity<TrafficStore>,
}

/// Хендл панели для действий вне неё (меню, правый клик в таблице).
struct SidebarHandle(Entity<SidebarStore>);

impl Global for SidebarHandle {}

impl SidebarStore {
    pub fn new(traffic: Entity<TrafficStore>, visible: bool) -> Self {
        Self {
            visible,
            expanded: HashSet::new(),
            traffic,
        }
    }

    /// Зарегистрировать панель глобально (одна на приложение).
    pub fn install(entity: &Entity<Self>, cx: &mut App) {
        cx.set_global(SidebarHandle(entity.clone()));
    }

    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<SidebarHandle>().map(|h| h.0.clone())
    }

    pub fn toggle_visible(entity: &Entity<Self>, cx: &mut App) {
        let visible = entity.update(cx, |this, cx| {
            this.visible = !this.visible;
            tracing::debug!(visible = this.visible, "sidebar toggled");
            cx.notify();
            this.visible
        });
        prefs::save_ui(move |ui| config::UiConfig {
            sidebar_visible: visible,
            ..ui
        });
    }

    fn toggle_expanded(entity: &Entity<Self>, host: &str, cx: &mut App) {
        entity.update(cx, |this, cx| {
            if !this.expanded.remove(host) {
                this.expanded.insert(host.to_string());
            }
            cx.notify();
        });
    }

    /// Закрепить/открепить домен (таблица и config.toml).
    pub fn toggle_pin(entity: &Entity<Self>, host: &str, cx: &mut App) {
        let traffic = entity.read(cx).traffic.clone();
        let pinned: BTreeSet<String> = {
            let current = &traffic.read(cx).pinned;
            if current.contains(host) {
                current.iter().filter(|h| *h != host).cloned().collect()
            } else {
                current.iter().cloned().chain([host.to_string()]).collect()
            }
        };
        let saved: Vec<String> = pinned.iter().cloned().collect();
        traffic.update(cx, |store, cx| {
            store.set_pinned(pinned);
            cx.notify();
        });
        entity.update(cx, |_, cx| cx.notify());
        prefs::save_ui(move |ui| config::UiConfig {
            pinned_hosts: saved,
            ..ui
        });
    }

    fn select(entity: &Entity<Self>, scope: Scope, cx: &mut App) {
        let traffic = entity.read(cx).traffic.clone();
        traffic.update(cx, |store, cx| {
            store.set_scope(scope);
            cx.notify();
        });
    }
}

/// Панель (RenderOnce над SidebarStore + TrafficStore).
#[derive(IntoElement)]
pub struct Sidebar {
    store: Entity<SidebarStore>,
}

impl Sidebar {
    pub fn new(store: Entity<SidebarStore>) -> Self {
        Self { store }
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let store = self.store.read(cx);
        let traffic = store.traffic.read(cx);
        let items = Rc::new(sidebar_items(
            traffic.apps(),
            traffic.domains(),
            traffic.rows.len(),
            &traffic.pinned,
            &store.expanded,
        ));
        let scope = Rc::new(traffic.scope.clone());
        let entity = self.store.clone();
        let count = items.len();
        glass_sidebar(&p).size_full().pt(px(4.)).child(
            uniform_list("sidebar-items", count, move |range, _, cx| {
                let p = glass(cx);
                range
                    .filter_map(|ix| items.get(ix).map(|item| (ix, item)))
                    .map(|(ix, item)| render_item(ix, item, &scope, &entity, &p, cx))
                    .collect::<Vec<_>>()
            })
            .size_full(),
        )
    }
}

fn nav_icon(icon: impl Into<Icon>, selected: bool, p: &Palette) -> AnyElement {
    let icon: Icon = icon.into();
    icon.small()
        .text_color(if selected {
            p.accent_text
        } else {
            p.text_secondary
        })
        .into_any_element()
}

fn render_item(
    ix: usize,
    item: &SidebarItem,
    scope: &Scope,
    store: &Entity<SidebarStore>,
    p: &Palette,
    cx: &App,
) -> AnyElement {
    if let SidebarItem::Section(title) = item {
        return div()
            .h(px(ROW_HEIGHT))
            .flex()
            .items_end()
            .px(px(14.))
            .pb(px(4.))
            .text_size(px(TEXT_XS))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(p.text_tertiary)
            .child(*title)
            .into_any_element();
    }
    if let SidebarItem::Hint(text) = item {
        return div()
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .px(px(14.))
            .text_size(px(TEXT_XS))
            .text_color(p.text_tertiary)
            .child(*text)
            .into_any_element();
    }
    let item_scope = item.scope();
    let selected = item_scope.as_ref() == Some(scope);
    let (indent, icon, label, count): (f32, AnyElement, String, usize) = match item {
        SidebarItem::All { count } => (
            0.,
            nav_icon(AppIcon::Layers, selected, p),
            "All".to_string(),
            *count,
        ),
        SidebarItem::Pinned { count } => (
            0.,
            nav_icon(AppIcon::Pin, selected, p),
            "Pinned".to_string(),
            *count,
        ),
        SidebarItem::Host {
            host,
            count,
            pinned,
            ..
        } => (
            0.,
            nav_icon(
                if *pinned {
                    AppIcon::Pin
                } else {
                    AppIcon::Globe
                },
                selected,
                p,
            ),
            host.clone(),
            *count,
        ),
        SidebarItem::Path { prefix, count, .. } => (
            18.,
            nav_icon(IconName::Folder, selected, p),
            prefix.clone(),
            *count,
        ),
        SidebarItem::App {
            name,
            count,
            icon_key,
            remote,
        } => {
            let client = ClientApp {
                name: name.clone(),
                icon_key: icon_key.clone(),
                remote: *remote,
            };
            (
                0.,
                client_icon(&client, p.text_secondary, cx),
                name.clone(),
                *count,
            )
        }
        SidebarItem::Section(_) | SidebarItem::Hint(_) => return div().into_any_element(),
    };
    let chevron = match item {
        SidebarItem::Host {
            host,
            expanded,
            has_paths: true,
            ..
        } => {
            let (store, host) = (store.clone(), host.clone());
            Some(
                div()
                    .id(("sidebar-chevron", ix))
                    .flex_none()
                    .w(px(14.))
                    .child(
                        Icon::new(if *expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(p.text_tertiary),
                    )
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        SidebarStore::toggle_expanded(&store, &host, cx);
                    }),
            )
        }
        _ => None,
    };
    let (fg, bg) = if selected {
        (p.text, p.selection_soft)
    } else {
        (p.text, gpui_kit::transparent_black())
    };
    let store_for_click = store.clone();
    let row = div()
        .id(("sidebar-item", ix))
        .h(px(ROW_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(8. + indent))
        .pr(px(8.))
        .rounded(px(RADIUS))
        .bg(bg)
        .text_size(px(TEXT))
        .text_color(fg)
        .when(!selected, |this| this.hover(|style| style.bg(p.row_hover)))
        .child(
            div()
                .flex_none()
                .w(px(14.))
                .when_some(chevron, |this, c| this.child(c)),
        )
        .child(icon)
        .child({
            // Длинный домен не влезает в панель — целиком в подсказке.
            let full: gpui_kit::SharedString = label.clone().into();
            div()
                .id(("sidebar-label", ix))
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx)
                })
                .child(label)
        })
        .child(
            div()
                .flex_none()
                .text_size(px(TEXT_XS))
                .text_color(p.text_tertiary)
                .child(count.to_string()),
        )
        .when_some(item_scope, |this, scope| {
            this.on_click(move |_, _, cx| SidebarStore::select(&store_for_click, scope.clone(), cx))
        });
    // Обёртка с полями: строка уходит от краёв панели, но тянется по ширине.
    let wrap = |row: AnyElement| div().w_full().px(px(6.)).child(row).into_any_element();
    match item {
        SidebarItem::Host { host, pinned, .. } => {
            let (store, host, pinned) = (store.clone(), host.clone(), *pinned);
            wrap(
                row.context_menu(move |menu, _, _| {
                    let (store, pin_host, copy_host) = (store.clone(), host.clone(), host.clone());
                    menu.item(
                        PopupMenuItem::new(if pinned { "Unpin" } else { "Pin" }).on_click(
                            move |_, _, cx| SidebarStore::toggle_pin(&store, &pin_host, cx),
                        ),
                    )
                    .separator()
                    .item(PopupMenuItem::new("Copy Host").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_host.clone()))
                    }))
                })
                .into_any_element(),
            )
        }
        _ => wrap(row.into_any_element()),
    }
}
