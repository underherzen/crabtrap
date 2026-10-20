//! Accept-loop и обслуживание одного клиентского соединения.
//!
//! Каждое соединение — задача в `JoinSet` главного цикла (Stop прерывает их
//! все), ограничение одновременных соединений — семафор на
//! `max_concurrent_conns`. На соединении поднимается hyper http1 сервер с
//! [`ProxyService`]; upgrade включён (нужно для CONNECT-туннелей).

use std::sync::Arc;

use application::dto::capture::ProxyEvent;
use chrono::Utc;
use domain::value_objects::ConnectionId;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

use crate::commands::{ConnGuard, EngineState};
use crate::http_proxy::ProxyService;
use crate::reverse::ReverseTarget;

/// Обслужить одно принятое соединение (запускается в JoinSet).
///
/// `permit` (слот лимита) уезжает в `guard` и живёт, пока жив хоть один его
/// клон — включая туннельную задачу. Соединение закрывается ровно один раз:
/// здесь, либо (после CONNECT) из туннельной задачи.
pub(crate) async fn serve_connection(
    state: Arc<EngineState>,
    stream: TcpStream,
    peer: std::net::SocketAddr,
    permit: tokio::sync::OwnedSemaphorePermit,
    reverse: Option<Arc<ReverseTarget>>,
) {
    let conn_id = ConnectionId::new();
    let seq = state.next_conn_seq();
    tracing::debug!(%conn_id, %peer, seq, "connection opened");
    state.events.publish(ProxyEvent::ConnectionOpened {
        id: conn_id,
        peer: peer.to_string(),
        ts: Utc::now(),
    });
    state.conn_opened();

    let guard = ConnGuard::new(state.clone(), conn_id, permit);
    let listener_addr = stream.local_addr().ok();
    let io = TokioIo::new(stream);
    let service = ProxyService::new(state.clone(), conn_id, guard.clone())
        .with_listener_addr(listener_addr)
        .with_reverse(reverse);

    let res = hyper::server::conn::http1::Builder::new()
        .preserve_header_case(true)
        .serve_connection(io, service)
        .with_upgrades()
        .await;

    if let Err(e) = res {
        // Обрыв клиента посреди соединения — обычное дело, не шумим.
        tracing::debug!(%conn_id, error = ?e, "http1 connection ended with error");
    }
    guard.close_unless_upgraded();
}
