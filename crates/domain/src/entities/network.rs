//! Сетевые настройки: внешний прокси, DNS, port forwarding, reverse proxy.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

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

/// DNS-override: хост → IP (спуфинг разрешения имён).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsOverride {
    pub host: Host,
    pub ip: IpAddr,
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
