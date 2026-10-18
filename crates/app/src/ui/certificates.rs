//! Экран сертификатов (Phase 3): статус CA, доверие, регенерация,
//! подсказка `http://<mac-ip>:<port>/ssl`.
//!
//! Действия уходит через use cases; исполнение — на tokio-хендле движка
//! (cert-сервис использует tokio::process/spawn_blocking), результат
//! возвращается в gpui через async-channel (D4) и применяется в Entity.

use std::sync::Arc;

use application::dto::certificates::{CaStatusDto, TrustStatusDto};
use application::services::CertificateService;
use application::use_cases::{GetCaStatusUseCase, InstallTrustUseCase, RegenerateCaUseCase};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled, Window, div, px,
};

use crate::app_state::ListenAddr;
use crate::ui::network::{lan_ip, listen_port};

/// LAN-адрес страницы раздачи: `http://<lan-ip>:<port>/ssl`.
///
/// IP определяется на каждый вызов (DHCP плавает); fallback — loopback.
/// Порт берётся из адреса листенера движка.
pub fn ssl_lan_url(listen_addr: &str) -> String {
    let ip = lan_ip().unwrap_or_else(|| "127.0.0.1".to_string());
    format!("http://{ip}:{}/ssl", listen_port(listen_addr))
}

/// Состояние экрана сертификатов.
pub struct CertificatesStore {
    runtime: tokio::runtime::Handle,
    service: Arc<dyn CertificateService>,
    pub ca: Option<CaStatusDto>,
    pub trust: Option<TrustStatusDto>,
    pub busy: bool,
    pub error: Option<String>,
    /// Инлайн-подтверждение регенерации (двухшаговая кнопка).
    pub confirm_regenerate: bool,
    /// Адрес листенера движка (порт для LAN-URL; хост меняет Settings → Network).
    pub listen_addr: ListenAddr,
    /// Текущий LAN-URL раздачи (пересчитывается на каждом открытии экрана).
    pub ssl_url: String,
    /// Selectable-поле с URL (Input readonly).
    pub url_input: Entity<InputState>,
}

impl CertificatesStore {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        runtime: tokio::runtime::Handle,
        service: Arc<dyn CertificateService>,
        listen_addr: ListenAddr,
    ) -> Self {
        let ssl_url = ssl_lan_url(&listen_addr.get());
        let url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(ssl_url.clone())
                .placeholder("http://<lan-ip>:<port>/ssl")
        });
        Self {
            runtime,
            service,
            ca: None,
            trust: None,
            busy: false,
            error: None,
            confirm_regenerate: false,
            listen_addr,
            ssl_url,
            url_input,
        }
    }

    /// Первичная загрузка (вызывается при открытии экрана/Refresh): статус +
    /// пересчёт LAN-URL (IP мог уплыть по DHCP).
    pub fn refresh(entity: &Entity<Self>, cx: &mut App) {
        let (service, runtime) = {
            let s = entity.read(cx);
            (s.service.clone(), s.runtime.clone())
        };
        run_cert_task(
            cx,
            entity,
            runtime,
            async move {
                let uc = GetCaStatusUseCase::new(service.clone());
                let status = uc.execute().await?;
                let trust = CertificateService::trust_status(&*service).await?;
                Ok((status, trust))
            },
            |s, (status, trust)| {
                s.ca = Some(status);
                s.trust = Some(trust);
            },
        );
        // LAN-IP: синхронный UDP-connect трюк (без реального сетевого IO),
        // поэтому пересчёт прямо здесь, а не в tokio-задаче.
        entity.update(cx, |s, cx| {
            let url = ssl_lan_url(&s.listen_addr.get());
            if url != s.ssl_url {
                s.ssl_url = url;
                cx.notify();
            }
        });
    }

    /// Установка доверия (GUI-промпт пароля администратора macOS).
    pub fn install_trust(entity: &Entity<Self>, cx: &mut App) {
        let (service, runtime) = {
            let s = entity.read(cx);
            (s.service.clone(), s.runtime.clone())
        };
        run_cert_task(
            cx,
            entity,
            runtime,
            async move { InstallTrustUseCase::new(service).execute().await },
            |s, trust| {
                s.trust = Some(trust);
            },
        );
    }

    /// Регенерация CA (новый fingerprint, доверие сбрасывается).
    pub fn regenerate(entity: &Entity<Self>, cx: &mut App) {
        let (service, runtime) = {
            let s = entity.read(cx);
            (s.service.clone(), s.runtime.clone())
        };
        run_cert_task(
            cx,
            entity,
            runtime,
            async move { RegenerateCaUseCase::new(service).execute().await },
            |s, status| {
                s.ca = Some(status);
                s.confirm_regenerate = false;
            },
        );
    }
}

/// Мост tokio → gpui: future исполняется на tokio-хендле, результат
/// приезжает через async-channel и применяется в Entity (D4: никакого
/// tokio-IO в gpui-тасках — только ожидание канала).
fn run_cert_task<T, F>(
    cx: &mut App,
    entity: &Entity<CertificatesStore>,
    runtime: tokio::runtime::Handle,
    fut: F,
    apply: impl FnOnce(&mut CertificatesStore, T) + Send + 'static,
) where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, domain::errors::DomainError>> + Send + 'static,
{
    entity.update(cx, |s, _| {
        s.busy = true;
        s.error = None;
    });
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
            entity.update(app, |s, cx| {
                s.busy = false;
                match result {
                    Ok(value) => apply(s, value),
                    Err(e) => s.error = Some(e.to_string()),
                }
                cx.notify();
            });
        });
    })
    .detach();
}

/// Панель «Certificates» (RenderOnce над Entity-состоянием).
#[derive(gpui_kit::IntoElement)]
pub struct CertificatePanel {
    store: Entity<CertificatesStore>,
}

impl CertificatePanel {
    pub fn new(store: Entity<CertificatesStore>) -> Self {
        Self { store }
    }
}

impl RenderOnce for CertificatePanel {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (ca, trust, busy, error, confirm, ssl_url, url_input, listen_addr) = {
            let s = self.store.read(cx);
            (
                s.ca.clone(),
                s.trust.clone(),
                s.busy,
                s.error.clone(),
                s.confirm_regenerate,
                s.ssl_url.clone(),
                s.url_input.clone(),
                s.listen_addr.get(),
            )
        };
        // URL в selectable-поле синхронизируем на рендере: set_value не
        // эмитит InputEvent::Change, цикла перерисовок нет.
        if url_input.read(cx).value() != ssl_url.as_str() {
            url_input.update(cx, |input, cx| {
                input.set_value(ssl_url.clone(), window, cx);
            });
        }

        let fingerprint = ca
            .as_ref()
            .and_then(|c| c.fingerprint.clone())
            .unwrap_or_else(|| "—".to_string());
        let created = ca
            .as_ref()
            .and_then(|c| {
                c.created_at
                    .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
            })
            .unwrap_or_else(|| "—".to_string());
        let cert_path = ca
            .as_ref()
            .map(|c| c.cert_path.display().to_string())
            .unwrap_or_else(|| "—".to_string());
        let trust_line = match &trust {
            Some(t) if t.trusted => {
                format!("Trusted ({})", t.keychain.as_deref().unwrap_or("keychain"))
            }
            Some(_) => "Not trusted".to_string(),
            None => "Unknown".to_string(),
        };
        let ssl_url_for_copy = ssl_url.clone();

        let store_for_install = self.store.clone();
        let store_for_regen = self.store.clone();
        let store_for_confirm = self.store.clone();
        let store_for_cancel = self.store.clone();
        let store_for_refresh = self.store.clone();

        let mut column =
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_6()
                .max_w(px(720.))
                .text_color(theme.foreground)
                .child(row("CA fingerprint (SHA-256)", fingerprint))
                .child(row("Created", created))
                .child(row("CA path", cert_path))
                .child(row("Trust status", trust_line))
                // QR + selectable URL (Phase 4): телефон сканирует и качает PEM.
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_4()
                        .py_1()
                        .child(qr_grid(&ssl_url))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .child("Device setup page (scan or open on the device):"),
                                )
                                .child(Input::new(&url_input).readonly(true).h(px(28.)).w(px(300.)))
                                .child(
                                    Button::new("cert-copy-url")
                                        .label("Copy URL")
                                        .secondary()
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(
                                                gpui_kit::ClipboardItem::new_string(
                                                    ssl_url_for_copy.clone(),
                                                ),
                                            );
                                        }),
                                ),
                        ),
                )
                .when(!config::lan_access(&listen_addr), |this| {
                    this.child(div().text_size(px(12.)).text_color(theme.danger).child(format!(
                    "The proxy listens on {listen_addr}, so only this Mac can reach it. \
                     To set up a phone, turn on Settings → Network → Allow devices on the \
                     local network."
                )))
                })
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .mt_2()
                        .child(
                            Button::new("cert-install-trust")
                                .label("Install Trust")
                                .primary()
                                .disabled(busy)
                                .on_click(move |_, _, cx| {
                                    let store = store_for_install.clone();
                                    CertificatesStore::install_trust(&store, cx);
                                }),
                        )
                        .child(regen_button(store_for_regen, busy, confirm))
                        .child(
                            Button::new("cert-refresh")
                                .label("Refresh")
                                .secondary()
                                .disabled(busy)
                                .on_click(move |_, _, cx| {
                                    let store = store_for_refresh.clone();
                                    CertificatesStore::refresh(&store, cx);
                                }),
                        ),
                );

        if let Some(err) = error {
            column = column.child(
                div()
                    .text_color(theme.danger)
                    .text_size(px(12.))
                    .child(format!("error: {err}")),
            );
        }
        if confirm {
            column = column.child(
                div()
                    .mt_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_size(px(12.))
                    .text_color(theme.danger)
                    .child("Regenerate creates a NEW CA. Continue?")
                    .child(
                        Button::new("cert-regen-confirm-yes")
                            .label("Yes, regenerate")
                            .danger()
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                let store = store_for_confirm.clone();
                                store.update(cx, |s, _| s.confirm_regenerate = false);
                                CertificatesStore::regenerate(&store, cx);
                            }),
                    )
                    .child(
                        Button::new("cert-regen-confirm-no")
                            .label("Cancel")
                            .secondary()
                            .on_click(move |_, _, cx| {
                                let store = store_for_cancel.clone();
                                store.update(cx, |s, cx| {
                                    s.confirm_regenerate = false;
                                    cx.notify();
                                });
                            }),
                    ),
            );
        }
        if busy {
            column = column.child(div().text_size(px(12.)).child("working…"));
        }
        column
            .child(
                div()
                    .mt_1()
                    .text_size(px(11.))
                    .text_color(theme.foreground.opacity(0.6))
                    .child(
                        "Install Trust asks for the macOS administrator password (GUI prompt). \
                         Regenerate CA invalidates all MITM certificates: the new CA must be \
                         trusted again (Install Trust) and re-installed on devices; the old CA \
                         stays in Keychain until you delete it.",
                    ),
            )
            .child(
                div()
                    .mt_1()
                    .text_size(px(11.))
                    .text_color(theme.foreground.opacity(0.6))
                    .child(
                        "On-device instructions: scan the QR (or open the URL on the device, \
                         same network), download ca.pem and follow the steps. iOS: profile → \
                         Settings → General → About → Certificate Trust Settings.",
                    ),
            )
    }
}

/// QR-код URL: модули qrcode-крейта рисуем div-гридом (Phase 4).
///
/// Тёмные модули — цветом текста темы, светлые — фон (quiet zone — отступ
/// контейнера). Размер модуля фиксирован: URL ~25×25 модулей → ~112 px.
fn qr_grid(url: &str) -> gpui_kit::Div {
    const MODULE: f32 = 4.0;
    const QUIET: f32 = 12.0;
    let (width, dark): (usize, Vec<bool>) = match qrcode::QrCode::new(url) {
        Ok(code) => (code.width(), {
            let colors = code.to_colors();
            colors
                .into_iter()
                .map(|c| matches!(c, qrcode::Color::Dark))
                .collect()
        }),
        Err(e) => {
            tracing::warn!(error = %e, url, "QR encode failed");
            (0, Vec::new())
        }
    };
    let mut grid = div()
        .p(px(QUIET))
        .bg(gpui_kit::white())
        .rounded(px(4.))
        .flex()
        .flex_col()
        .overflow_hidden();
    if width == 0 {
        return grid.child(div().w(px(100.)).h(px(24.)));
    }
    for y in 0..width {
        let mut row = div().flex().flex_row();
        for x in 0..width {
            let cell = div().w(px(MODULE)).h(px(MODULE));
            row = row.child(if dark[y * width + x] {
                cell.bg(gpui_kit::black())
            } else {
                cell.bg(gpui_kit::white())
            });
        }
        grid = grid.child(row);
    }
    grid
}

fn regen_button(store: Entity<CertificatesStore>, busy: bool, confirm: bool) -> Button {
    let mut btn = Button::new("cert-regenerate")
        .label("Regenerate CA")
        .danger();
    if !busy && !confirm {
        btn = btn.on_click(move |_, _, cx| {
            store.update(cx, |s, cx| {
                s.confirm_regenerate = true;
                cx.notify();
            });
        });
    } else {
        btn = btn.disabled(true);
    }
    btn
}

fn row(label: &str, value: String) -> gpui_kit::Div {
    div()
        .flex()
        .flex_row()
        .gap_2()
        .text_size(px(12.))
        .child(
            div()
                .w(px(180.))
                .flex_shrink_0()
                .child(SharedString::from(format!("{label}:"))),
        )
        .child(div().child(SharedString::from(value)))
}
