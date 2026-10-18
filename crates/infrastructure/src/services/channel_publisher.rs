//! Публикация событий через unbounded `async-channel` — единственный
//! транспорт engine → UI (решение D4: tokio-потоки и gpui оба умеют
//! await-ить async-channel).

use async_channel::{Receiver, Sender};

use application::dto::capture::ProxyEvent;
use application::services::EventPublisher;

pub struct ChannelEventPublisher {
    sender: Sender<ProxyEvent>,
    receiver: Receiver<ProxyEvent>,
}

impl ChannelEventPublisher {
    pub fn new() -> Self {
        let (sender, receiver) = async_channel::unbounded();
        Self { sender, receiver }
    }

    /// Клон ресивера для потребителя (pump событий в app/runtime.rs).
    /// Ресивер хранится и в publisher'е — канал живёт, пока жив publisher.
    pub fn subscribe(&self) -> Receiver<ProxyEvent> {
        self.receiver.clone()
    }

    /// Текущая длина очереди (метрика для мониторинга отставания UI).
    pub fn pending(&self) -> usize {
        self.sender.len()
    }
}

impl Default for ChannelEventPublisher {
    fn default() -> Self {
        Self::new()
    }
}

impl EventPublisher for ChannelEventPublisher {
    fn publish(&self, event: ProxyEvent) {
        // unbounded: try_send не блокирует; ошибка — только закрытый канал.
        if let Err(e) = self.sender.try_send(event) {
            tracing::warn!(error = %e, "event channel closed, dropping event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use domain::value_objects::ConnectionId;

    #[tokio::test]
    async fn published_events_drain_in_order() {
        let publisher = ChannelEventPublisher::new();
        let rx = publisher.subscribe();

        publisher.publish(ProxyEvent::ConnectionOpened {
            id: ConnectionId::new(),
            peer: "127.0.0.1:1".into(),
            ts: Utc::now(),
        });
        publisher.publish(ProxyEvent::ConnectionClosed {
            id: ConnectionId::new(),
        });
        assert_eq!(publisher.pending(), 2);

        assert!(matches!(
            rx.recv().await,
            Ok(ProxyEvent::ConnectionOpened { .. })
        ));
        assert!(matches!(
            rx.recv().await,
            Ok(ProxyEvent::ConnectionClosed { .. })
        ));
        assert_eq!(publisher.pending(), 0);
    }

    #[tokio::test]
    async fn cloned_receivers_share_the_stream_once_per_event() {
        // async-channel — не broadcast: каждое сообщение достаётся ровно
        // одному ресиверу. Штатный режим crabtrap — единственный pump в app.
        let publisher = ChannelEventPublisher::new();
        let rx1 = publisher.subscribe();
        let rx2 = publisher.subscribe();
        publisher.publish(ProxyEvent::ConnectionClosed {
            id: ConnectionId::new(),
        });
        publisher.publish(ProxyEvent::ConnectionClosed {
            id: ConnectionId::new(),
        });

        let a = rx1.recv().await;
        let b = rx2.recv().await;
        assert!(a.is_ok() && b.is_ok());
        // Оба сообщения получены по одному разу.
        assert_eq!(publisher.pending(), 0);
    }

    #[tokio::test]
    async fn publish_after_all_receivers_dropped_does_not_panic() {
        let publisher = ChannelEventPublisher::new();
        drop(publisher.subscribe());
        publisher.publish(ProxyEvent::ConnectionClosed {
            id: ConnectionId::new(),
        });
    }
}
