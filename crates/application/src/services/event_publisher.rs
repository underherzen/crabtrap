//! Порт публикации событий. Реализация (ChannelEventPublisher) кладёт
//! событие в unbounded `async-channel` — неблокирующе для движка.

use crate::dto::capture::ProxyEvent;

pub trait EventPublisher: Send + Sync {
    /// Опубликовать событие. Синхронно и неблокирующе: переполнение
    /// невозможно (unbounded), закрытый канал — предупреждение в лог.
    fn publish(&self, event: ProxyEvent);
}
