//! Отправка сообщений в открытый WebSocket из UI (Resend / Send Message):
//! регистр сокетов по обмену и очереди вставки для обоих направлений.
//! Сам кадр пишет ретрансляция ([`crate::ws`]) — только на границе
//! сообщения, чтобы не разрезать чужой кадр.

use std::collections::HashMap;
use std::sync::Mutex;

use domain::entities::websocket::{WsDirection, WsOpcode};
use domain::value_objects::ExchangeId;
use tokio::sync::mpsc;

/// Сколько сообщений может ждать границы кадра в одном направлении.
pub(crate) const QUEUE: usize = 64;
/// Потолок одного сообщения из UI.
const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

/// Сообщение для вставки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Injection {
    pub opcode: WsOpcode,
    pub payload: Vec<u8>,
}

/// Очереди вставки открытого сокета.
#[derive(Clone)]
struct Injector {
    to_server: mpsc::Sender<Injection>,
    to_client: mpsc::Sender<Injection>,
}

/// Приёмные концы очередей — у ретрансляции.
pub(crate) struct Queues {
    pub to_server: mpsc::Receiver<Injection>,
    pub to_client: mpsc::Receiver<Injection>,
}

/// Открытые сокеты движка.
#[derive(Default)]
pub(crate) struct WsSockets(Mutex<HashMap<ExchangeId, Injector>>);

impl WsSockets {
    /// Зарегистрировать сокет обмена; снимается [`Self::remove`].
    pub(crate) fn register(&self, id: ExchangeId) -> Queues {
        let (to_server, server_rx) = mpsc::channel(QUEUE);
        let (to_client, client_rx) = mpsc::channel(QUEUE);
        self.lock().insert(
            id,
            Injector {
                to_server,
                to_client,
            },
        );
        Queues {
            to_server: server_rx,
            to_client: client_rx,
        }
    }

    pub(crate) fn remove(&self, id: &ExchangeId) {
        self.lock().remove(id);
    }

    /// Поставить сообщение в очередь сокета. `Err` — понятная причина.
    pub(crate) fn send(
        &self,
        id: &ExchangeId,
        direction: WsDirection,
        injection: Injection,
    ) -> Result<(), String> {
        validate(&injection)?;
        let injector = self
            .lock()
            .get(id)
            .cloned()
            .ok_or("the WebSocket is closed")?;
        let queue = match direction {
            WsDirection::Sent => &injector.to_server,
            WsDirection::Received => &injector.to_client,
        };
        queue.try_send(injection).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                "too many messages are waiting to be sent".to_string()
            }
            mpsc::error::TrySendError::Closed(_) => "the WebSocket is closed".to_string(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ExchangeId, Injector>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Только данные: управляющие кадры (close/ping/pong) — дело самих
/// сторон; текст обязан быть UTF-8 (RFC 6455 §5.6).
fn validate(injection: &Injection) -> Result<(), String> {
    if injection.payload.len() > MAX_PAYLOAD {
        return Err(format!(
            "a message can be at most {} MiB",
            MAX_PAYLOAD / 1024 / 1024
        ));
    }
    match injection.opcode {
        WsOpcode::Text if std::str::from_utf8(&injection.payload).is_err() => {
            Err("a text message must be valid UTF-8".into())
        }
        WsOpcode::Text | WsOpcode::Binary => Ok(()),
        WsOpcode::Close | WsOpcode::Ping | WsOpcode::Pong => {
            Err("only text and binary messages can be sent".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(payload: &[u8]) -> Injection {
        Injection {
            opcode: WsOpcode::Text,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn messages_reach_the_queue_of_their_direction() {
        let sockets = WsSockets::default();
        let id = ExchangeId::new();
        let mut queues = sockets.register(id);

        sockets
            .send(&id, WsDirection::Sent, text(b"up"))
            .expect("queued");
        sockets
            .send(&id, WsDirection::Received, text(b"down"))
            .expect("queued");

        assert_eq!(queues.to_server.try_recv().expect("server"), text(b"up"));
        assert_eq!(queues.to_client.try_recv().expect("client"), text(b"down"));
    }

    #[test]
    fn closed_unknown_and_invalid_messages_are_refused() {
        let sockets = WsSockets::default();
        let id = ExchangeId::new();
        assert!(sockets.send(&id, WsDirection::Sent, text(b"x")).is_err());
        let _queues = sockets.register(id);
        assert!(sockets.send(&id, WsDirection::Sent, text(&[0xff])).is_err());
        let ping = Injection {
            opcode: WsOpcode::Ping,
            payload: Vec::new(),
        };
        assert!(sockets.send(&id, WsDirection::Sent, ping).is_err());
        let huge = Injection {
            opcode: WsOpcode::Binary,
            payload: vec![0; MAX_PAYLOAD + 1],
        };
        assert!(sockets.send(&id, WsDirection::Sent, huge).is_err());
        sockets.remove(&id);
        assert!(sockets.send(&id, WsDirection::Sent, text(b"x")).is_err());
    }
}
