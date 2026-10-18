//! Очистка при выходе из приложения (поведение Charles).
//!
//! На macOS GPUI завершает процесс через `NSApp terminate:` и НЕ возвращается
//! из `Application::run()`, поэтому код «после run()» недостижим. Единственное
//! надёжное место — `cx.on_app_quit`. Его future получает лишь
//! `SHUTDOWN_TIMEOUT` (200 мс), так что работа делается синхронно в самом
//! колбэке (блокирующе, с собственным таймаутом), а future возвращается готовым.

use std::time::Duration;

use crate::app_state::AppState;
use crate::runtime::EngineRuntime;
use crate::ui::system_proxy::ProxyIndicator;

/// Сколько ждём `networksetup` при выходе (дольше нельзя — пользователь ждёт).
const PROXY_CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);

// Выход: Cmd+Q и пункт меню Exit.
gpui_kit::actions!(crabtrap, [Quit]);

/// Синхронная очистка: снять системный прокси (если он наш) и погасить движок.
/// Вызывается из `on_app_quit` на главном потоке.
pub fn run_quit_cleanup(
    state: &AppState,
    runtime: &tokio::runtime::Handle,
    engine: Option<EngineRuntime>,
) {
    disable_system_proxy_if_ours(state, runtime);
    if let Some(engine) = engine {
        engine.shutdown();
    }
}

fn disable_system_proxy_if_ours(state: &AppState, runtime: &tokio::runtime::Handle) {
    let svc = state.system_proxy_service.clone();
    let listen_addr = state.config.proxy.listen_addr.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    runtime.spawn(async move {
        disable_proxy_if_ours(svc, &listen_addr).await;
        let _ = tx.send(());
    });
    if rx.recv_timeout(PROXY_CLEANUP_TIMEOUT).is_err() {
        tracing::warn!("system proxy cleanup timed out on quit");
    }
}

/// Снять системный прокси, если в нём прописан наш листенер (активно или
/// «stale»); чужой прокси не трогаем.
async fn disable_proxy_if_ours(
    svc: std::sync::Arc<dyn application::services::SystemProxyService>,
    listen_addr: &str,
) {
    match svc.status().await {
        Ok(status) if ProxyIndicator::of(&status, listen_addr).ours_anywhere() => {
            match svc.disable().await {
                Ok(()) => tracing::info!("system proxy disabled on exit"),
                Err(e) => tracing::warn!(error = %e, "cannot disable system proxy on exit"),
            }
        }
        Ok(status) if status.enabled => tracing::warn!(
            server = ?status.server,
            "system proxy enabled but points elsewhere, leaving as is"
        ),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "cannot read system proxy status on exit"),
    }
}

/// Сколько ждём штатного выхода через главный поток после сигнала.
const SIGNAL_QUIT_GRACE: Duration = Duration::from_secs(5);

/// SIGTERM / SIGINT / SIGHUP (kill, Ctrl-C в терминале, остановка из IDE):
/// GPUI их не ловит и `on_app_quit` не вызывается — без этого системный
/// прокси оставался бы указывать на мёртвый порт и у машины пропадал
/// интернет. Сигнал → `quit_tx` → главный поток зовёт `cx.quit()` (штатная
/// очистка в `on_app_quit`, движок гасится). Если главный поток не ответил
/// за [`SIGNAL_QUIT_GRACE`] — аварийно: снять прокси и `exit`.
pub fn install_signal_cleanup(
    state: &AppState,
    runtime: &tokio::runtime::Handle,
    quit_tx: async_channel::Sender<()>,
) {
    use tokio::signal::unix::{SignalKind, signal};
    let svc = state.system_proxy_service.clone();
    let listen_addr = state.config.proxy.listen_addr.clone();
    runtime.spawn(async move {
        let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
            signal(SignalKind::hangup()),
        ) else {
            tracing::warn!("cannot install signal handlers; system proxy cleanup on kill disabled");
            return;
        };
        let name = tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = int.recv() => "SIGINT",
            _ = hup.recv() => "SIGHUP",
        };
        tracing::info!(signal = name, "terminating: quitting via main thread");
        let _ = quit_tx.send(()).await;
        tokio::time::sleep(SIGNAL_QUIT_GRACE).await;
        tracing::warn!("main thread did not quit in time, forcing exit");
        let _ = tokio::time::timeout(
            PROXY_CLEANUP_TIMEOUT,
            disable_proxy_if_ours(svc, &listen_addr),
        )
        .await;
        std::process::exit(0);
    });
}
