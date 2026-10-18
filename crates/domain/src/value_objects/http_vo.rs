//! HTTP-специфичные value objects: метод, статус, версия, заголовки, протокол.

use serde::{Deserialize, Serialize};

use crate::errors::DomainError;

/// HTTP-метод (newtype над строкой, нормализуется в верхний регистр —
/// произвольные extension-методы допустимы).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Method(String);

impl Method {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into().to_uppercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Method {
    fn default() -> Self {
        Self::new("GET")
    }
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Код HTTP-статуса (валидный диапазон 100..=599).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StatusCodeVo(pub u16);

impl StatusCodeVo {
    pub fn new(code: u16) -> Result<Self, DomainError> {
        if (100..=599).contains(&code) {
            Ok(Self(code))
        } else {
            Err(DomainError::InvalidEntity(format!(
                "status code out of range: {code}"
            )))
        }
    }

    pub fn is_success(self) -> bool {
        (200..=299).contains(&self.0)
    }
}

/// Версия HTTP-протокола сообщения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum HttpVersionVo {
    #[serde(rename = "HTTP/0.9")]
    Http09,
    #[serde(rename = "HTTP/1.0")]
    Http10,
    #[default]
    #[serde(rename = "HTTP/1.1")]
    Http11,
    #[serde(rename = "HTTP/2")]
    H2,
    #[serde(rename = "HTTP/3")]
    H3,
}

impl std::fmt::Display for HttpVersionVo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Http09 => "HTTP/0.9",
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
            Self::H2 => "HTTP/2",
            Self::H3 => "HTTP/3",
        };
        f.write_str(s)
    }
}

/// Заголовок HTTP-сообщения (порядок заголовков значим — поэтому Vec, не Map).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderEntry {
    pub name: String,
    pub value: String,
}

impl HeaderEntry {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }

    /// Найти первый заголовок по имени (case-insensitive, как в HTTP).
    pub fn find<'a>(headers: &'a [HeaderEntry], name: &str) -> Option<&'a HeaderEntry> {
        headers.iter().find(|h| h.name.eq_ignore_ascii_case(name))
    }
}

/// Протокол соединения/обмена (для колонки Protocol и решений в движке).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    Http1,
    Http2,
    WebSocket,
    Tunnel,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Http1 => "HTTP/1",
            Self::Http2 => "HTTP/2",
            Self::WebSocket => "WebSocket",
            Self::Tunnel => "Tunnel",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_normalizes_to_uppercase() {
        assert_eq!(Method::new("get").as_str(), "GET");
    }

    #[test]
    fn status_code_validates_range() {
        assert!(StatusCodeVo::new(200).is_ok());
        assert!(StatusCodeVo::new(99).is_err());
        assert!(StatusCodeVo::new(600).is_err());
        assert!(StatusCodeVo::new(204).unwrap().is_success());
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let headers = vec![HeaderEntry::new("Content-Type", "application/json")];
        assert!(HeaderEntry::find(&headers, "content-type").is_some());
    }
}
