//! Нижняя панель окна: состояние прокси слева, статистика захвата справа
//! (старт/стоп — в плашке состояния тулбара и в меню Proxy).
//!
//! `EngineStatusStore` — GPUI-Entity, куда pump (runtime.rs) применяет
//! `ProxyEvent::EngineStatus`.

use application::dto::capture::ProxyEvent;
use chrono::{DateTime, Utc};
use domain::entities::rule::RuleSet;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, Entity, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px};

use crate::ui::system_proxy::{ProxyIndicator, SystemProxyStore};
use crate::ui::theme::components::dot;
use crate::ui::theme::metrics::{STATUS_BAR_HEIGHT, TEXT_XS};
use crate::ui::theme::semantic::ExchangeState;
use crate::ui::theme::{ToneName, glass};
use crate::ui::traffic::store::TrafficStore;

/// Состояние движка для UI (обновляется событиями EngineStatus).
#[derive(Debug, Clone, Default)]
pub struct EngineStatusStore {
    pub listening: Option<String>,
    pub active_conns: usize,
    pub error: Option<String>,
    pub last_update: Option<DateTime<Utc>>,
}

impl EngineStatusStore {
    pub fn apply_batch(&mut self, events: &[ProxyEvent]) {
        // Берём последнее EngineStatus из пачки (пачка сохраняет порядок).
        for event in events {
            if let ProxyEvent::EngineStatus {
                listening,
                active_conns,
                error,
            } = event
            {
                self.listening = listening.clone();
                self.active_conns = *active_conns;
                self.error = error.clone();
                self.last_update = Some(Utc::now());
            }
        }
    }

    /// Текст статуса для левой части бара.
    pub fn status_text(&self) -> String {
        if let Some(error) = &self.error {
            format!("Proxy: error: {error}")
        } else if let Some(addr) = &self.listening {
            format!("Proxy: running {addr}")
        } else {
            "Proxy: stopped".to_string()
        }
    }
}

/// Нижняя панель окна.
#[derive(gpui_kit::IntoElement)]
pub struct StatusBar {
    status: Entity<EngineStatusStore>,
    traffic: Entity<TrafficStore>,
    system_proxy: Entity<SystemProxyStore>,
}

impl StatusBar {
    pub fn new(
        status: Entity<EngineStatusStore>,
        traffic: Entity<TrafficStore>,
        system_proxy: Entity<SystemProxyStore>,
    ) -> Self {
        Self {
            status,
            traffic,
            system_proxy,
        }
    }
}

/// Человекочитаемый объём (Б/КБ/МБ/ГБ).
pub fn human_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else if n < 1024 * 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Подпись обменов: «N exchanges» или «V of N exchanges» (фильтр сузил
/// вид); `seen` больше `kept` — старые вытеснены из буфера, тогда
/// отдельно «M seen».
pub fn exchanges_label(visible: usize, kept: usize, seen: u64) -> (String, Option<String>) {
    let main = if visible < kept {
        format!("{visible} of {}", plural(kept, "exchange", "exchanges"))
    } else {
        plural(kept, "exchange", "exchanges")
    };
    let dropped = (seen > kept as u64).then(|| format!("{seen} seen"));
    (main, dropped)
}

/// Системный прокси указывает на crabtrap, а листенер остановлен —
/// у приложений пропал интернет. Текст предупреждения или None.
pub fn proxy_warning(indicator: &ProxyIndicator, listening: bool) -> Option<&'static str> {
    (indicator.ours_anywhere() && !listening)
        .then_some("macOS proxy points to crabtrap, but the proxy is stopped")
}

/// Allow List действует — записывается не весь трафик: подпись или None.
pub fn allow_list_note(rules: &RuleSet) -> Option<String> {
    rules.allow_list_active().then(|| {
        let hosts = rules.allow_list.iter().filter(|e| e.enabled).count();
        format!(
            "Allow List: recording {} only",
            plural(hosts, "host", "hosts")
        )
    })
}

/// «1 connection» / «3 connections».
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl RenderOnce for StatusBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let store = self.status.read(cx);
        let tone = if store.error.is_some() {
            ToneName::Red
        } else if store.listening.is_some() {
            ToneName::Green
        } else {
            ToneName::Gray
        };
        let warning = proxy_warning(
            &self.system_proxy.read(cx).indicator(),
            store.listening.is_some(),
        );
        let allow_note = crate::ui::rules::window::store(cx)
            .and_then(|rules| allow_list_note(&rules.read(cx).rules));
        let traffic = self.traffic.read(cx);
        let stats = traffic.stats;
        let (exchanges, seen) =
            exchanges_label(traffic.visible_len(), traffic.rows.len(), stats.exchanges);
        let separator = || div().w(px(1.)).h(px(10.)).bg(p.hairline);
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(STATUS_BAR_HEIGHT))
            .px(px(12.))
            .border_t_1()
            .border_color(p.hairline)
            .text_size(px(TEXT_XS))
            .text_color(p.text_secondary)
            .child(dot(p.tone(tone).fg, 6.))
            .child(store.status_text())
            .when_some(warning, |this, warning| {
                this.child(separator()).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .text_color(p.tone(ToneName::Orange).fg)
                        .child(dot(p.tone(ToneName::Orange).fg, 6.))
                        .child(warning),
                )
            })
            .when_some(allow_note, |this, note| {
                this.child(separator()).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .text_color(p.tone(ToneName::Orange).fg)
                        .child(dot(p.tone(ToneName::Orange).fg, 6.))
                        .child(note),
                )
            })
            .child(div().flex_1())
            .child(plural(store.active_conns, "connection", "connections"))
            .child(separator())
            .child(exchanges)
            .when_some(seen, |this, seen| {
                this.child(div().text_color(p.text_tertiary).child(seen))
            })
            .child(separator())
            .child(format!("{} captured", human_bytes(stats.bytes)))
            .when(stats.failed > 0, |this| {
                this.child(separator()).child(
                    div()
                        .text_color(p.tone(ExchangeState::Failed.tone()).fg)
                        .child(format!("{} failed", stats.failed)),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_list_note_only_while_the_list_filters() {
        use domain::entities::rule::AllowEntry;
        assert_eq!(allow_list_note(&RuleSet::default()), None);
        let rules = RuleSet {
            allow_enabled: true,
            allow_list: vec![AllowEntry::new("a.test"), AllowEntry::new("b.test")],
            ..RuleSet::default()
        };
        assert_eq!(
            allow_list_note(&rules).as_deref(),
            Some("Allow List: recording 2 hosts only")
        );
        let off = RuleSet {
            allow_enabled: false,
            ..rules
        };
        assert_eq!(allow_list_note(&off), None);
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn exchanges_label_distinguishes_filter_from_eviction() {
        assert_eq!(exchanges_label(10, 10, 10), ("10 exchanges".into(), None));
        assert_eq!(
            exchanges_label(3, 10, 10),
            ("3 of 10 exchanges".into(), None)
        );
        assert_eq!(
            exchanges_label(1000, 1000, 10_000),
            ("1000 exchanges".into(), Some("10000 seen".into()))
        );
    }

    #[test]
    fn warns_when_system_proxy_points_at_stopped_crabtrap() {
        assert!(proxy_warning(&ProxyIndicator::On, false).is_some());
        assert!(proxy_warning(&ProxyIndicator::Stale, false).is_some());
    }

    #[test]
    fn no_warning_while_running_or_proxy_not_ours() {
        assert_eq!(proxy_warning(&ProxyIndicator::On, true), None);
        assert_eq!(proxy_warning(&ProxyIndicator::Off, false), None);
        assert_eq!(
            proxy_warning(&ProxyIndicator::Other("10.0.0.1:3128".into()), false),
            None
        );
    }

    #[test]
    fn plural_picks_form() {
        assert_eq!(plural(1, "connection", "connections"), "1 connection");
        assert_eq!(plural(0, "connection", "connections"), "0 connections");
    }
}
