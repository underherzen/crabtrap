//! Вкладка «Messages» (Phase 11b): сообщения WebSocket-соединения —
//! направление (↑ отправлено клиентом, ↓ получено), время, тип, размер и
//! содержимое (текст — как есть, двоичное — первые байты в hex).

use domain::entities::websocket::{WebSocketMessage, WsDirection, WsOpcode};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::v_flex;
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement as _, Styled, Window, div, px,
};

use super::ExchangeContext;
use crate::ui::theme::metrics::TEXT_XS;
use crate::ui::theme::{Palette, ToneName, glass};

/// Сколько текста сообщения показывать в строке (полное — копированием
/// через Raw/экспорт позже).
const SHOWN_CHARS: usize = 4096;
/// Байт двоичного сообщения в hex-превью.
const HEX_PREVIEW: usize = 64;
/// Сколько последних сообщений рисовать (без виртуализации).
const SHOWN_MESSAGES: usize = 300;

#[derive(IntoElement)]
pub struct MessagesTab {
    ctx: ExchangeContext,
}

impl MessagesTab {
    pub fn new(ctx: ExchangeContext) -> Self {
        Self { ctx }
    }
}

/// Содержимое сообщения для показа.
pub fn message_text(message: &WebSocketMessage) -> String {
    let mut text = match message.opcode {
        WsOpcode::Text | WsOpcode::Close => {
            let text = String::from_utf8_lossy(&message.payload);
            match text.char_indices().nth(SHOWN_CHARS) {
                Some((cut, _)) => format!("{}…", &text[..cut]),
                None => text.into_owned(),
            }
        }
        _ => message.payload[..message.payload.len().min(HEX_PREVIEW)]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" "),
    };
    if message.truncated() {
        text.push_str(&format!("  … ({} bytes in total)", message.size));
    }
    text
}

fn opcode_label(opcode: WsOpcode) -> &'static str {
    match opcode {
        WsOpcode::Text => "text",
        WsOpcode::Binary => "binary",
        WsOpcode::Close => "close",
        WsOpcode::Ping => "ping",
        WsOpcode::Pong => "pong",
    }
}

fn row(
    ix: usize,
    message: &WebSocketMessage,
    mono: gpui_kit::SharedString,
    p: &Palette,
) -> AnyElement {
    let (arrow, tone) = match message.direction {
        WsDirection::Sent => ("↑", ToneName::Blue),
        WsDirection::Received => ("↓", ToneName::Green),
    };
    let meta = format!(
        "{}  {}  {} B",
        message
            .at
            .with_timezone(&chrono::Local)
            .format("%H:%M:%S%.3f"),
        opcode_label(message.opcode),
        message.size
    );
    div()
        .id(("ws-message", ix))
        .flex_none()
        .flex()
        .gap(px(8.))
        .px(px(8.))
        .py(px(6.))
        .border_b_1()
        .border_color(p.hairline)
        .child(
            div()
                .flex_none()
                .w(px(14.))
                .text_color(p.tone(tone).fg)
                .child(arrow),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(TEXT_XS))
                        .text_color(p.text_tertiary)
                        .child(meta),
                )
                .child(
                    div()
                        .font_family(mono)
                        .text_size(px(12.))
                        .child(message_text(message)),
                ),
        )
        .into_any_element()
}

impl RenderOnce for MessagesTab {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let mono = cx.theme().mono_font_family.clone();
        let messages = &self.ctx.websocket;
        if messages.is_empty() {
            return super::placeholder("No messages yet", &p).into_any_element();
        }
        let sent = messages
            .iter()
            .filter(|m| m.direction == WsDirection::Sent)
            .count();
        let skipped = messages.len().saturating_sub(SHOWN_MESSAGES);
        let mut summary = format!(
            "{} messages — {} sent, {} received",
            messages.len(),
            sent,
            messages.len() - sent
        );
        if skipped > 0 {
            summary.push_str(&format!(" · showing the last {SHOWN_MESSAGES}"));
        }
        v_flex()
            .id("messages-tab")
            .size_full()
            .overflow_y_scroll()
            .p(px(10.))
            .gap(px(6.))
            .child(
                div()
                    .flex_none()
                    .text_size(px(TEXT_XS))
                    .text_color(p.text_tertiary)
                    .child(summary),
            )
            .child(
                v_flex().flex_none().children(
                    messages
                        .iter()
                        .enumerate()
                        .skip(skipped)
                        .map(|(ix, m)| row(ix, m, mono.clone(), &p)),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn message(opcode: WsOpcode, payload: &[u8], size: u64) -> WebSocketMessage {
        WebSocketMessage {
            direction: WsDirection::Sent,
            opcode,
            payload: payload.to_vec(),
            size,
            at: Utc::now(),
        }
    }

    #[test]
    fn text_binary_and_truncated_messages() {
        assert_eq!(message_text(&message(WsOpcode::Text, b"hi", 2)), "hi");
        assert_eq!(
            message_text(&message(WsOpcode::Binary, &[0, 255], 2)),
            "00 ff"
        );
        assert!(message_text(&message(WsOpcode::Text, b"x", 10)).ends_with("(10 bytes in total)"));
    }
}
