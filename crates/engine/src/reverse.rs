//! Reverse Proxy (как в Proxyman): отдельный HTTP-листенер на локальном
//! порту; запросы к нему уходят на целевой сервер (`http://localhost:9090/x`
//! → `https://api.example.com/x`) тем же конвейером, что и через прокси:
//! правила, запись, throttle. Нужен клиентам, которые не умеют прокси.
//!
//! Листенеры живут в цикле `run` рядом с основным: таблица меняется
//! командой `SetReverseProxies` (лишние гасятся, новые встают, неизменные
//! не трогаются), Stop гасит все. Итог каждого bind — событие
//! `ReverseProxyStatus`.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use domain::entities::network::ReverseProxyConfig;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::commands::EngineState;
use crate::server;

/// Пауза после ошибки accept (как у основного листенера).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Тег обмена, пришедшего через Reverse Proxy.
pub(crate) const REVERSE_TAG: &str = "reverse-proxy";

/// Куда уходят запросы листенера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReverseTarget {
    /// `https://api.example.com` / `http://localhost:3000` — без пути.
    pub(crate) origin: String,
    /// Оставить Host клиента (`localhost:9090`) вместо хоста цели.
    pub(crate) preserve_host: bool,
}

impl ReverseTarget {
    /// Абсолютный URI цели для origin-form запроса (`/x?y`).
    pub(crate) fn uri_for(&self, path_and_query: &str) -> Result<http::Uri, http::uri::InvalidUri> {
        format!("{}{path_and_query}", self.origin).parse()
    }
}

/// Запущенные листенеры по локальному порту.
pub(crate) struct ReverseProxies {
    state: Arc<EngineState>,
    conn_limit: Arc<Semaphore>,
    /// Адрес интерфейса основного листенера: обратные слушают там же.
    bind_ip: IpAddr,
    running: HashMap<u16, Running>,
    /// Записи, которые не встали: повторяются при следующей таблице, но
    /// ошибка публикуется заново только для изменённой записи — правка
    /// любого другого правила не плодит одинаковых уведомлений.
    failed: HashMap<u16, ReverseProxyConfig>,
}

/// Задача листенера; drop прерывает её (и соединения в её JoinSet).
struct Running {
    config: ReverseProxyConfig,
    task: JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ReverseProxies {
    pub(crate) fn new(
        state: Arc<EngineState>,
        conn_limit: Arc<Semaphore>,
        main_addr: Option<SocketAddr>,
    ) -> Self {
        Self {
            state,
            conn_limit,
            bind_ip: main_addr.map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |a| a.ip()),
            running: HashMap::new(),
            failed: HashMap::new(),
        }
    }

    /// Привести запущенные листенеры к таблице.
    pub(crate) async fn apply(&mut self, entries: Vec<ReverseProxyConfig>) {
        let wanted: HashMap<u16, ReverseProxyConfig> =
            entries.into_iter().map(|e| (e.listen_port, e)).collect();
        let stale: Vec<u16> = self
            .running
            .iter()
            .filter(|(port, run)| wanted.get(port) != Some(&run.config))
            .map(|(port, _)| *port)
            .collect();
        for port in stale {
            self.stop(port).await;
        }
        let dropped: Vec<u16> = self
            .failed
            .keys()
            .filter(|port| !wanted.contains_key(port))
            .copied()
            .collect();
        for port in dropped {
            self.failed.remove(&port);
            self.publish(port, None, None);
        }
        for (port, config) in wanted {
            if !self.running.contains_key(&port) {
                self.start(config).await;
            }
        }
    }

    /// Погасить все (Stop движка).
    pub(crate) async fn stop_all(&mut self) {
        let ports: Vec<u16> = self.running.keys().copied().collect();
        for port in ports {
            self.stop(port).await;
        }
    }

    async fn stop(&mut self, port: u16) {
        if let Some(mut run) = self.running.remove(&port) {
            run.task.abort();
            // Дождаться освобождения порта до повторного bind; JoinError
            // здесь — штатная отмена.
            let _ = (&mut run.task).await;
            self.publish(port, None, None);
        }
    }

    async fn start(&mut self, config: ReverseProxyConfig) {
        let port = config.listen_port;
        let addr = SocketAddr::new(self.bind_ip, port);
        let bound = match points_at_itself(self.bind_ip, &config) {
            true => Err(format!(
                "port {port} forwards to itself ({}) — requests would loop",
                config.target
            )),
            false => TcpListener::bind(addr)
                .await
                .map_err(|error| format!("cannot listen on {addr}: {error}")),
        };
        let listener = match bound {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%addr, %error, "reverse proxy not started");
                if self.failed.get(&port) != Some(&config) {
                    self.publish(port, None, Some(error));
                }
                self.failed.insert(port, config);
                return;
            }
        };
        self.failed.remove(&port);
        let target = Arc::new(ReverseTarget {
            origin: config.target.trim_end_matches('/').to_string(),
            preserve_host: config.preserve_host,
        });
        let local = listener.local_addr().unwrap_or(addr);
        tracing::info!(%local, target = %target.origin, "reverse proxy listening");
        self.publish(port, Some(local.to_string()), None);
        let task = tokio::spawn(accept_loop(
            Arc::clone(&self.state),
            Arc::clone(&self.conn_limit),
            listener,
            target,
        ));
        self.running.insert(port, Running { config, task });
    }

    fn publish(&self, listen_port: u16, listening: Option<String>, error: Option<String>) {
        self.state.events.publish(ProxyEvent::ReverseProxyStatus {
            listen_port,
            listening,
            error,
        });
    }
}

/// Цель — этот же листенер (`localhost:9090` → `localhost:9090`).
fn points_at_itself(bind_ip: IpAddr, config: &ReverseProxyConfig) -> bool {
    let Ok(target) = url::Url::parse(&config.target) else {
        return false;
    };
    let local_host = match target.host() {
        Some(url::Host::Domain(name)) => {
            name.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        }
        Some(url::Host::Ipv4(ip)) => {
            IpAddr::V4(ip) == bind_ip || ip.is_loopback() || ip.is_unspecified()
        }
        Some(url::Host::Ipv6(ip)) => {
            IpAddr::V6(ip) == bind_ip || ip.is_loopback() || ip.is_unspecified()
        }
        None => false,
    };
    local_host && target.port_or_known_default() == Some(config.listen_port)
}

/// Accept-цикл одного листенера; соединения — в его JoinSet (abort задачи
/// рвёт и их).
async fn accept_loop(
    state: Arc<EngineState>,
    conn_limit: Arc<Semaphore>,
    listener: TcpListener,
    target: Arc<ReverseTarget>,
) {
    let mut conns = tokio::task::JoinSet::new();
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let _ = stream.set_nodelay(true);
                let Ok(permit) = Arc::clone(&conn_limit).try_acquire_owned() else {
                    tracing::warn!(%peer, "connection limit reached, dropping reverse connection");
                    continue;
                };
                while conns.try_join_next().is_some() {}
                conns.spawn(server::serve_connection(
                    Arc::clone(&state),
                    stream,
                    peer,
                    permit,
                    Some(Arc::clone(&target)),
                ));
            }
            Err(error) => {
                tracing::warn!(%error, "reverse proxy accept failed, backing off");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_on_its_own_port_is_a_loop() {
        let lan: IpAddr = "192.168.1.10".parse().expect("ip");
        let config = |target: &str| ReverseProxyConfig {
            listen_port: 9090,
            target: target.into(),
            preserve_host: false,
        };
        let loops = |target: &str| points_at_itself(lan, &config(target));
        assert!(loops("http://localhost:9090"));
        assert!(loops("http://127.0.0.1:9090"));
        assert!(loops("http://[::1]:9090"));
        assert!(loops("http://localhost.:9090"));
        assert!(loops("http://192.168.1.10:9090"));
        assert!(!loops("http://localhost:9091"));
        assert!(!loops("https://api.example.com:9090"));
        assert!(!loops("http://192.168.1.11:9090"));
    }

    #[test]
    fn target_uri_keeps_path_and_query() {
        let target = ReverseTarget {
            origin: "https://api.example.com:8443".into(),
            preserve_host: false,
        };
        assert_eq!(
            target.uri_for("/v1/users?id=7").expect("uri").to_string(),
            "https://api.example.com:8443/v1/users?id=7"
        );
    }
}
