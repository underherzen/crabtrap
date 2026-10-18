//! Поля ответа Map Local в диалоге: Status и Content-Type. Пустой Status —
//! 200, пустой Content-Type — по расширению файла. Ошибки — по полям.

use std::ops::RangeInclusive;

pub use super::edit::MapLocalResponse;

/// Коды ответа Map Local: только финальные (1xx клиенту не отдать).
pub const MAP_LOCAL_STATUS_RANGE: RangeInclusive<u16> = 200..=599;

/// Значения полей (как введены).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseForm {
    pub status: String,
    pub content_type: String,
}

/// Ошибки по полям (None — поле в порядке).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResponseErrors {
    pub status: Option<String>,
    pub content_type: Option<String>,
}

impl Default for ResponseForm {
    fn default() -> Self {
        Self::from_response(&MapLocalResponse::default())
    }
}

impl ResponseForm {
    pub fn from_response(response: &MapLocalResponse) -> Self {
        Self {
            status: response.status.to_string(),
            content_type: response.content_type.clone().unwrap_or_default(),
        }
    }

    /// Ответ из полей; `Err` — ошибки под нужными полями.
    pub fn to_response(&self) -> Result<MapLocalResponse, ResponseErrors> {
        let status = match self.status.trim() {
            "" => Ok(MapLocalResponse::default().status),
            s => s
                .parse::<u16>()
                .ok()
                .filter(|code| MAP_LOCAL_STATUS_RANGE.contains(code))
                .ok_or_else(|| "Status must be a number 200–599".to_string()),
        };
        let content_type = match self.content_type.trim() {
            "" => Ok(None),
            ct => content_type_error(ct)
                .map_or_else(|| Ok(Some(ct.to_string())), |e| Err(e.to_string())),
        };
        match (status, content_type) {
            (Ok(status), Ok(content_type)) => Ok(MapLocalResponse {
                status,
                content_type,
            }),
            (status, content_type) => Err(ResponseErrors {
                status: status.err(),
                content_type: content_type.err(),
            }),
        }
    }
}

/// Почему значение не годится в заголовок Content-Type (`None` — годится):
/// печатный ASCII без переводов строк и `type/subtype` с непустыми частями.
pub fn content_type_error(value: &str) -> Option<&'static str> {
    if !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Some("Content-Type must be plain ASCII on one line");
    }
    let mime = value.split(';').next().unwrap_or_default().trim();
    match mime.split_once('/') {
        Some((kind, sub)) if !kind.is_empty() && !sub.is_empty() && !mime.contains(' ') => None,
        _ => Some("Content-Type looks like type/subtype, e.g. application/json"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(status: &str, content_type: &str) -> ResponseForm {
        ResponseForm {
            status: status.into(),
            content_type: content_type.into(),
        }
    }

    #[test]
    fn default_form_is_200_with_auto_content_type() {
        assert_eq!(
            ResponseForm::default().to_response(),
            Ok(MapLocalResponse::default())
        );
        assert_eq!(
            MapLocalResponse::default(),
            MapLocalResponse {
                status: 200,
                content_type: None
            }
        );
    }

    #[test]
    fn fields_are_trimmed_and_empty_means_default() {
        assert_eq!(
            form(" 404 ", " text/plain; charset=utf-8 ").to_response(),
            Ok(MapLocalResponse {
                status: 404,
                content_type: Some("text/plain; charset=utf-8".into()),
            })
        );
        assert_eq!(form("", "").to_response(), Ok(MapLocalResponse::default()));
    }

    #[test]
    fn bad_status_is_reported_on_its_field() {
        for status in ["abc", "99", "101", "600", "-1"] {
            let errors = form(status, "").to_response().expect_err(status);
            assert!(errors.status.is_some(), "{status}");
            assert!(errors.content_type.is_none(), "{status}");
        }
    }

    #[test]
    fn bad_content_type_is_reported_on_its_field() {
        for ct in ["json", "text/plain\r\nX-Evil: 1", "тип/x", "/json", "text/"] {
            let errors = form("200", ct).to_response().expect_err(ct);
            assert!(errors.content_type.is_some(), "{ct:?}");
            assert!(errors.status.is_none(), "{ct:?}");
        }
    }

    #[test]
    fn round_trips_through_response() {
        let response = MapLocalResponse {
            status: 500,
            content_type: Some("application/problem+json".into()),
        };
        assert_eq!(
            ResponseForm::from_response(&response).to_response(),
            Ok(response)
        );
        assert_eq!(
            ResponseForm::from_response(&MapLocalResponse::default()),
            ResponseForm::default()
        );
    }

    #[test]
    fn content_type_check_is_shared_with_rule_validation() {
        assert_eq!(content_type_error("application/json"), None);
        assert!(content_type_error("").is_some());
    }
}
