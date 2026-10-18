//! Быстрый фильтр — чипы над таблицей (как в Proxyman): протокол
//! (HTTP / HTTPS / WebSocket, один из) и тип содержимого ответа (JSON,
//! Form, …, несколько сразу). Пустой — показывает всё («All»).

use std::collections::BTreeSet;

use domain::value_objects::http_vo::Protocol;

use super::store::TrafficRow;
use crate::ui::theme::semantic::ContentKind;

/// Чип протокола.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolChip {
    Http,
    Https,
    WebSocket,
}

impl ProtocolChip {
    pub const ALL: [ProtocolChip; 3] = [Self::Http, Self::Https, Self::WebSocket];

    pub fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::Https => "HTTPS",
            Self::WebSocket => "WebSocket",
        }
    }

    fn matches(self, row: &TrafficRow) -> bool {
        let s = &row.summary;
        let websocket = s.protocol == Protocol::WebSocket;
        match self {
            Self::WebSocket => websocket,
            // Туннель (CONNECT без расшифровки) — тоже HTTPS.
            Self::Https => !websocket && (s.scheme == "https" || s.protocol == Protocol::Tunnel),
            Self::Http => !websocket && s.scheme == "http" && s.protocol != Protocol::Tunnel,
        }
    }
}

/// Тип содержимого строки (по Content-Type ответа).
pub fn row_kind(row: &TrafficRow) -> ContentKind {
    ContentKind::from_content_type(row.content_type.as_deref())
}

/// Состояние чипов.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuickFilter {
    pub protocol: Option<ProtocolChip>,
    pub kinds: BTreeSet<ContentKindKey>,
}

/// `ContentKind` для упорядоченного множества (порядок — как у чипов).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContentKindKey(u8);

impl From<ContentKind> for ContentKindKey {
    fn from(kind: ContentKind) -> Self {
        let ix = ContentKind::ALL
            .iter()
            .position(|k| *k == kind)
            .unwrap_or(0);
        Self(ix as u8)
    }
}

impl QuickFilter {
    pub fn is_empty(&self) -> bool {
        self.protocol.is_none() && self.kinds.is_empty()
    }

    pub fn has_kind(&self, kind: ContentKind) -> bool {
        self.kinds.contains(&kind.into())
    }

    /// Выбрать протокол (повторный клик по выбранному — снять).
    pub fn toggle_protocol(&self, chip: ProtocolChip) -> Self {
        Self {
            protocol: (self.protocol != Some(chip)).then_some(chip),
            ..self.clone()
        }
    }

    /// Добавить/убрать тип содержимого.
    pub fn toggle_kind(&self, kind: ContentKind) -> Self {
        let key = ContentKindKey::from(kind);
        let kinds = if self.kinds.contains(&key) {
            self.kinds.iter().copied().filter(|k| *k != key).collect()
        } else {
            self.kinds.iter().copied().chain([key]).collect()
        };
        Self {
            kinds,
            ..self.clone()
        }
    }

    pub fn matches(&self, row: &TrafficRow) -> bool {
        self.protocol.is_none_or(|chip| chip.matches(row))
            && (self.kinds.is_empty() || self.kinds.contains(&row_kind(row).into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::traffic::store::test_row;

    fn row(scheme: &str, protocol: Protocol, content_type: Option<&str>) -> TrafficRow {
        let mut row = test_row("a.test", "/");
        row.summary.scheme = scheme.into();
        row.summary.protocol = protocol;
        row.content_type = content_type.map(Into::into);
        row
    }

    #[test]
    fn empty_filter_matches_everything() {
        let filter = QuickFilter::default();
        assert!(filter.is_empty());
        assert!(filter.matches(&row("http", Protocol::Http1, None)));
    }

    #[test]
    fn protocol_chips() {
        let http = row("http", Protocol::Http1, None);
        let https = row("https", Protocol::Http2, None);
        let tunnel = row("http", Protocol::Tunnel, None);
        let ws = row("https", Protocol::WebSocket, None);
        let only = |chip| QuickFilter::default().toggle_protocol(chip);
        assert!(only(ProtocolChip::Http).matches(&http));
        assert!(!only(ProtocolChip::Http).matches(&https));
        assert!(only(ProtocolChip::Https).matches(&https));
        assert!(only(ProtocolChip::Https).matches(&tunnel));
        assert!(!only(ProtocolChip::Https).matches(&ws));
        assert!(only(ProtocolChip::WebSocket).matches(&ws));
    }

    #[test]
    fn toggling_the_selected_protocol_clears_it() {
        let filter = QuickFilter::default().toggle_protocol(ProtocolChip::Https);
        assert_eq!(filter.protocol, Some(ProtocolChip::Https));
        assert_eq!(filter.toggle_protocol(ProtocolChip::Https).protocol, None);
        assert_eq!(
            filter.toggle_protocol(ProtocolChip::Http).protocol,
            Some(ProtocolChip::Http)
        );
    }

    #[test]
    fn kinds_are_multi_select_and_compose_with_protocol() {
        let json = row("https", Protocol::Http1, Some("application/json"));
        let image = row("https", Protocol::Http1, Some("image/png"));
        let css_http = row("http", Protocol::Http1, Some("text/css"));
        let filter = QuickFilter::default()
            .toggle_kind(ContentKind::Json)
            .toggle_kind(ContentKind::Css);
        assert!(filter.matches(&json));
        assert!(filter.matches(&css_http));
        assert!(!filter.matches(&image));
        let https_only = filter.toggle_protocol(ProtocolChip::Https);
        assert!(!https_only.matches(&css_http));
        assert!(
            !filter
                .toggle_kind(ContentKind::Json)
                .has_kind(ContentKind::Json)
        );
    }
}
