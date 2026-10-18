//! Доступ к прокси из локальной сети (Settings → Network): листенер на всех
//! интерфейсах (`0.0.0.0`, телефон в той же Wi-Fi проксируется через Mac) или
//! только на этом Mac (`127.0.0.1`). Переключение сохраняется в config.toml
//! и переносит работающий листенер на новый адрес.

use application::dto::proxy_config::EngineCommand;
use gpui_kit::{App, Context, Entity};

use crate::app_state::AppState;
use crate::ui::status_bar::EngineStatusStore;

/// IP этого Mac в локальной сети (None — сети нет). Синхронный UDP-connect
/// трюк без реального сетевого IO.
pub fn lan_ip() -> Option<String> {
    local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .map_err(|e| tracing::warn!(error = %e, "LAN IP lookup failed"))
        .ok()
}

/// Порт адреса листенера (`0.0.0.0:8080` → `8080`).
pub fn listen_port(listen_addr: &str) -> &str {
    listen_addr.rsplit(':').next().unwrap_or("8080")
}

pub struct NetworkStore {
    state: AppState,
    status: Entity<EngineStatusStore>,
    /// Последняя ошибка сохранения конфига.
    pub error: Option<String>,
}

impl NetworkStore {
    pub fn new(state: AppState, status: Entity<EngineStatusStore>, cx: &mut Context<Self>) -> Self {
        // Статус движка (Running/Stopped) влияет на подсказки панели.
        cx.observe(&status, |_, _, cx| cx.notify()).detach();
        Self {
            state,
            status,
            error: None,
        }
    }

    pub fn listen_addr(&self) -> String {
        self.state.listen_addr.get()
    }

    pub fn lan_access(&self) -> bool {
        config::lan_access(&self.listen_addr())
    }

    /// Включить/выключить доступ из сети: сохранить адрес и, если прокси
    /// запущен, перенести листенер (остановленный поднимется на новом адресе
    /// при следующем Start).
    pub fn set_lan_access(entity: &Entity<Self>, allow: bool, cx: &mut App) {
        entity.update(cx, |this, cx| {
            let current = this.listen_addr();
            let next = config::with_lan_access(&current, allow);
            if next == current {
                return;
            }
            let saved = config::AppConfig::update_proxy(|proxy| config::ProxyConfig {
                listen_addr: next.clone(),
                ..proxy
            });
            if let Err(e) = saved {
                tracing::error!(error = %e, "cannot save listen address");
                this.error = Some(format!("Cannot save config.toml: {e}"));
                cx.notify();
                return;
            }
            this.error = None;
            this.state.listen_addr.set(next);
            if this.status.read(cx).listening.is_some() {
                this.state.engine_command.send(EngineCommand::Start {
                    settings: this.state.proxy_settings(),
                });
            }
            cx.notify();
        });
    }
}
