//! Контекстное меню строки трафика (правый клик), как в Charles: правило
//! для этого запроса без ручного переписывания хоста и пути.
//!
//! Map Local… / Map Remote… / Block… открывают окно Tools с диалогом, где
//! уже подставлены хост, путь и метод; Script… — Script Editor с ними же; «Enable SSL Proxying for <host>»
//! добавляет хост сразу; «Pin/Unpin <host>» — закрепить домен в боковой
//! панели; «Copy URL» — в буфер обмена.

use application::dto::capture::ExchangeSummaryDto;
use application::use_cases::rules::edit::with_ssl_host_enabled;
use application::use_cases::rules::location_form::LocationForm;
use gpui_kit::ClipboardItem;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};

use crate::ui::compose;
use crate::ui::rules::RulesTab;
use crate::ui::rules::store::RulesStore;
use crate::ui::rules::window as rules_window;
use crate::ui::scripting::window as scripting_window;
use crate::ui::sidebar::SidebarStore;

/// Полный URL обмена.
pub fn exchange_url(summary: &ExchangeSummaryDto) -> String {
    format!("{}://{}{}", summary.scheme, summary.host, summary.path)
}

/// Хост без порта (строка туннеля — `host:8443`).
pub fn bare_host(summary: &ExchangeSummaryDto) -> String {
    match summary.host.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() && !host.contains(':') => {
            host.to_string()
        }
        _ => summary.host.clone(),
    }
}

/// Форма правила с хостом (без порта), путём и методом обмена.
pub fn rule_prefill(summary: &ExchangeSummaryDto) -> LocationForm {
    LocationForm {
        host: bare_host(summary),
        ..LocationForm::from_exchange(summary)
    }
}

/// Пункты меню для обмена (`pinned` — домен уже закреплён).
pub fn exchange_menu(menu: PopupMenu, summary: &ExchangeSummaryDto, pinned: bool) -> PopupMenu {
    let prefill = rule_prefill(summary);
    let open_tool = |label: &'static str, tab: RulesTab| {
        let prefill = prefill.clone();
        PopupMenuItem::new(label)
            .on_click(move |_, _, cx| rules_window::open(tab, Some(prefill.clone()), cx))
    };
    let host = bare_host(summary);
    let url = exchange_url(summary);
    let id = summary.id;
    menu.item(PopupMenuItem::new("Repeat").on_click(move |_, _, cx| compose::repeat(id, cx)))
        .item(
            PopupMenuItem::new("Edit & Resend…")
                .on_click(move |_, _, cx| compose::open_edit(id, cx)),
        )
        .separator()
        .item(open_tool("Map Local…", RulesTab::MapLocal))
        .item(open_tool("Map Remote…", RulesTab::MapRemote))
        .item(open_tool("Block…", RulesTab::BlockList))
        .item(open_tool("Rewrite…", RulesTab::Rewrite))
        .item(open_tool("Breakpoint…", RulesTab::Breakpoints))
        .item({
            let prefill = prefill.clone();
            PopupMenuItem::new("Script…")
                .on_click(move |_, _, cx| scripting_window::open_new(prefill.clone(), cx))
        })
        .separator()
        .item(
            PopupMenuItem::new(format!("Enable SSL Proxying for {host}")).on_click(
                move |_, _, cx| {
                    if let Some(store) = rules_window::store(cx) {
                        let host = host.clone();
                        RulesStore::apply(&store, cx, move |rules| {
                            with_ssl_host_enabled(rules, &host)
                        });
                    }
                },
            ),
        )
        .item(
            PopupMenuItem::new(format!(
                "{} {}",
                if pinned { "Unpin" } else { "Pin" },
                summary.host
            ))
            .on_click({
                let host = summary.host.clone();
                move |_, _, cx| {
                    if let Some(sidebar) = SidebarStore::global(cx) {
                        SidebarStore::toggle_pin(&sidebar, &host, cx);
                    }
                }
            }),
        )
        .separator()
        .item(PopupMenuItem::new("Copy URL").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use domain::value_objects::http_vo::Protocol;
    use domain::value_objects::{ConnectionId, ExchangeId};

    fn summary(host: &str, path: &str) -> ExchangeSummaryDto {
        ExchangeSummaryDto {
            id: ExchangeId::new(),
            connection_id: ConnectionId::new(),
            started_at: Utc::now(),
            method: "GET".into(),
            scheme: "https".into(),
            host: host.into(),
            path: path.into(),
            status: Some(200),
            req_size: 0,
            resp_size: 0,
            duration_ms: 0.0,
            protocol: Protocol::Http1,
            tls_mitm: Some(true),
            tags: Vec::new(),
        }
    }

    #[test]
    fn url_and_bare_host() {
        let s = summary("api.com", "/v1?x=1");
        assert_eq!(exchange_url(&s), "https://api.com/v1?x=1");
        assert_eq!(bare_host(&s), "api.com");
        assert_eq!(bare_host(&summary("tunnel.com:8443", "")), "tunnel.com");
    }
}
