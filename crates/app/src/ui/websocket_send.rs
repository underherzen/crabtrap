//! Отправка сообщений в открытый WebSocket из вкладки Messages: Resend
//! (то же сообщение ещё раз) и Edit & Send / Send Message (свой текст или
//! hex, к серверу или к клиенту). Движок пишет кадр на границе сообщения
//! (`EngineCommand::SendWebSocketMessage`); неудача приходит событием
//! `WebSocketSendFailed` — уведомлением.

use application::dto::proxy_config::EngineCommand;
use domain::entities::websocket::{WebSocketMessage, WsDirection, WsOpcode};
use domain::value_objects::ExchangeId;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::radio::Radio;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Global, IntoElement, ParentElement, Render, Styled,
    WeakEntity, Window, div, px,
};

use crate::runtime::EngineHandle;
use crate::ui::notify::notify_error;
use crate::ui::rules::match_fields::row;

const DIALOG_WIDTH: f32 = 560.;
const EDITOR_HEIGHT: f32 = 160.;

/// Хендл движка для отправки.
#[derive(Clone)]
struct WsSend(EngineHandle);

impl Global for WsSend {}

pub fn install(engine: EngineHandle, cx: &mut App) {
    cx.set_global(WsSend(engine));
}

/// Начальные значения диалога.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendForm {
    pub direction: WsDirection,
    pub binary: bool,
    /// Текст или hex (`00 ff 1a`).
    pub content: String,
}

impl SendForm {
    /// Пустая форма: текст серверу.
    pub fn blank() -> Self {
        Self {
            direction: WsDirection::Sent,
            binary: false,
            content: String::new(),
        }
    }

    /// Форма из записанного сообщения (Edit & Send).
    pub fn from_message(message: &WebSocketMessage) -> Self {
        let binary = message.opcode == WsOpcode::Binary;
        Self {
            direction: message.direction,
            binary,
            content: match binary {
                true => to_hex(&message.payload),
                false => String::from_utf8_lossy(&message.payload).into_owned(),
            },
        }
    }

    /// Тип и байты сообщения.
    pub fn payload(&self) -> Result<(WsOpcode, Vec<u8>), String> {
        match self.binary {
            true => parse_hex(&self.content).map(|bytes| (WsOpcode::Binary, bytes)),
            false => Ok((WsOpcode::Text, self.content.clone().into_bytes())),
        }
    }
}

/// Можно ли отправить сообщение ещё раз как есть (данные, содержимое не
/// обрезано потолком записи).
pub fn can_resend(message: &WebSocketMessage) -> bool {
    matches!(message.opcode, WsOpcode::Text | WsOpcode::Binary) && !message.truncated()
}

/// Resend: то же сообщение в ту же сторону.
pub fn resend(id: ExchangeId, message: &WebSocketMessage, cx: &mut App) {
    if !can_resend(message) {
        notify_error(
            "Only complete text and binary messages can be resent".into(),
            cx,
        );
        return;
    }
    send(
        id,
        message.direction,
        message.opcode,
        message.payload.clone(),
        cx,
    );
}

fn send(id: ExchangeId, direction: WsDirection, opcode: WsOpcode, payload: Vec<u8>, cx: &mut App) {
    let Some(WsSend(engine)) = cx.try_global::<WsSend>().cloned() else {
        tracing::warn!("websocket send requested before install");
        return;
    };
    engine.send(EngineCommand::SendWebSocketMessage {
        id,
        direction,
        opcode,
        payload,
    });
}

fn to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `00 ff 1A`, `00ff1a` — пробелы и переводы строк игнорируются.
pub fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let digits: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(bad) = digits.chars().find(|c| !c.is_ascii_hexdigit()) {
        return Err(format!("'{bad}' is not a hex digit"));
    }
    if !digits.len().is_multiple_of(2) {
        return Err("Hex needs two digits per byte".into());
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&digits[i..i + 2], 16)
                .map_err(|_| format!("'{}' is not a hex byte", &digits[i..i + 2]))
        })
        .collect()
}

pub struct SendDialog {
    id: ExchangeId,
    direction: WsDirection,
    binary: bool,
    content: Entity<EditorState>,
    error: Option<String>,
}

impl SendDialog {
    fn new(id: ExchangeId, form: SendForm, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let content = cx.new(|cx| {
            let mut state = EditorState::new(window, cx);
            state.set_value(form.content.clone(), window, cx);
            state
        });
        Self {
            id,
            direction: form.direction,
            binary: form.binary,
            content,
            error: None,
        }
    }

    /// Отправить; `true` — закрыть диалог.
    fn submit(&mut self, cx: &mut Context<Self>) -> bool {
        let form = SendForm {
            direction: self.direction,
            binary: self.binary,
            content: self.content.read(cx).value().to_string(),
        };
        match form.payload() {
            Ok((opcode, payload)) => {
                send(self.id, self.direction, opcode, payload, cx);
                true
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                false
            }
        }
    }
}

fn set_flag(weak: &WeakEntity<SendDialog>, cx: &mut App, f: impl FnOnce(&mut SendDialog)) {
    let _ = weak.update(cx, |d, cx| {
        f(d);
        cx.notify();
    });
}

impl Render for SendDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (danger, border) = (theme.danger, theme.border);
        let weak = cx.entity().downgrade();
        let direction = |id: &'static str, label: &'static str, value: WsDirection| {
            let weak = weak.clone();
            Radio::new(id)
                .label(label)
                .checked(self.direction == value)
                .on_click(move |_, _, cx| set_flag(&weak, cx, move |d| d.direction = value))
        };
        let kind = |id: &'static str, label: &'static str, binary: bool| {
            let weak = weak.clone();
            Radio::new(id)
                .label(label)
                .checked(self.binary == binary)
                .on_click(move |_, _, cx| set_flag(&weak, cx, move |d| d.binary = binary))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(row(
                "Send",
                div()
                    .flex()
                    .gap(px(16.))
                    .child(direction("ws-to-server", "To server", WsDirection::Sent))
                    .child(direction(
                        "ws-to-client",
                        "To client",
                        WsDirection::Received,
                    ))
                    .into_any_element(),
                None,
                danger,
            ))
            .child(row(
                "Type",
                div()
                    .flex()
                    .gap(px(16.))
                    .child(kind("ws-text", "Text", false))
                    .child(kind("ws-binary", "Binary (hex)", true))
                    .into_any_element(),
                None,
                danger,
            ))
            .child(row(
                "Message",
                div()
                    .h(px(EDITOR_HEIGHT))
                    .border_1()
                    .border_color(border)
                    .rounded(px(6.))
                    .child(Editor::new(&self.content).size_full().bordered(false))
                    .into_any_element(),
                self.error.clone(),
                danger,
            ))
    }
}

/// Открыть диалог отправки в окне `window`.
pub fn open_send_dialog(id: ExchangeId, form: SendForm, window: &mut Window, cx: &mut App) {
    let dialog = cx.new(|cx| SendDialog::new(id, form, window, cx));
    window.open_dialog(cx, move |d, _, _| {
        let submit = dialog.clone();
        let footer = DialogFooter::new()
            .child(
                Button::new("ws-send-cancel")
                    .label("Cancel")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(
                Button::new("ws-send-submit")
                    .label("Send")
                    .primary()
                    .on_click(move |_, window, cx| {
                        if submit.update(cx, |d, cx| d.submit(cx)) {
                            window.close_dialog(cx);
                        }
                    }),
            );
        d.title("Send WebSocket Message")
            .w(px(DIALOG_WIDTH))
            .child(dialog.clone())
            .footer(footer)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn message(opcode: WsOpcode, payload: &[u8], size: u64) -> WebSocketMessage {
        WebSocketMessage {
            direction: WsDirection::Received,
            opcode,
            payload: payload.to_vec(),
            size,
            at: Utc::now(),
            injected: false,
        }
    }

    #[test]
    fn hex_is_parsed_with_any_spacing() {
        assert_eq!(parse_hex("00 ff\n1A"), Ok(vec![0, 255, 26]));
        assert_eq!(parse_hex("00ff1a"), Ok(vec![0, 255, 26]));
        assert!(parse_hex("abc").is_err());
        assert!(parse_hex("zz").is_err());
        assert!(parse_hex("éé").is_err(), "non-ASCII must not panic");
        assert_eq!(parse_hex(""), Ok(Vec::new()));
    }

    #[test]
    fn edit_form_round_trips_messages() {
        let binary = message(WsOpcode::Binary, &[0, 255], 2);
        let form = SendForm::from_message(&binary);
        assert_eq!(form.direction, WsDirection::Received);
        assert_eq!(form.content, "00 ff");
        assert_eq!(form.payload(), Ok((WsOpcode::Binary, vec![0, 255])));
        let text = SendForm::from_message(&message(WsOpcode::Text, b"hi", 2));
        assert_eq!(text.payload(), Ok((WsOpcode::Text, b"hi".to_vec())));
    }

    #[test]
    fn only_complete_data_messages_can_be_resent() {
        assert!(can_resend(&message(WsOpcode::Text, b"hi", 2)));
        assert!(!can_resend(&message(WsOpcode::Text, b"h", 99)), "truncated");
        assert!(!can_resend(&message(WsOpcode::Ping, b"", 0)));
    }
}
