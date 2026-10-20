//! Короткие уведомления в активном окне для действий без своего окна
//! (контекстное меню, строка меню).

use gpui_kit::App;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;

/// Показать уведомление в активном окне (нет окна — только в лог).
pub fn notify(notification: Notification, cx: &mut App) {
    let Some(handle) = cx.active_window() else {
        tracing::info!("notification without an active window dropped");
        return;
    };
    let _ = handle.update(cx, |_, window, cx| {
        window.push_notification(notification.autohide(true), cx);
    });
}

/// Ошибка действия: в лог и уведомлением.
pub fn notify_error(message: String, cx: &mut App) {
    tracing::warn!(%message, "action failed");
    notify(Notification::error(message), cx);
}
