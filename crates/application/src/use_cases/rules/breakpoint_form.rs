//! Форма окна брейкпоинта: что показать из припаркованного сообщения и
//! как собрать правку для `ResolveIntercept`.

use domain::value_objects::http_vo::HeaderEntry;

use crate::dto::rules::{InterceptPreviewDto, InterceptableMessageDto};
use crate::use_cases::compose::form::{is_token, parse_header_lines};

/// Изменён ли текст тела относительно исходных байт (переводы строк не
/// считаются).
fn text_changed(text: &str, original: Option<&[u8]>) -> bool {
    let Some(original) = original.and_then(|b| std::str::from_utf8(b).ok()) else {
        return true;
    };
    text.replace("\r\n", "\n").trim_end_matches('\n')
        != original.replace("\r\n", "\n").trim_end_matches('\n')
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakpointForm {
    /// Запрос (метод и URL правятся) или ответ (статус).
    pub is_request: bool,
    pub method: String,
    pub url: String,
    pub status: String,
    /// По строке на заголовок: `Name: value`.
    pub headers: String,
    /// Текст тела; `None` — тело не загружено или двоичное (уйдёт как было).
    pub body: Option<String>,
}

impl BreakpointForm {
    pub fn from_preview(preview: &InterceptPreviewDto) -> Self {
        let message = &preview.message;
        Self {
            is_request: message.status.is_none(),
            method: message.method.clone().unwrap_or_default(),
            url: message.uri.clone().unwrap_or_default(),
            status: message.status.map(|s| s.to_string()).unwrap_or_default(),
            headers: message
                .headers
                .iter()
                .map(|h| format!("{}: {}", h.name, h.value))
                .collect::<Vec<_>>()
                .join("\n"),
            body: message
                .body
                .as_ref()
                .and_then(|b| String::from_utf8(b.clone()).ok()),
        }
    }

    /// Правка для движка или текст ошибки.
    pub fn to_edited(
        &self,
        original: &InterceptableMessageDto,
    ) -> Result<InterceptableMessageDto, String> {
        let headers = parse_header_lines(&self.headers)?
            .into_iter()
            .map(|h| HeaderEntry {
                name: h.name,
                value: h.value,
            })
            .collect();
        let body = match &self.body {
            // Текст не правили (редактор мог свести CRLF к LF) — исходные
            // байты: multipart и подписи тел не ломаются.
            Some(text) if !text_changed(text, original.body.as_deref()) => original.body.clone(),
            Some(text) => Some(text.clone().into_bytes()),
            None => original.body.clone(),
        };
        if self.is_request {
            let method = self.method.trim().to_ascii_uppercase();
            if !is_token(&method) {
                return Err("Method must be an HTTP token, e.g. GET".into());
            }
            let url = self.url.trim();
            let valid = url::Url::parse(url)
                .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
                && !url.contains(char::is_whitespace);
            if !valid {
                return Err("URL must be absolute http(s)://host/path without spaces".into());
            }
            return Ok(InterceptableMessageDto {
                method: Some(method),
                uri: Some(url.to_string()),
                headers,
                body,
                ..original.clone()
            });
        }
        let status = match self.status.trim().parse::<u16>() {
            Ok(status) if (200..=599).contains(&status) => status,
            _ => return Err("Status must be a number 200–599".into()),
        };
        Ok(InterceptableMessageDto {
            status: Some(status),
            headers,
            body,
            ..original.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::entities::rule::BreakPhase;
    use domain::value_objects::{ExchangeId, InterceptId};

    fn preview(message: InterceptableMessageDto) -> InterceptPreviewDto {
        InterceptPreviewDto {
            intercept_id: InterceptId::new(),
            exchange_id: ExchangeId::new(),
            phase: BreakPhase::Request,
            url: "https://a.test/x".into(),
            message,
        }
    }

    fn request(body: Option<&[u8]>) -> InterceptableMessageDto {
        InterceptableMessageDto {
            method: Some("GET".into()),
            uri: Some("https://a.test/x".into()),
            status: None,
            reason: None,
            headers: vec![HeaderEntry {
                name: "Accept".into(),
                value: "*/*".into(),
            }],
            body: body.map(<[u8]>::to_vec),
        }
    }

    #[test]
    fn request_roundtrip_and_edit() {
        let original = request(Some(b"hi"));
        let mut form = BreakpointForm::from_preview(&preview(original.clone()));
        assert_eq!(form.headers, "Accept: */*");
        assert_eq!(form.to_edited(&original).expect("same"), original);
        form.method = "post".into();
        form.body = Some("bye".into());
        let edited = form.to_edited(&original).expect("edited");
        assert_eq!(edited.method.as_deref(), Some("POST"));
        assert_eq!(edited.body.as_deref(), Some(&b"bye"[..]));
    }

    #[test]
    fn untouched_crlf_body_keeps_original_bytes() {
        let original = request(Some(b"--b\r\nfield\r\n--b--\r\n"));
        let mut form = BreakpointForm::from_preview(&preview(original.clone()));
        form.body = Some("--b\nfield\n--b--".into());
        assert_eq!(form.to_edited(&original).expect("ok").body, original.body);
    }

    #[test]
    fn binary_body_is_not_editable_and_kept() {
        let original = request(Some(&[0xff, 0x00]));
        let form = BreakpointForm::from_preview(&preview(original.clone()));
        assert_eq!(form.body, None);
        assert_eq!(form.to_edited(&original).expect("ok").body, original.body);
    }

    #[test]
    fn response_status_is_validated() {
        let original = InterceptableMessageDto {
            method: None,
            uri: None,
            status: Some(200),
            ..request(None)
        };
        let mut form = BreakpointForm::from_preview(&preview(original.clone()));
        assert!(!form.is_request);
        form.status = "99".into();
        assert!(form.to_edited(&original).is_err());
        form.status = "503".into();
        assert_eq!(form.to_edited(&original).expect("ok").status, Some(503));
    }
}
