//! Сетевые настройки: внешний прокси, DNS, port forwarding, reverse proxy.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::value_objects::match_pattern::host_pattern_matches;
use crate::value_objects::{Host, SocketAddrVo};

/// Тип внешнего прокси.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyKind {
    Http,
    Socks5,
    /// Автоконфигурация (PAC-скрипт).
    Pac,
}

/// Авторизация внешнего прокси.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyAuth {
    pub username: String,
    pub password: String,
}

/// Апстрим-прокси для всего исходящего трафика.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalProxyConfig {
    pub kind: ProxyKind,
    pub host: Host,
    pub port: u16,
    #[serde(default)]
    pub auth: Option<ProxyAuth>,
    /// URL PAC-скрипта (для kind = Pac).
    #[serde(default)]
    pub pac_url: Option<String>,
}

/// DNS-override: хост → IP (спуфинг разрешения имён). Шаблон — как в
/// списках хостов: `api.example.com`, `*.example.com` (и сам домен), glob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsOverride {
    pub host_pattern: String,
    pub ip: IpAddr,
}

impl DnsOverride {
    pub fn new(host_pattern: impl Into<String>, ip: IpAddr) -> Self {
        Self {
            host_pattern: host_pattern.into(),
            ip,
        }
    }
}

/// Подменённый адрес хоста: первый совпавший override (порядок списка).
pub fn spoofed_ip(overrides: &[DnsOverride], host: &str) -> Option<IpAddr> {
    overrides
        .iter()
        .find(|o| host_pattern_matches(&o.host_pattern, host))
        .map(|o| o.ip)
}

/// Проброс TCP-порта (запись соединения без exchange).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortForward {
    pub listen: SocketAddrVo,
    pub target: SocketAddrVo,
}

/// Reverse proxy: отдельный листенер → перезапись на target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseProxyConfig {
    pub listen_port: u16,
    /// Целевой origin (`https://api.example.com`).
    pub target: String,
    #[serde(default)]
    pub preserve_host: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_matching_override_wins() {
        let ip = |s: &str| s.parse::<IpAddr>().expect("ip");
        let overrides = vec![
            DnsOverride::new("api.example.com", ip("10.0.0.1")),
            DnsOverride::new("*.example.com", ip("10.0.0.2")),
        ];
        assert_eq!(
            spoofed_ip(&overrides, "API.example.com"),
            Some(ip("10.0.0.1"))
        );
        assert_eq!(spoofed_ip(&overrides, "example.com"), Some(ip("10.0.0.2")));
        assert_eq!(
            spoofed_ip(&overrides, "cdn.example.com."),
            Some(ip("10.0.0.2"))
        );
        assert_eq!(spoofed_ip(&overrides, "example.org"), None);
        assert_eq!(spoofed_ip(&[], "example.com"), None);
    }
}
