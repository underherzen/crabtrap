//! Вкладка «Overview» (Phase 5): общий блок метаданных обмена, TLS-блок
//! и горизонтальная стек-диаграмма фаз тайминга.
//!
//! Чистые функции (URL/authority/веса сегментов/размеры) вынесены на уровень
//! модуля и покрыты тестами ниже; рендер — только компоновка `div`-ов.

use super::kv::Selectable;
use application::dto::inspection::canonical_reason;
use domain::entities::body::BodyRef;
use domain::value_objects::http_vo::Protocol;
use domain::value_objects::timing::TimingPhases;
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Hsla, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement as _, Styled, Window, div, px,
};

use super::ExchangeContext;
use crate::ui::group_box::GroupBox;
use crate::ui::theme::components::tone_pill;
use crate::ui::theme::semantic::tag_tone;
use crate::ui::theme::{Palette, ToneName};
use crate::ui::traffic::table::{human_bytes, status_color};

/// Метка фазы тайминга (порядок = порядок сегментов бара и легенды).
const PHASES: [&str; 6] = ["dns", "connect", "tls", "send", "wait", "receive"];

/// Пол видимой ширины бара для ненулевых, но крошечных фаз (иначе слайд
/// в 0.1 мс не виден на фоне wait в 500 мс).
const PHASE_MIN_WEIGHT: f64 = 0.02;

/// Полный URL обмена: `scheme://host` + path (summary.path уже включает query).
pub fn full_url(scheme: &str, host: &str, path: &str) -> String {
    if path.starts_with('/') {
        format!("{scheme}://{host}{path}")
    } else {
        format!("{scheme}://{host}/{path}")
    }
}

/// Authority удалённого узла (host[:port]): из абсолютного URI запроса
/// (прокси-запросы приходят в absolute-form, порт виден только там);
/// origin-form → нормализованный host из summary (порт неизвестен).
pub fn remote_authority(uri: &str, host: &str) -> String {
    uri.split_once("://")
        .map(|(_, rest)| {
            let end = rest.find('/').unwrap_or(rest.len());
            rest[..end].to_string()
        })
        .filter(|authority| !authority.is_empty())
        .unwrap_or_else(|| host.to_string())
}

/// Человекочитаемая метка протокола (как в колонке Type таблицы трафика).
pub fn protocol_label(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Http1 => "http1.1",
        Protocol::Http2 => "h2",
        Protocol::WebSocket => "ws",
        Protocol::Tunnel => "tunnel",
    }
}

/// Разбивка размера сообщения: (заголовки, тело) из raw_size и BodyRef.
///
/// Тело может отсутствовать (тогда всё — заголовки); size тела не превосходит
/// raw_size, но защищаемся saturating_sub на случай рассинхрона.
pub fn split_sizes(raw_size: u64, body: Option<&BodyRef>) -> (u64, u64) {
    let body_size = body.map(|b| b.size).unwrap_or(0);
    (raw_size.saturating_sub(body_size), body_size)
}

/// Сегмент диаграммы таймингов: метка фазы, длительность и вес (доля бара).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimingSegment {
    pub label: &'static str,
    pub ms: f64,
    /// Доля ширины бара; 0 — фазу не рисуем. Ненулевые фазы не уже пола
    /// [`PHASE_MIN_WEIGHT`].
    pub weight: f64,
}

/// Сегменты бара: 6 фаз в фиксированном порядке, масштаб к `or_summed_total`
/// (total_ms движка, иначе сумма фаз).
pub fn timing_segments(phases: &TimingPhases) -> Vec<TimingSegment> {
    let values = [
        phases.dns_ms,
        phases.connect_ms,
        phases.tls_ms,
        phases.send_ms,
        phases.wait_ms,
        phases.receive_ms,
    ];
    let total = phases.or_summed_total();
    PHASES
        .iter()
        .zip(values)
        .map(|(&label, ms)| TimingSegment {
            label,
            ms,
            weight: weight_of(ms, total),
        })
        .collect()
}

/// Вес одной фазы: 0 для нулевых фаз и нулевого total, иначе `ms/total`
/// с полом [`PHASE_MIN_WEIGHT`].
fn weight_of(ms: f64, total: f64) -> f64 {
    if total <= 0.0 || ms <= 0.0 {
        0.0
    } else {
        (ms / total).max(PHASE_MIN_WEIGHT)
    }
}

/// Цвет сегмента фазы: dns · connect · tls · send · wait · receive — тона
/// палитры (различимы в обеих темах).
fn phase_color(ix: usize, p: &Palette) -> Hsla {
    const PHASES: [ToneName; 6] = [
        ToneName::Teal,
        ToneName::Blue,
        ToneName::Purple,
        ToneName::Orange,
        ToneName::Green,
        ToneName::Pink,
    ];
    p.tone(PHASES[ix.min(PHASES.len() - 1)]).fg
}

/// Строка «label | value» двухколоночной сетки: фиксированная колонка меток,
/// значение занимает остаток и переносится по словам. Оба текста выделяются
/// мышью (id строки — метка, она уникальна в пределах вкладки).
fn kv_row(
    label: &str,
    value: impl Into<gpui_kit::SharedString>,
    label_color: Hsla,
    value_color: Hsla,
) -> gpui_kit::Div {
    h_flex()
        .items_start()
        .gap_2()
        .child(
            div()
                .w(px(110.))
                .flex_none()
                .pt(px(1.))
                .text_size(px(11.))
                .text_color(label_color)
                .cursor_text()
                .child(Selectable::new(
                    gpui_kit::SharedString::from(format!("overview-label-{label}")),
                    label.to_owned(),
                )),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(12.))
                .text_color(value_color)
                .cursor_text()
                .child(Selectable::new(
                    gpui_kit::SharedString::from(format!("overview-value-{label}")),
                    value,
                )),
        )
}

/// Секция-карточка (General / TLS / Timing).
fn block(title: &str, border: Hsla, muted: Hsla) -> GroupBox {
    GroupBox::new(title, border, muted)
}

/// Overview: сводка + TLS + тайминги.
#[derive(gpui_kit::IntoElement)]
pub struct OverviewTab {
    ctx: ExchangeContext,
}

impl OverviewTab {
    pub fn new(ctx: ExchangeContext) -> Self {
        Self { ctx }
    }
}

impl RenderOnce for OverviewTab {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = crate::ui::theme::glass(cx);
        let theme = cx.theme();
        let d = &*self.ctx.detail;
        let s = &d.summary;
        let muted = theme.foreground.opacity(0.55);
        let failed = d.note.as_deref().is_some_and(|n| n.starts_with("failed:"));

        // --- General -----------------------------------------------------
        let (req_hdr, req_body) = split_sizes(d.request.raw_size, d.request.body.as_ref());
        let resp_sizes = d
            .response
            .as_ref()
            .map(|r| split_sizes(r.raw_size, r.body.as_ref()));
        let status_value = if failed {
            "failed".to_string()
        } else {
            match (&d.response, s.status) {
                (Some(r), Some(status)) => match canonical_reason(status).or(r.reason.as_deref()) {
                    Some(reason) => format!("{status} {reason}"),
                    None => status.to_string(),
                },
                _ => "pending…".to_string(),
            }
        };
        let duration = d.timing.or_summed_total();

        let mut general = block("General", theme.border, muted)
            .child(kv_row(
                "URL",
                full_url(&s.scheme, &s.host, &s.path),
                muted,
                theme.foreground,
            ))
            .child(kv_row("Method", s.method.clone(), muted, theme.foreground))
            .child(kv_row(
                "Status",
                status_value,
                muted,
                status_color(s.status, failed, &palette),
            ))
            .child(kv_row(
                "HTTP version",
                d.request.version.to_string(),
                muted,
                theme.foreground,
            ))
            .child(kv_row(
                "Remote",
                remote_authority(&d.request.uri, &s.host),
                muted,
                theme.foreground,
            ))
            .child(kv_row(
                "Duration",
                if duration > 0.0 {
                    format!("{duration:.1} ms")
                } else {
                    "…".to_string()
                },
                muted,
                theme.foreground,
            ))
            .child(kv_row(
                "Request size",
                format!(
                    "{} (headers {}, body {})",
                    human_bytes(d.request.raw_size),
                    human_bytes(req_hdr),
                    human_bytes(req_body)
                ),
                muted,
                theme.foreground,
            ))
            .child(kv_row(
                "Response size",
                match &resp_sizes {
                    Some((hdr, body)) => format!(
                        "{} (headers {}, body {})",
                        human_bytes(d.response.as_ref().map(|r| r.raw_size).unwrap_or(0)),
                        human_bytes(*hdr),
                        human_bytes(*body)
                    ),
                    None => "pending…".to_string(),
                },
                muted,
                theme.foreground,
            ))
            .child(kv_row(
                "Protocol",
                protocol_label(s.protocol),
                muted,
                theme.foreground,
            ));

        if !s.tags.is_empty() {
            let chips = h_flex().flex_wrap().gap_1().children(
                s.tags
                    .iter()
                    .map(|tag| tone_pill(tag.clone(), palette.tone(tag_tone(tag)))),
            );
            general = general.child(
                h_flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(muted)
                            .child("Tags"),
                    )
                    .child(chips),
            );
        }

        if let Some(note) = &d.note {
            let (text, color) = if let Some(reason) = note.strip_prefix("failed:") {
                (reason, theme.danger)
            } else {
                (note.as_str(), theme.foreground.opacity(0.75))
            };
            general = general.child(kv_row("Note", text, muted, color));
        }

        // --- TLS ---------------------------------------------------------
        fn dash(v: Option<&str>) -> &str {
            v.unwrap_or("—")
        }
        let tls_block = d.tls.as_ref().map(|tls| {
            block("TLS", theme.border, muted)
                .child(kv_row(
                    "SNI",
                    dash(tls.sni.as_deref()),
                    muted,
                    theme.foreground,
                ))
                .child(kv_row(
                    "Version",
                    dash(tls.version.as_deref()),
                    muted,
                    theme.foreground,
                ))
                .child(kv_row(
                    "Cipher suite",
                    dash(tls.cipher_suite.as_deref()),
                    muted,
                    theme.foreground,
                ))
                .child(kv_row(
                    "ALPN",
                    dash(tls.alpn.as_deref()),
                    muted,
                    theme.foreground,
                ))
                .child(kv_row(
                    "MITM",
                    if tls.mitm { "yes" } else { "no" },
                    muted,
                    theme.foreground,
                ))
                .child(kv_row(
                    "Peer subject",
                    dash(tls.peer_subject.as_deref()),
                    muted,
                    theme.foreground,
                ))
                .child(kv_row(
                    "Cert valid until",
                    tls.leaf_not_after
                        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                        .unwrap_or_else(|| "—".to_string()),
                    muted,
                    theme.foreground,
                ))
        });

        // --- Timing ------------------------------------------------------
        let timing_block = if duration <= 0.0 {
            block("Timing", theme.border, muted).child(
                div()
                    .text_size(px(12.))
                    .text_color(muted)
                    .child("no timing data"),
            )
        } else {
            let segs = timing_segments(&d.timing);
            let bar = h_flex()
                .h(px(12.))
                .w_full()
                .rounded_sm()
                .overflow_hidden()
                .children(
                    segs.iter()
                        .enumerate()
                        .filter(|(_, seg)| seg.weight > 0.0)
                        .map(|(ix, seg)| {
                            div()
                                .h_full()
                                .flex_grow(seg.weight as f32)
                                .bg(phase_color(ix, &palette))
                        }),
                );
            let legend =
                h_flex()
                    .flex_wrap()
                    .gap_x_3()
                    .gap_y_1()
                    .children(segs.iter().enumerate().map(|(ix, seg)| {
                        let color = if seg.ms > 0.0 {
                            muted
                        } else {
                            theme.foreground.opacity(0.3)
                        };
                        h_flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .size(px(8.))
                                    .rounded_full()
                                    .bg(phase_color(ix, &palette)),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(color)
                                    .child(format!("{} {:.1} ms", seg.label, seg.ms)),
                            )
                    }));
            block("Timing", theme.border, muted)
                .child(bar)
                .child(legend)
        };

        div()
            .id("overview-tab")
            .size_full()
            .overflow_y_scroll()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(general)
            .when_some(tls_block, |this, tls| this.child(tls))
            .child(timing_block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_url_joins_path_variants() {
        assert_eq!(
            full_url("https", "example.com", "/a/b?x=1"),
            "https://example.com/a/b?x=1"
        );
        assert_eq!(full_url("http", "example.com", "a"), "http://example.com/a");
        assert_eq!(full_url("http", "example.com", ""), "http://example.com/");
    }

    #[test]
    fn remote_authority_prefers_absolute_uri() {
        assert_eq!(
            remote_authority("http://example.com:8080/x?y=1", "example.com"),
            "example.com:8080"
        );
        assert_eq!(
            remote_authority("https://example.com/", "example.com"),
            "example.com"
        );
        // origin-form (MITM-ветка): порта в URI нет — host из summary.
        assert_eq!(
            remote_authority("/index.html", "example.com"),
            "example.com"
        );
        // Пустой authority (URI оборван) — не показываем, fallback на host.
        assert_eq!(remote_authority("http://", "fallback"), "fallback");
    }

    #[test]
    fn protocol_labels_cover_all_variants() {
        assert_eq!(protocol_label(Protocol::Http1), "http1.1");
        assert_eq!(protocol_label(Protocol::Http2), "h2");
        assert_eq!(protocol_label(Protocol::WebSocket), "ws");
        assert_eq!(protocol_label(Protocol::Tunnel), "tunnel");
    }

    #[test]
    fn split_sizes_headers_plus_body() {
        let body = BodyRef {
            id: Default::default(),
            size: 300,
            content_type: None,
            encoding: None,
            storage: Default::default(),
        };
        assert_eq!(split_sizes(1024, Some(&body)), (724, 300));
        assert_eq!(split_sizes(1024, None), (1024, 0));
        // Тело больше raw (рассинхрон) → заголовки 0, а не underflow.
        assert_eq!(split_sizes(10, Some(&body)), (0, 300));
    }

    #[test]
    fn timing_segments_proportional() {
        let phases = TimingPhases {
            dns_ms: 10.0,
            connect_ms: 20.0,
            tls_ms: 10.0,
            send_ms: 10.0,
            wait_ms: 40.0,
            receive_ms: 10.0,
            total_ms: 100.0,
        };
        let segs = timing_segments(&phases);
        assert_eq!(segs.len(), 6);
        assert_eq!(segs[0].label, "dns");
        assert_eq!(segs[0].weight, 0.1);
        assert_eq!(segs[4].label, "wait");
        assert_eq!(segs[4].weight, 0.4);
    }

    #[test]
    fn timing_segments_floor_and_zeros() {
        let phases = TimingPhases {
            dns_ms: 0.0,       // нулевая фаза — не рисуем
            connect_ms: 0.001, // крошечная — пол 2%
            tls_ms: 0.0,
            send_ms: 0.0,
            wait_ms: 99.999,
            receive_ms: 0.0,
            total_ms: 100.0,
        };
        let segs = timing_segments(&phases);
        assert_eq!(segs[0].weight, 0.0);
        assert_eq!(segs[1].weight, PHASE_MIN_WEIGHT);
        // Веса — флоаты: сравнение с эпсилоном, не на равенство.
        assert!(
            (segs[4].weight - 0.99999).abs() < 1e-6,
            "weight={}",
            segs[4].weight
        );
    }

    #[test]
    fn timing_segments_zero_total_uses_phase_sum() {
        // total_ms не проставлен — берём сумму фаз (or_summed_total).
        let phases = TimingPhases {
            dns_ms: 25.0,
            connect_ms: 25.0,
            tls_ms: 0.0,
            send_ms: 0.0,
            wait_ms: 50.0,
            receive_ms: 0.0,
            total_ms: 0.0,
        };
        let segs = timing_segments(&phases);
        assert_eq!(segs[0].weight, 0.25);
        assert_eq!(segs[4].weight, 0.5);
    }

    #[test]
    fn timing_segments_no_data_all_zero() {
        let phases = TimingPhases::default();
        assert!(timing_segments(&phases).iter().all(|s| s.weight == 0.0));
    }
}
