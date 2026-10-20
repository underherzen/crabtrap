//! Семантика цвета: метод, статус, тег, тип содержимого → тон палитры.
//! Чистые функции — одна таблица правил для таблицы, инспектора и чипов.

use super::palette::ToneName;

/// Тон HTTP-метода (как в Proxyman: GET синий, POST зелёный, …).
pub fn method_tone(method: &str) -> ToneName {
    match method.to_ascii_uppercase().as_str() {
        "GET" => ToneName::Blue,
        "POST" => ToneName::Green,
        "PUT" => ToneName::Orange,
        "PATCH" => ToneName::Purple,
        "DELETE" => ToneName::Red,
        "CONNECT" => ToneName::Teal,
        _ => ToneName::Gray,
    }
}

/// Состояние обмена для точки в строке таблицы и плашки статуса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeState {
    /// Ответа ещё нет.
    Pending,
    Success,
    Redirect,
    ClientError,
    ServerError,
    /// Обмен провалился (сеть, TLS, блок…).
    Failed,
}

impl ExchangeState {
    pub fn of(status: Option<u16>, failed: bool) -> Self {
        if failed {
            return Self::Failed;
        }
        match status {
            None => Self::Pending,
            Some(s) if s < 300 => Self::Success,
            Some(s) if s < 400 => Self::Redirect,
            Some(s) if s < 500 => Self::ClientError,
            Some(_) => Self::ServerError,
        }
    }

    pub fn tone(self) -> ToneName {
        match self {
            Self::Pending => ToneName::Gray,
            Self::Success => ToneName::Green,
            Self::Redirect => ToneName::Blue,
            Self::ClientError => ToneName::Orange,
            Self::ServerError | Self::Failed => ToneName::Red,
        }
    }
}

/// Тон тега обмена: известные теги — по смыслу, остальные — стабильно по
/// хэшу имени (один и тот же тег всегда одного цвета).
pub fn tag_tone(tag: &str) -> ToneName {
    match tag {
        "blocked" => ToneName::Red,
        "map-local" | "mapped-local" => ToneName::Purple,
        "map-remote" | "mapped-remote" => ToneName::Teal,
        "throttled" => ToneName::Yellow,
        "dropped" => ToneName::Red,
        "dns-spoofed" => ToneName::Orange,
        _ => {
            const POOL: [ToneName; 5] = [
                ToneName::Blue,
                ToneName::Pink,
                ToneName::Teal,
                ToneName::Purple,
                ToneName::Orange,
            ];
            let hash = tag
                .bytes()
                .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b.into()));
            POOL[hash as usize % POOL.len()]
        }
    }
}

/// Тип содержимого ответа (чипы фильтра и бейдж в таблице).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentKind {
    Json,
    Form,
    Xml,
    Js,
    Css,
    Image,
    Document,
    Other,
}

impl ContentKind {
    pub const ALL: [ContentKind; 8] = [
        ContentKind::Json,
        ContentKind::Form,
        ContentKind::Xml,
        ContentKind::Js,
        ContentKind::Css,
        ContentKind::Image,
        ContentKind::Document,
        ContentKind::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Form => "Form",
            Self::Xml => "XML",
            Self::Js => "JS",
            Self::Css => "CSS",
            Self::Image => "Image",
            Self::Document => "Document",
            Self::Other => "Other",
        }
    }

    /// По `Content-Type` (параметры вроде `; charset=` игнорируются).
    pub fn from_content_type(content_type: Option<&str>) -> Self {
        let Some(raw) = content_type else {
            return Self::Other;
        };
        let mime = raw
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let (kind, subtype) = mime.split_once('/').unwrap_or((mime.as_str(), ""));
        match (kind, subtype) {
            (_, s) if s == "json" || s.ends_with("+json") || s == "x-ndjson" => Self::Json,
            ("application", "x-www-form-urlencoded") | ("multipart", _) => Self::Form,
            (_, s) if s == "xml" || s.ends_with("+xml") => Self::Xml,
            (_, "javascript" | "ecmascript" | "x-javascript") => Self::Js,
            ("text", "css") => Self::Css,
            ("image", _) => Self::Image,
            ("text", "html" | "plain" | "markdown") | ("application", "pdf") => Self::Document,
            _ => Self::Other,
        }
    }

    pub fn tone(self) -> ToneName {
        match self {
            Self::Json => ToneName::Purple,
            Self::Form => ToneName::Orange,
            Self::Xml => ToneName::Teal,
            Self::Js => ToneName::Yellow,
            Self::Css => ToneName::Pink,
            Self::Image => ToneName::Green,
            Self::Document => ToneName::Blue,
            Self::Other => ToneName::Gray,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_map_to_conventional_tones() {
        assert_eq!(method_tone("GET"), ToneName::Blue);
        assert_eq!(method_tone("post"), ToneName::Green);
        assert_eq!(method_tone("DELETE"), ToneName::Red);
        assert_eq!(method_tone("OPTIONS"), ToneName::Gray);
    }

    #[test]
    fn exchange_state_from_status() {
        assert_eq!(ExchangeState::of(None, false), ExchangeState::Pending);
        assert_eq!(ExchangeState::of(Some(204), false), ExchangeState::Success);
        assert_eq!(ExchangeState::of(Some(304), false), ExchangeState::Redirect);
        assert_eq!(
            ExchangeState::of(Some(404), false),
            ExchangeState::ClientError
        );
        assert_eq!(
            ExchangeState::of(Some(503), false),
            ExchangeState::ServerError
        );
        assert_eq!(ExchangeState::of(Some(200), true), ExchangeState::Failed);
    }

    #[test]
    fn known_tags_have_meaningful_tones_and_others_are_stable() {
        assert_eq!(tag_tone("blocked"), ToneName::Red);
        assert_eq!(tag_tone("throttled"), ToneName::Yellow);
        assert_eq!(tag_tone("custom"), tag_tone("custom"));
    }

    #[test]
    fn content_kind_from_content_type() {
        let k = |s| ContentKind::from_content_type(Some(s));
        assert_eq!(k("application/json; charset=utf-8"), ContentKind::Json);
        assert_eq!(k("application/problem+json"), ContentKind::Json);
        assert_eq!(k("application/x-www-form-urlencoded"), ContentKind::Form);
        assert_eq!(k("multipart/form-data; boundary=x"), ContentKind::Form);
        assert_eq!(k("text/xml"), ContentKind::Xml);
        assert_eq!(k("image/svg+xml"), ContentKind::Xml);
        assert_eq!(k("application/javascript"), ContentKind::Js);
        assert_eq!(k("TEXT/CSS"), ContentKind::Css);
        assert_eq!(k("image/png"), ContentKind::Image);
        assert_eq!(k("text/html"), ContentKind::Document);
        assert_eq!(k("application/octet-stream"), ContentKind::Other);
        assert_eq!(ContentKind::from_content_type(None), ContentKind::Other);
    }
}
