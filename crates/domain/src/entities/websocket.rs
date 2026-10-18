//! Сообщения WebSocket-обмена (Phase 11b): что прошло по соединению после
//! апгрейда — направление, тип, содержимое (с потолком), размер, время.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Сколько байт содержимого сообщения хранится (размер — полный).
pub const WS_PAYLOAD_CAP: usize = 64 * 1024;
/// Сколько сообщений хранится на обмен (старые вытесняются).
pub const WS_MESSAGES_CAP: usize = 2000;
/// Сколько байт содержимого хранится на обмен: дальше сообщения пишутся
/// без содержимого (размер, тип и время остаются).
pub const WS_BYTES_CAP: usize = 16 * 1024 * 1024;

/// Порция сообщений: начиная с курсора и курсор для следующей.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WsMessagesPage {
    pub messages: Vec<WebSocketMessage>,
    /// Сколько сообщений пришло всего (курсор следующего запроса).
    pub next: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WsDirection {
    /// Клиент → сервер.
    Sent,
    /// Сервер → клиент.
    Received,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WsOpcode {
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSocketMessage {
    pub direction: WsDirection,
    pub opcode: WsOpcode,
    /// Содержимое (не больше [`WS_PAYLOAD_CAP`]).
    pub payload: Vec<u8>,
    /// Полный размер содержимого.
    pub size: u64,
    pub at: DateTime<Utc>,
}

impl WebSocketMessage {
    /// Текст сообщения (text — как есть, остальное — `None`).
    pub fn text(&self) -> Option<String> {
        match self.opcode {
            WsOpcode::Text => Some(String::from_utf8_lossy(&self.payload).into_owned()),
            _ => None,
        }
    }

    /// Содержимое обрезано потолком.
    pub fn truncated(&self) -> bool {
        (self.payload.len() as u64) < self.size
    }
}
