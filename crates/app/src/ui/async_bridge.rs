//! Мост tokio → gpui (решение D4), generic-версия паттерна из
//! `certificates.rs::run_cert_task`.
//!
//! Future исполняется на tokio-хендле движка (никакого tokio-IO в
//! gpui-тасках), результат приезжает через `async-channel` и применяется
//! в Entity. Используется DetailPane и (Phase 5) вкладками инспектора для
//! ленивой загрузки/декодирования тел.

use gpui_kit::{App, Context, Entity};

/// Запустить future на движковом рантайме и применить результат в Entity.
///
/// `apply` получает `Result<T, DomainError>` и сам решает, что писать в
/// состояние (включая ошибку/busy-флаг); после применения вызывает
/// `cx.notify()` неявно НЕ делает — вызовите внутри `apply`, если нужно.
pub fn spawn_ui_task<S, T, F, A>(
    cx: &mut App,
    entity: &Entity<S>,
    runtime: tokio::runtime::Handle,
    fut: F,
    apply: A,
) where
    S: 'static,
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, domain::errors::DomainError>> + Send + 'static,
    A: FnOnce(&mut S, Result<T, domain::errors::DomainError>, &mut Context<S>) + Send + 'static,
{
    let (tx, rx) = async_channel::bounded(1);
    runtime.spawn(async move {
        let _ = tx.send(fut.await).await;
    });
    let entity = entity.clone();
    cx.spawn(async move |cx| {
        let Ok(result) = rx.recv().await else {
            return;
        };
        cx.update(|app| {
            entity.update(app, |state, cx| apply(state, result, cx));
        });
    })
    .detach();
}
