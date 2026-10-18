//! Сохранение настроек интерфейса (закреплённые домены, сайдбар,
//! раскладка инспектора) в `[ui]` config.toml.
//!
//! Записи идут строго по очереди в одном фоновом потоке: два клика подряд
//! не перепишут файл в обратном порядке и не столкнутся на диске.

use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

use config::{AppConfig, UiConfig};

type UiChange = Box<dyn FnOnce(UiConfig) -> UiConfig + Send>;

fn queue() -> &'static Sender<UiChange> {
    static QUEUE: OnceLock<Sender<UiChange>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = channel::<UiChange>();
        let spawned = std::thread::Builder::new()
            .name("crabtrap-prefs".into())
            .spawn(move || {
                for change in rx {
                    match AppConfig::update_ui(change) {
                        Ok(_) => tracing::debug!("UI preferences saved"),
                        Err(e) => {
                            tracing::warn!(error = %e, "cannot save UI preferences to config.toml")
                        }
                    }
                }
            });
        // Поток не создался — приёмник уже уничтожен, `send` вернёт ошибку
        // и её залогирует `save_ui`.
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "cannot start UI preferences writer");
        }
        tx
    })
}

/// Поменять `[ui]` в config.toml. Ошибка записи — только лог: настройка
/// уже применена в окне, теряется лишь её сохранение.
pub fn save_ui(change: impl FnOnce(UiConfig) -> UiConfig + Send + 'static) {
    if queue().send(Box::new(change)).is_err() {
        tracing::warn!("UI preferences writer is gone; setting not saved");
    }
}
