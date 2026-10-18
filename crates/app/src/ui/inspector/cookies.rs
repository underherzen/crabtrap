//! Вкладка «Cookies»: request `Cookie` и response `Set-Cookie` через
//! `application::dto::inspection::{parse_cookies, parse_set_cookie}`
//! (Phase 5, агент D).
//!
//! Stateless-рендер: парсинг заголовков дешёвый (микросекунды на строку),
//! Entity-состояние не нужно — каждый рендер читает заголовки из
//! `ExchangeContext.detail`. Таблица: name | value | attributes
//! (path/domain/expires — вторичные чипы, secure/httponly/samesite —
//! цветные, остальные атрибуты — как есть). Значение — целиком,
//! копирование — кликом, кнопкой или меню (см. [`super::kv`]).

use application::dto::capture::HeaderEntryDto;
use application::dto::inspection::{CookieEntry, parse_cookies, parse_set_cookie};
use gpui_kit::InteractiveElement as _;
use gpui_kit::StatefulInteractiveElement as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{Sizable as _, Size, h_flex};
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px, relative,
};

use super::kv::{KvRow, NAME_COLUMN_MAX, NAME_COLUMN_SHARE, PairStyle};
use super::{ExchangeContext, Side};

/// Все cookies из заголовков `Cookie` запроса (несколько заголовков
/// конкатенируются по RFC 6265 §5.4).
pub fn request_cookies(headers: &[HeaderEntryDto]) -> Vec<CookieEntry> {
    headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("cookie"))
        .flat_map(|h| parse_cookies(&h.value))
        .collect()
}

/// Все cookies из заголовков `Set-Cookie` ответа (по одному на заголовок).
pub fn response_cookies(headers: &[HeaderEntryDto]) -> Vec<CookieEntry> {
    headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("set-cookie"))
        .filter_map(|h| parse_set_cookie(&h.value))
        .collect()
}

/// Cookies (вкладка инспектора).
#[derive(gpui_kit::IntoElement)]
pub struct CookiesTab {
    ctx: ExchangeContext,
}

impl CookiesTab {
    pub fn new(ctx: ExchangeContext) -> Self {
        Self { ctx }
    }
}

impl RenderOnce for CookiesTab {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let detail = &self.ctx.detail;
        let section = match (self.ctx.side, &detail.response) {
            (Side::Request, _) => cookie_section(
                "Cookie",
                &request_cookies(&detail.request.headers),
                false,
                theme,
            ),
            (Side::Response, Some(response)) => cookie_section(
                "Set-Cookie",
                &response_cookies(&response.headers),
                false,
                theme,
            ),
            (Side::Response, None) => cookie_section("Set-Cookie", &[], true, theme),
        };
        div()
            .id(("cookies-tab", self.ctx.side as usize))
            .size_full()
            .flex()
            .flex_col()
            .min_h(px(0.))
            .p_3()
            .gap_4()
            .overflow_y_scroll()
            .child(section)
    }
}

/// Одна секция (заголовок + таблица name/value/attributes).
fn cookie_section(
    title: &str,
    cookies: &[CookieEntry],
    no_response: bool,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let count = cookies.len();
    // flex_none: не сжиматься в прокручиваемой колонке вкладки.
    let mut section = div().flex().flex_col().flex_none().gap_1().child(
        h_flex()
            .items_baseline()
            .gap_2()
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(title.to_string()),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.muted_foreground)
                    .child(format!("({count})")),
            ),
    );

    if count == 0 {
        let message = if no_response && title.starts_with("Response") {
            "No response"
        } else {
            "No cookies"
        };
        return section
            .child(
                div()
                    .py_2()
                    .text_size(px(12.))
                    .text_color(theme.foreground.opacity(0.5))
                    .child(message),
            )
            .into_any_element();
    }

    // Шапка таблицы — те же колонки, что у строк (см. `super::kv`).
    section = section.child(
        h_flex()
            .gap(px(8.))
            .px_2()
            .py_1()
            .text_size(px(10.))
            .text_color(theme.muted_foreground)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .w(relative(NAME_COLUMN_SHARE))
                    .max_w(px(NAME_COLUMN_MAX))
                    .flex_none()
                    .child("NAME"),
            )
            .child(div().flex_1().child("VALUE · ATTRIBUTES")),
    );

    let mono = theme.mono_font_family.clone();
    section
        .children(
            cookies
                .iter()
                .enumerate()
                .map(|(ix, c)| cookie_row(ix, c, mono.clone())),
        )
        .into_any_element()
}

/// Строка cookie: имя | значение (целиком), под ним чипы атрибутов.
fn cookie_row(ix: usize, cookie: &CookieEntry, mono: gpui_kit::SharedString) -> AnyElement {
    let chips = attribute_chips(cookie);
    let row = KvRow::new(
        ("cookie-row", ix),
        cookie.name.clone(),
        cookie.value.clone(),
        mono,
    )
    .pair(PairStyle::Assignment)
    .striped(ix % 2 == 1);
    if chips.is_empty() {
        row.into_any_element()
    } else {
        row.below(h_flex().flex_wrap().gap_1().children(chips))
            .into_any_element()
    }
}

/// Чипы атрибутов Set-Cookie: известные — с семантическими цветами
/// (Secure — success, HttpOnly — warning, SameSite — info, path/domain/
/// expires — secondary), остальные (Max-Age, Priority, …) — как есть.
fn attribute_chips(cookie: &CookieEntry) -> Vec<AnyElement> {
    let mut chips = Vec::new();

    // Флаги через хелперы CookieEntry.
    if cookie.is_secure() {
        chips.push(
            Tag::success()
                .with_size(Size::XSmall)
                .child("Secure")
                .into_any_element(),
        );
    }
    if cookie.is_http_only() {
        chips.push(
            Tag::warning()
                .with_size(Size::XSmall)
                .child("HttpOnly")
                .into_any_element(),
        );
    }

    // Остальные атрибуты в исходном порядке; Secure/HttpOnly уже показаны.
    for (name, value) in &cookie.attrs {
        let lower = name.to_ascii_lowercase();
        if lower == "secure" || lower == "httponly" {
            continue;
        }
        let label = if value.is_empty() {
            name.clone()
        } else {
            format!("{name}={value}")
        };
        let tag = match lower.as_str() {
            "samesite" => Tag::info(),
            _ => Tag::secondary(),
        };
        chips.push(tag.with_size(Size::XSmall).child(label).into_any_element());
    }
    chips
}

#[cfg(test)]
mod tests {
    use super::*;
    use application::dto::capture::HeaderEntryDto;

    fn header(name: &str, value: &str) -> HeaderEntryDto {
        HeaderEntryDto {
            name: name.into(),
            value: value.into(),
        }
    }

    #[test]
    fn request_cookies_from_headers() {
        let headers = vec![
            header("Host", "example.com"),
            header("cookie", "session=abc; theme=dark"),
            header("X-Other", "1"),
        ];
        let cookies = request_cookies(&headers);
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].name, "session");
        assert_eq!(cookies[1].value, "dark");
    }

    #[test]
    fn request_cookies_multiple_headers_concat() {
        let headers = vec![header("Cookie", "a=1"), header("Cookie", "b=2")];
        assert_eq!(request_cookies(&headers).len(), 2);
    }

    #[test]
    fn response_cookies_parse_each_set_cookie() {
        let headers = vec![
            header("Set-Cookie", "id=u7; Path=/; Secure"),
            header("set-cookie", "theme=light; SameSite=Lax"),
            header("Set-Cookie", "broken-no-equals"),
        ];
        let cookies = response_cookies(&headers);
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].name, "id");
        assert!(cookies[0].is_secure());
        assert_eq!(cookies[1].attr("samesite"), Some("Lax"));
    }

    #[test]
    fn response_cookies_empty_when_none() {
        assert!(response_cookies(&[]).is_empty());
        assert!(response_cookies(&[header("Content-Type", "text/html")]).is_empty());
    }
}
