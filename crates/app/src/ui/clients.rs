//! Клиенты обменов (Track G8): какое приложение открыло соединение.
//!
//! Pump отдаёт сюда пачки событий: `ConnectionOpened` с loopback-адресом
//! копится и раз в [`RESOLVE_DEBOUNCE`] уходит пачкой в
//! [`ClientProcessService`] (один вызов lsof на всю пачку); адрес из сети —
//! это устройство, его имя — IP. Найденные клиенты подписывают строки в
//! TrafficStore, иконки приложений (PNG из NSWorkspace) кэшируются по пути
//! бандла и рисуются таблицей и боковой панелью через [`ClientsStore::icon`].
//!
//! Ограничение: соединение, закрытое раньше вызова lsof (одиночный запрос
//! без keep-alive живёт миллисекунды), уже не принадлежит процессу —
//! такие обмены остаются без клиента. Браузеры держат соединения, их видно.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use application::dto::capture::ProxyEvent;
use application::dto::clients::ClientAppDto;
use application::services::ClientProcessService;
use domain::value_objects::ConnectionId;
use gpui_kit::{App, AppContext as _, Entity, Global, Image, ImageFormat};

use crate::ui::async_bridge::spawn_ui_task;
use crate::ui::traffic::apps::{ClientApp, peer_ip, peer_port};
use crate::ui::traffic::store::TrafficStore;

/// Сколько копим новые соединения, прежде чем спросить lsof.
const RESOLVE_DEBOUNCE: Duration = Duration::from_millis(80);

enum IconSlot {
    Loading,
    Ready(Arc<Image>),
    Missing,
}

pub struct ClientsStore {
    service: Arc<dyn ClientProcessService>,
    runtime: tokio::runtime::Handle,
    traffic: Entity<TrafficStore>,
    proxy_port: u16,
    /// Локальные соединения, ждущие lsof: (соединение, порт клиента).
    pending: Vec<(ConnectionId, u16)>,
    resolving: bool,
    icons: HashMap<String, IconSlot>,
}

struct ClientsHandle(Entity<ClientsStore>);

impl Global for ClientsHandle {}

impl ClientsStore {
    pub fn new(
        service: Arc<dyn ClientProcessService>,
        runtime: tokio::runtime::Handle,
        traffic: Entity<TrafficStore>,
        listen_addr: &str,
    ) -> Self {
        Self {
            service,
            runtime,
            traffic,
            proxy_port: peer_port(listen_addr).unwrap_or_default(),
            pending: Vec::new(),
            resolving: false,
            icons: HashMap::new(),
        }
    }

    /// Зарегистрировать глобально (иконки читают таблица и панель).
    pub fn install(entity: &Entity<Self>, cx: &mut App) {
        cx.set_global(ClientsHandle(entity.clone()));
    }

    /// Иконка приложения по ключу (если уже загружена).
    pub fn icon(key: &str, cx: &App) -> Option<Arc<Image>> {
        let store = cx.try_global::<ClientsHandle>()?.0.read(cx);
        match store.icons.get(key) {
            Some(IconSlot::Ready(image)) => Some(image.clone()),
            _ => None,
        }
    }

    /// Пачка событий pump'а: запомнить новые соединения.
    pub fn on_events(entity: &Entity<Self>, events: &[ProxyEvent], cx: &mut App) {
        let mut devices = Vec::new();
        let queued = entity.update(cx, |this, _| {
            let before = this.pending.len();
            for event in events {
                let ProxyEvent::ConnectionOpened { id, peer, .. } = event else {
                    continue;
                };
                match (peer_ip(peer), peer_port(peer)) {
                    (Some((_, true)), Some(port)) => this.pending.push((*id, port)),
                    (Some((ip, false)), _) => devices.push((*id, Arc::new(ClientApp::device(&ip)))),
                    _ => {}
                }
            }
            this.pending.len() > before
        });
        if !devices.is_empty() {
            let traffic = entity.read(cx).traffic.clone();
            traffic.update(cx, |store, cx| {
                store.set_clients(devices);
                cx.notify();
            });
        }
        if queued {
            Self::schedule(entity, cx);
        }
    }

    fn schedule(entity: &Entity<Self>, cx: &mut App) {
        let start = entity.update(cx, |this, _| !std::mem::replace(&mut this.resolving, true));
        if !start {
            return;
        }
        let weak = entity.downgrade();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(RESOLVE_DEBOUNCE).await;
            if let Some(entity) = weak.upgrade() {
                cx.update(|cx| Self::resolve_now(&entity, cx));
            }
        })
        .detach();
    }

    fn resolve_now(entity: &Entity<Self>, cx: &mut App) {
        let (service, runtime, proxy_port, batch) = entity.update(cx, |this, _| {
            (
                this.service.clone(),
                this.runtime.clone(),
                this.proxy_port,
                std::mem::take(&mut this.pending),
            )
        });
        let ports: Vec<u16> = batch.iter().map(|(_, port)| *port).collect();
        let fut = async move { service.resolve(proxy_port, ports).await };
        spawn_ui_task(cx, entity, runtime, fut, move |this, result, cx| {
            this.resolving = false;
            let found: HashMap<u16, ClientAppDto> = match result {
                Ok(lookups) => lookups
                    .into_iter()
                    .map(|l| (l.client_port, l.app))
                    .collect(),
                Err(e) => {
                    tracing::debug!(error = %e, "client lookup failed");
                    HashMap::new()
                }
            };
            let resolved: Vec<(ConnectionId, Arc<ClientApp>)> = batch
                .iter()
                .filter_map(|(conn, port)| {
                    let app = found.get(port)?;
                    Some((
                        *conn,
                        Arc::new(ClientApp {
                            name: app.name.clone(),
                            icon_key: app.bundle_path.clone().or_else(|| app.executable.clone()),
                            remote: false,
                        }),
                    ))
                })
                .collect();
            let wanted: Vec<ClientAppDto> = found
                .into_values()
                .filter(|app| {
                    let key = app.bundle_path.as_ref().or(app.executable.as_ref());
                    key.is_some_and(|k| !this.icons.contains_key(k))
                })
                .collect();
            this.traffic.update(cx, |store, cx| {
                store.set_clients(resolved);
                cx.notify();
            });
            // Иконки и следующая пачка — после выхода из update этого стора
            // (повторный update той же Entity внутри — паника gpui).
            let entity = cx.entity();
            let more = !this.pending.is_empty();
            cx.defer(move |cx| {
                for app in wanted {
                    Self::load_icon(&entity, app, cx);
                }
                if more {
                    Self::schedule(&entity, cx);
                }
            });
        });
    }

    fn load_icon(entity: &Entity<Self>, app: ClientAppDto, cx: &mut App) {
        let Some(key) = app.bundle_path.clone().or_else(|| app.executable.clone()) else {
            return;
        };
        let (service, runtime) = entity.update(cx, |this, _| {
            this.icons.entry(key.clone()).or_insert(IconSlot::Loading);
            (this.service.clone(), this.runtime.clone())
        });
        let fut = async move { service.icon_png(app).await };
        spawn_ui_task(cx, entity, runtime, fut, move |this, result, cx| {
            let slot = match result {
                Ok(Some(png)) => {
                    IconSlot::Ready(Arc::new(Image::from_bytes(ImageFormat::Png, png)))
                }
                _ => IconSlot::Missing,
            };
            this.icons.insert(key, slot);
            cx.notify();
            // Иконку рисуют таблица и панель — обе следят за TrafficStore.
            this.traffic.update(cx, |_, cx| cx.notify());
        });
    }
}

/// Создать стор клиентов (вызывается в `main`).
pub fn build(
    service: Arc<dyn ClientProcessService>,
    runtime: tokio::runtime::Handle,
    traffic: &Entity<TrafficStore>,
    listen_addr: &str,
    cx: &mut App,
) -> Entity<ClientsStore> {
    let entity = cx.new(|_| ClientsStore::new(service, runtime, traffic.clone(), listen_addr));
    ClientsStore::install(&entity, cx);
    entity
}
