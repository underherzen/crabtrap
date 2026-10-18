//! Вкладка «Query»: параметры query string запроса (раскодированные);
//! значение — целиком, копирование — см. [`super::kv`].

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::v_flex;
use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement as _, Styled, Window, div, px,
};

use super::ExchangeContext;
use super::kv::{KvRow, PairStyle};
use crate::ui::theme::glass;
use crate::ui::theme::metrics::TEXT_XS;

/// Параметры query string из URI (абсолютного или origin-form), в порядке
/// появления; `+` и `%XX` раскодируются.
pub fn query_params(uri: &str) -> Vec<(String, String)> {
    let without_fragment = uri.split('#').next().unwrap_or("");
    let Some((_, query)) = without_fragment.split_once('?') else {
        return Vec::new();
    };
    url::form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[derive(IntoElement)]
pub struct QueryTab {
    ctx: ExchangeContext,
}

impl QueryTab {
    pub fn new(ctx: ExchangeContext) -> Self {
        Self { ctx }
    }
}

impl RenderOnce for QueryTab {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let mono = cx.theme().mono_font_family.clone();
        let params = query_params(&self.ctx.detail.request.uri);
        if params.is_empty() {
            return super::placeholder("No query parameters", &p).into_any_element();
        }
        let count = params.len();
        let rows = params.into_iter().enumerate().map(|(ix, (name, value))| {
            KvRow::new(("query-row", ix), name, value, mono.clone())
                .pair(PairStyle::Assignment)
                .striped(ix % 2 == 1)
        });
        v_flex()
            .id("query-tab")
            .size_full()
            .overflow_y_scroll()
            .p(px(10.))
            .gap(px(8.))
            .child(
                div()
                    .flex_none()
                    .text_size(px(TEXT_XS))
                    .text_color(p.text_tertiary)
                    .child(if count == 1 {
                        "1 parameter".to_string()
                    } else {
                        format!("{count} parameters")
                    }),
            )
            .child(v_flex().flex_none().children(rows))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_absolute_and_origin_form() {
        assert_eq!(
            query_params("https://a.test/search?q=hello+world&lang=en"),
            [
                ("q".into(), "hello world".into()),
                ("lang".into(), "en".into())
            ]
        );
        assert_eq!(
            query_params("/x?id=%D0%BF&empty="),
            [("id".into(), "п".into()), ("empty".into(), String::new())]
        );
    }

    #[test]
    fn no_query_or_fragment_only() {
        assert!(query_params("/plain").is_empty());
        assert!(query_params("/page#section?not=query").is_empty());
        assert_eq!(query_params("/a?x=1#frag"), [("x".into(), "1".into())]);
    }
}
