//! SystemProxyStore — состояние кнопки «macOS Proxy» (тоггл системного
//! прокси через `networksetup`). Паттерн CertificatesStore: ассоциированные
//! функции над `&Entity<Self>`, асинхронщина на движковом рантайме,
//! результат — через async-channel (D4).

use std::sync::Arc;

use application::dto::proxy_config::SystemProxyStatusDto;
use application::services::SystemProxyService;
use application::use_cases::{
    GetSystemProxyStatusUseCase, SetSystemProxyUseCase, UnsetSystemProxyUseCase,
};
use domain::value_objects::{Host, SocketAddrVo};
use gpui_kit::{App, Entity};

use crate::app_state::AppState;

/// Период фонового перечитывания статуса системного прокси.
const STATUS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
use crate::ui::async_bridge::spawn_ui_task;

/// Что на самом деле с системным прокси относительно НАШЕГО листенера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyIndicator {
    /// Приложения ходят через crabtrap (эффективные HTTP и HTTPS — наши).
    On,
    /// Прокси не настроен.
    Off,
    /// В каком-то сервисе прописаны мы, но в текущей сети это не действует
    /// (другой активный сервис, VPN пересоздал свой, только HTTP и т.п.).
    Stale,
    /// Приложения ходят через ЧУЖОЙ прокси.
    Other(String),
}

impl ProxyIndicator {
    pub fn of(status: &SystemProxyStatusDto, listen_addr: &str) -> Self {
        let (host, port) = split_listen(listen_addr);
        let ours = format!("{host}:{port}");
        let is_ours = |s: &Option<String>| s.as_deref() == Some(ours.as_str());
        if is_ours(&status.server) && is_ours(&status.secure_server) {
            return Self::On;
        }
        let foreign = [&status.server, &status.secure_server]
            .into_iter()
            .flatten()
            .find(|s| **s != ours);
        if let Some(server) = foreign {
            return Self::Other(server.clone());
        }
        if status.configured_servers.contains(&ours)
            || is_ours(&status.server)
            || is_ours(&status.secure_server)
        {
            return Self::Stale;
        }
        Self::Off
    }

    /// Мы прописаны хоть где-то — при выходе это надо снять.
    pub fn ours_anywhere(&self) -> bool {
        matches!(self, Self::On | Self::Stale)
    }
}

pub struct SystemProxyStore {
    /// Последний известный статус (None — ещё не спрашивали).
    pub status: Option<SystemProxyStatusDto>,
    pub busy: bool,
    pub error: Option<String>,
    service: Arc<dyn SystemProxyService>,
    runtime: tokio::runtime::Handle,
    listen_host: String,
    listen_port: u16,
    /// Растёт на каждом тоггле: refresh, начатый до тоггла, не затирает
    /// его результат устаревшим статусом.
    generation: u64,
}

impl SystemProxyStore {
    pub fn new(state: &AppState, runtime: tokio::runtime::Handle) -> Self {
        let (host, port) = split_listen(&state.config.proxy.listen_addr);
        Self {
            status: None,
            busy: false,
            error: None,
            service: state.system_proxy_service.clone(),
            runtime,
            listen_host: host,
            listen_port: port,
            generation: 0,
        }
    }

    /// Индикатор для UI (None-статус — ещё не прочитан → Off).
    pub fn indicator(&self) -> ProxyIndicator {
        self.status
            .as_ref()
            .map(|s| ProxyIndicator::of(s, &format!("{}:{}", self.listen_host, self.listen_port)))
            .unwrap_or(ProxyIndicator::Off)
    }

    /// Трафик реально идёт через crabtrap.
    pub fn enabled(&self) -> bool {
        self.indicator() == ProxyIndicator::On
    }

    /// Фоновое перечитывание статуса: сеть/VPN меняются, пока окно открыто,
    /// а networksetup-события нам никто не шлёт.
    pub fn start_polling(entity: &Entity<Self>, cx: &mut App) {
        let weak = entity.downgrade();
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(STATUS_POLL_INTERVAL).await;
                let Some(entity) = weak.upgrade() else {
                    break;
                };
                cx.update(|cx| Self::refresh(&entity, cx));
            }
        })
        .detach();
    }

    /// Перечитать статус (вызывается при старте и после тогглов).
    pub fn refresh(entity: &Entity<Self>, cx: &mut App) {
        let (service, runtime, generation) = {
            let s = entity.read(cx);
            if s.busy {
                return;
            }
            (s.service.clone(), s.runtime.clone(), s.generation)
        };
        spawn_ui_task(
            cx,
            entity,
            runtime,
            async move { GetSystemProxyStatusUseCase::new(service).execute().await },
            move |state: &mut Self, result, cx| {
                if state.busy || state.generation != generation {
                    return; // пока читали, был тоггл — его статус свежее
                }
                match result {
                    Ok(status) => {
                        state.status = Some(status);
                        state.error = None;
                    }
                    Err(e) => state.error = Some(e.to_string()),
                }
                cx.notify();
            },
        );
    }

    /// Тоггл: трафик идёт через нас → снять; иначе (off / stale / чужой)
    /// → прописать crabtrap во все сервисы.
    pub fn toggle(entity: &Entity<Self>, cx: &mut App) {
        let props = entity.update(cx, |state, cx| {
            if state.busy {
                cx.notify();
                return None;
            }
            state.busy = true;
            state.generation += 1;
            state.error = None;
            cx.notify();
            Some((
                state.service.clone(),
                state.runtime.clone(),
                state.enabled(),
                state.listen_host.clone(),
                state.listen_port,
            ))
        });
        let (service, runtime, enabled, host, port) = match props {
            Some(v) => v,
            None => return,
        };
        let host_for_error = host.clone();
        spawn_ui_task(
            cx,
            entity,
            runtime,
            async move {
                // Тоггл и перечитывание статуса — ОДНА задача: отдельный
                // refresh после спавна гонился бы с networksetup и читал
                // старое состояние (кнопка залипала в «off»).
                let toggled = if enabled {
                    UnsetSystemProxyUseCase::new(service.clone())
                        .execute()
                        .await
                } else {
                    let host = Host::new(&host)
                        .map_err(|e| domain::DomainError::ValidationError(e.to_string()))?;
                    SetSystemProxyUseCase::new(service.clone())
                        .execute(SocketAddrVo::new(host, port), Vec::new())
                        .await
                };
                let status = GetSystemProxyStatusUseCase::new(service).execute().await;
                match (toggled, status) {
                    (Ok(()), Ok(status)) => Ok(status),
                    (Err(e), _) => Err(e),
                    (Ok(()), Err(e)) => Err(e),
                }
            },
            move |state: &mut Self, result, cx| {
                state.busy = false;
                match result {
                    Ok(status) => {
                        tracing::info!(
                            host = %host_for_error,
                            port,
                            enabled = status.enabled,
                            "system proxy toggled"
                        );
                        state.status = Some(status);
                        state.error = None;
                    }
                    Err(e) => state.error = Some(e.to_string()),
                }
                cx.notify();
            },
        );
    }
}

/// Адреса «все интерфейсы»: подключаться к ним как к прокси нельзя.
const WILDCARD_HOSTS: [&str; 3] = ["0.0.0.0", "[::]", "::"];

/// Адрес прокси для системных настроек этого Mac: `127.0.0.1:8080` →
/// ("127.0.0.1", 8080); листенер на всех интерфейсах (`0.0.0.0`) — тоже
/// loopback; fallback (127.0.0.1, 8080).
pub(crate) fn split_listen(addr: &str) -> (String, u16) {
    match addr.rsplit_once(':') {
        Some((host, port)) => {
            let port = port.parse().unwrap_or(8080);
            let host = if WILDCARD_HOSTS.contains(&host) {
                "127.0.0.1"
            } else {
                host
            };
            (host.to_string(), port)
        }
        None => ("127.0.0.1".to_string(), 8080),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(
        http: Option<&str>,
        https: Option<&str>,
        configured: &[&str],
    ) -> SystemProxyStatusDto {
        SystemProxyStatusDto {
            enabled: http.is_some() && https.is_some(),
            server: http.map(str::to_string),
            secure_server: https.map(str::to_string),
            configured_servers: configured.iter().map(|s| s.to_string()).collect(),
            bypass: Vec::new(),
            details: None,
        }
    }

    const OURS: &str = "127.0.0.1:8080";

    #[test]
    fn on_only_when_effective_http_and_https_point_at_us() {
        let st = status(Some(OURS), Some(OURS), &[OURS]);
        assert_eq!(ProxyIndicator::of(&st, OURS), ProxyIndicator::On);
    }

    #[test]
    fn configured_but_not_effective_is_stale_not_on() {
        // Wi-Fi прописан на нас, но активный сервис (VPN) — мимо прокси.
        let st = status(None, None, &[OURS]);
        assert_eq!(ProxyIndicator::of(&st, OURS), ProxyIndicator::Stale);
        // Только HTTP — HTTPS мимо: тоже не «on».
        let st = status(Some(OURS), None, &[OURS]);
        assert_eq!(ProxyIndicator::of(&st, OURS), ProxyIndicator::Stale);
    }

    #[test]
    fn foreign_effective_proxy_is_other() {
        let st = status(
            Some("10.0.0.1:3128"),
            Some("10.0.0.1:3128"),
            &["10.0.0.1:3128"],
        );
        assert_eq!(
            ProxyIndicator::of(&st, OURS),
            ProxyIndicator::Other("10.0.0.1:3128".into())
        );
    }

    #[test]
    fn nothing_configured_is_off() {
        assert_eq!(
            ProxyIndicator::of(&status(None, None, &[]), OURS),
            ProxyIndicator::Off
        );
    }

    #[test]
    fn ours_anywhere_covers_on_and_stale() {
        assert!(ProxyIndicator::On.ours_anywhere());
        assert!(ProxyIndicator::Stale.ours_anywhere());
        assert!(!ProxyIndicator::Off.ours_anywhere());
        assert!(!ProxyIndicator::Other("x".into()).ours_anywhere());
    }

    #[test]
    fn splits_listen_addr() {
        assert_eq!(split_listen("127.0.0.1:8080"), ("127.0.0.1".into(), 8080));
        // Листенер на всех интерфейсах: системный прокси Mac — loopback.
        assert_eq!(split_listen("0.0.0.0:3128"), ("127.0.0.1".into(), 3128));
        assert_eq!(split_listen("[::]:8080"), ("127.0.0.1".into(), 8080));
        assert_eq!(split_listen("junk"), ("127.0.0.1".into(), 8080));
    }
}
