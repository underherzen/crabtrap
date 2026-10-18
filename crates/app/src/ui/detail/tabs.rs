//! Вкладки панелей инспектора: у запроса и ответа свой набор и свой выбор.

use crate::ui::inspector::Side;

/// Вкладка панели инспектора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab {
    Overview,
    Headers,
    Query,
    Body,
    Cookies,
    Raw,
    Hex,
    /// Сообщения WebSocket (только у ответа WebSocket-обмена).
    Messages,
}

impl InspectorTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Headers => "Headers",
            Self::Query => "Query",
            Self::Body => "Body",
            Self::Cookies => "Cookies",
            Self::Raw => "Raw",
            Self::Hex => "Hex",
            Self::Messages => "Messages",
        }
    }

    /// Вкладки панели стороны (порядок = порядок в переключателе); у
    /// WebSocket-обмена ответ — сообщения, заголовки рукопожатия и raw.
    pub fn for_side(side: Side, websocket: bool) -> &'static [InspectorTab] {
        const WEBSOCKET: [InspectorTab; 3] = [
            InspectorTab::Messages,
            InspectorTab::Headers,
            InspectorTab::Raw,
        ];
        const REQUEST: [InspectorTab; 7] = [
            InspectorTab::Overview,
            InspectorTab::Headers,
            InspectorTab::Query,
            InspectorTab::Body,
            InspectorTab::Cookies,
            InspectorTab::Raw,
            InspectorTab::Hex,
        ];
        const RESPONSE: [InspectorTab; 5] = [
            InspectorTab::Headers,
            InspectorTab::Body,
            InspectorTab::Cookies,
            InspectorTab::Raw,
            InspectorTab::Hex,
        ];
        match side {
            Side::Request => &REQUEST,
            Side::Response if websocket => &WEBSOCKET,
            Side::Response => &RESPONSE,
        }
    }

    /// Вкладка по умолчанию: сводка у запроса, тело у ответа.
    pub fn default_for(side: Side) -> Self {
        match side {
            Side::Request => Self::Overview,
            Side::Response => Self::Body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_side_offers_its_default_tab() {
        for side in [Side::Request, Side::Response] {
            assert!(InspectorTab::for_side(side, false).contains(&InspectorTab::default_for(side)));
        }
    }

    #[test]
    fn overview_and_query_live_on_the_request_side_only() {
        let response = InspectorTab::for_side(Side::Response, false);
        assert!(!response.contains(&InspectorTab::Overview));
        assert!(!response.contains(&InspectorTab::Query));
        assert!(InspectorTab::for_side(Side::Request, false).contains(&InspectorTab::Query));
        assert_eq!(
            InspectorTab::for_side(Side::Response, true)[0],
            InspectorTab::Messages
        );
    }
}
