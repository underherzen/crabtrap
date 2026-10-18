//! Хост и адресные value objects.

use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::errors::DomainError;

/// Имя хоста (нормализованное): домен, IPv4 или IPv6.
///
/// Инварианты: непусто, без схемы/пути/пробелов, нижний регистр,
/// без конечной точки (`example.com.` → `example.com`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Host(String);

impl Host {
    /// Создать и нормализовать хост.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let raw = value.into();
        let normalized = raw.trim().trim_end_matches('.').to_lowercase();
        if normalized.is_empty() {
            return Err(DomainError::InvalidEntity("host is empty".to_string()));
        }
        if normalized.contains(['/', '\\', ':', ' ']) {
            return Err(DomainError::InvalidEntity(format!("invalid host: {raw}")));
        }
        Ok(Self(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Host {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Host> for String {
    fn from(value: Host) -> Self {
        value.0
    }
}

impl AsRef<str> for Host {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// IP-адрес с портом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IpEndpoint {
    pub ip: IpAddr,
    pub port: u16,
}

/// Хост+порт (serde-friendly): `{"host": "127.0.0.1", "port": 8080}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SocketAddrVo {
    pub host: Host,
    pub port: u16,
}

impl SocketAddrVo {
    pub fn new(host: Host, port: u16) -> Self {
        Self { host, port }
    }
}

impl fmt::Display for SocketAddrVo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // IPv6 в адресах принято заворачивать в скобки.
        if self.host.as_str().contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_is_normalized_to_lowercase() {
        let host = Host::new("  API.Example.COM. ").expect("valid host");
        assert_eq!(host.as_str(), "api.example.com");
    }

    #[test]
    fn host_rejects_invalid_values() {
        assert!(Host::new("").is_err());
        assert!(Host::new("   ").is_err());
        assert!(Host::new("example.com/path").is_err());
        assert!(Host::new("http://example.com").is_err());
        assert!(Host::new("exa mple").is_err());
    }

    #[test]
    fn host_serde_roundtrip_normalizes() {
        let json = serde_json::to_string("Example.COM").expect("serialize");
        let host: Host = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(host.as_str(), "example.com");
    }

    #[test]
    fn socket_addr_display() {
        let vo = SocketAddrVo::new(Host::new("127.0.0.1").unwrap(), 8080);
        assert_eq!(vo.to_string(), "127.0.0.1:8080");
    }
}
