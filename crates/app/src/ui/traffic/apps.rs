//! Клиенты обменов (Track G8): какое приложение открыло соединение, и
//! список приложений для боковой панели со счётчиками (инкрементально, см.
//! [`AppCounts`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::store::TrafficRow;

/// Клиент соединения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientApp {
    /// Имя приложения, процесса или адрес устройства в сети.
    pub name: String,
    /// Ключ иконки (путь `.app`-бандла); `None` — иконки нет.
    pub icon_key: Option<String>,
    /// Устройство в сети, а не приложение на этом Mac.
    pub remote: bool,
}

impl ClientApp {
    /// Устройство в сети (телефон, другой компьютер) — по IP.
    pub fn device(ip: &str) -> Self {
        Self {
            name: ip.to_string(),
            icon_key: None,
            remote: true,
        }
    }
}

/// Приложение в боковой панели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppNode {
    pub name: String,
    pub count: usize,
    pub icon_key: Option<String>,
    pub remote: bool,
}

/// Счётчики приложений-клиентов + снимок для панели (как DomainCounts).
#[derive(Debug, Default)]
pub struct AppCounts {
    apps: HashMap<String, (usize, Arc<ClientApp>)>,
    snapshot: Vec<AppNode>,
    dirty: bool,
}

impl AppCounts {
    pub fn add(&mut self, row: &TrafficRow) {
        if let Some(client) = &row.client {
            self.apps
                .entry(client.name.clone())
                .or_insert_with(|| (0, client.clone()))
                .0 += 1;
            self.dirty = true;
        }
    }

    pub fn remove(&mut self, row: &TrafficRow) {
        let Some(client) = &row.client else {
            return;
        };
        if let Some(entry) = self.apps.get_mut(&client.name) {
            entry.0 = entry.0.saturating_sub(1);
            if entry.0 == 0 {
                self.apps.remove(&client.name);
            }
            self.dirty = true;
        }
    }

    pub fn clear(&mut self) {
        self.apps.clear();
        self.snapshot.clear();
        self.dirty = false;
    }

    /// Пересобрать снимок, если счётчики менялись (по алфавиту).
    pub fn refresh(&mut self) {
        if !self.dirty {
            return;
        }
        let sorted: BTreeMap<&String, &(usize, Arc<ClientApp>)> = self.apps.iter().collect();
        self.snapshot = sorted
            .into_values()
            .map(|(count, client)| AppNode {
                name: client.name.clone(),
                count: *count,
                icon_key: client.icon_key.clone(),
                remote: client.remote,
            })
            .collect();
        self.dirty = false;
    }

    pub fn snapshot(&self) -> &[AppNode] {
        &self.snapshot
    }
}

/// Приложения по строкам целиком (тесты).
#[cfg(test)]
pub fn build_apps(rows: &[TrafficRow]) -> Vec<AppNode> {
    let mut counts = AppCounts::default();
    rows.iter().for_each(|row| counts.add(row));
    counts.refresh();
    counts.snapshot
}

/// IP-адрес из `ip:port` / `[v6]:port` и признак loopback.
pub fn peer_ip(peer: &str) -> Option<(String, bool)> {
    let addr: std::net::SocketAddr = peer.parse().ok()?;
    Some((addr.ip().to_string(), addr.ip().is_loopback()))
}

/// Порт из `ip:port`.
pub fn peer_port(peer: &str) -> Option<u16> {
    peer.parse::<std::net::SocketAddr>().ok().map(|a| a.port())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::traffic::store::test_row;

    fn with_client(host: &str, name: &str) -> TrafficRow {
        TrafficRow {
            client: Some(Arc::new(ClientApp {
                name: name.into(),
                icon_key: Some(format!("/Applications/{name}.app")),
                remote: false,
            })),
            ..test_row(host, "/")
        }
    }

    #[test]
    fn apps_are_counted_and_sorted() {
        let rows = vec![
            with_client("a.test", "Safari"),
            with_client("b.test", "Google Chrome"),
            with_client("c.test", "Safari"),
            test_row("d.test", "/"),
        ];
        let apps = build_apps(&rows);
        let names: Vec<_> = apps.iter().map(|a| (a.name.as_str(), a.count)).collect();
        assert_eq!(names, [("Google Chrome", 1), ("Safari", 2)]);
        assert_eq!(
            apps[1].icon_key.as_deref(),
            Some("/Applications/Safari.app")
        );
    }

    #[test]
    fn peers_parse_with_loopback_flag() {
        assert_eq!(peer_ip("127.0.0.1:5000"), Some(("127.0.0.1".into(), true)));
        assert_eq!(peer_ip("[::1]:5000"), Some(("::1".into(), true)));
        assert_eq!(
            peer_ip("192.168.1.20:5000"),
            Some(("192.168.1.20".into(), false))
        );
        assert_eq!(peer_port("192.168.1.20:5000"), Some(5000));
        assert_eq!(peer_ip("garbage"), None);
    }
}
