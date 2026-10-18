//! Текстовый режим вкладки Body: тело как обычный текст в read-only
//! редакторе — выделить и скопировать любой участок (Cmd+C), плюс «Copy»
//! всего тела. JSON показывается отформатированным (2 пробела).

use std::sync::Arc;

use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::{AnyElement, AppContext as _, Context, Entity, IntoElement, Styled, Window};

use super::body::RenderKind;

/// Текст тела для текстового режима; `None` — у вида нет текстового
/// представления (картинка, бинарное).
pub fn body_text(kind: &RenderKind) -> Option<String> {
    match kind {
        RenderKind::JsonTree { value } => serde_json::to_string_pretty(value.as_ref()).ok(),
        RenderKind::PrettyText { text, .. } => Some(text.as_str().to_string()),
        RenderKind::Form { rows } => Some(
            rows.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        RenderKind::Image { .. } | RenderKind::ImageTooBig { .. } | RenderKind::Binary { .. } => {
            None
        }
    }
}

/// Read-only редактор с текстом тела (создаётся лениво — нужен Window).
pub fn build_text_editor<T: 'static>(
    text: &Arc<String>,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Entity<EditorState> {
    let text = text.as_str().to_string();
    cx.new(|cx| {
        let mut state = EditorState::new(window, cx).line_number(true);
        state.set_value(text, window, cx);
        state
    })
}

/// Элемент текстового режима.
pub fn render_text_editor(editor: &Entity<EditorState>) -> AnyElement {
    Editor::new(editor)
        .readonly(true)
        .size_full()
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_pretty_printed() {
        let value: serde_json::Value = serde_json::from_str(r#"{"a":[1,{"b":null}]}"#).unwrap();
        let text = body_text(&RenderKind::JsonTree {
            value: Arc::new(value),
        })
        .expect("text");
        assert!(text.contains("\n  \"a\": ["), "{text}");
        assert!(text.contains("\"b\": null"), "{text}");
    }

    #[test]
    fn plain_text_and_form_have_text_binary_has_none() {
        let pretty = RenderKind::PrettyText {
            text: Arc::new("<html/>".into()),
            encoding_note: "utf-8".into(),
        };
        assert_eq!(body_text(&pretty).as_deref(), Some("<html/>"));
        let form = RenderKind::Form {
            rows: Arc::new(vec![("a".into(), "1".into()), ("b".into(), "x y".into())]),
        };
        assert_eq!(body_text(&form).as_deref(), Some("a=1\nb=x y"));
        let binary = RenderKind::Binary {
            len: 3,
            preview: "00".into(),
        };
        assert_eq!(body_text(&binary), None);
    }
}
