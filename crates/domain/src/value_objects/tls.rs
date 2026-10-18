//! Информация о TLS-соединении обмена.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Параметры TLS-сессии (MITM-лист в UI показывает `mitm`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsInfo {
    /// SNI, отправленный клиентом.
    pub sni: Option<String>,
    /// Выбранный ALPN-протокол.
    pub alpn: Option<String>,
    /// Версия TLS (например, `TLSv1.3`).
    pub version: Option<String>,
    /// Suite-имя шифра.
    pub cipher_suite: Option<String>,
    /// Соединение расшифровано MITM-прокси.
    pub mitm: bool,
    /// NotAfter листа сертификата (для индикатора истечения).
    pub leaf_not_after: Option<DateTime<Utc>>,
    /// Subject сертификата узла.
    pub peer_subject: Option<String>,
}
