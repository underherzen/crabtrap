//! Поля условия правила, общие для диалогов Tools, Rewrite и Script Editor:
//! режим Wildcard (Host / Path / «Ignore query») или Regex (по полному URL),
//! Method, GraphQL-операция, превью «Matches: …» и «Test URL» с живым
//! результатом. Модель и проверка — `application::rules::location_form`.

use application::use_cases::rules::location_form::{
    LocationErrors, LocationForm, METHODS, UrlCheck,
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::radio::Radio;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Hsla, IntoElement, ParentElement, Render,
    Styled, Subscription, WeakEntity, Window, div, px,
};

use crate::ui::theme::{ToneName, glass};

/// Ширина подписи слева от поля (как в диалогах Tools).
pub const LABEL_WIDTH: f32 = 96.;

/// Какие поля показывать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchFieldsKind {
    /// Всё: режим, Host/Path или Regex, Method, GraphQL, Test URL.
    Full,
    /// Без GraphQL (Throttle: тело запроса не читается).
    NoGraphql,
    /// Только Host (SSL Proxying).
    HostOnly,
}

pub struct MatchFields {
    kind: MatchFieldsKind,
    /// Пустые Host и Path значат «всё» (Rewrite) — для превью.
    empty_matches_everything: bool,
    host: Entity<InputState>,
    path: Entity<InputState>,
    url_regex: Entity<InputState>,
    graphql: Entity<InputState>,
    test_url: Entity<InputState>,
    method: Option<String>,
    use_regex: bool,
    ignore_query: bool,
    errors: LocationErrors,
    _subscriptions: Vec<Subscription>,
}

impl MatchFields {
    pub fn new(
        kind: MatchFieldsKind,
        form: &LocationForm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = |placeholder: &str, value: &str| {
            let (placeholder, value) = (placeholder.to_string(), value.to_string());
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                state.set_value(value, window, cx);
                state
            })
        };
        let host = input(
            "api.example.com  or  *.example.com  (paste a URL to split it)",
            &form.host,
        );
        let path = input("/api/*   (empty — any path)", &form.path);
        let url_regex = input(r"https://api\.example\.com/v[12]/.*", &form.url_regex);
        let graphql = input("GetUser  or  Get*   (empty — any)", &form.graphql_operation);
        let test_url = input("https://api.example.com/v1/users?page=2", "");
        // Превью и результат Test URL — на каждый ввод; родитель наблюдает
        // за компонентом (cx.notify) и тоже перерисовывается.
        let _subscriptions = [&host, &path, &url_regex, &graphql, &test_url]
            .into_iter()
            .map(|input| cx.subscribe(input, |_, _, _: &InputEvent, cx| cx.notify()))
            .collect();
        Self {
            kind,
            empty_matches_everything: false,
            host,
            path,
            url_regex,
            graphql,
            test_url,
            method: form.method.clone(),
            use_regex: form.use_regex,
            ignore_query: form.ignore_query,
            errors: LocationErrors::default(),
            _subscriptions,
        }
    }

    /// Пустое условие — «всё» (Rewrite), а не ошибка.
    pub fn empty_matches_everything(mut self) -> Self {
        self.empty_matches_everything = true;
        self
    }

    pub fn form(&self, cx: &App) -> LocationForm {
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        LocationForm {
            host: value(&self.host),
            path: value(&self.path),
            method: self.method.clone(),
            use_regex: self.use_regex,
            url_regex: value(&self.url_regex),
            ignore_query: self.ignore_query,
            graphql_operation: match self.kind {
                MatchFieldsKind::Full => value(&self.graphql),
                MatchFieldsKind::NoGraphql | MatchFieldsKind::HostOnly => String::new(),
            },
        }
    }

    pub fn set_errors(&mut self, errors: LocationErrors, cx: &mut Context<Self>) {
        self.errors = errors;
        cx.notify();
    }

    fn update(weak: &WeakEntity<Self>, cx: &mut App, f: impl FnOnce(&mut Self)) {
        let _ = weak.update(cx, |fields, cx| {
            f(fields);
            cx.notify();
        });
    }

    fn method_menu(&self, weak: WeakEntity<Self>) -> impl IntoElement {
        let label = format!("{} ▾", self.method.as_deref().unwrap_or("Any method"));
        Button::new("match-method")
            .label(label)
            .dropdown_menu(move |menu, _, _| {
                let choices =
                    std::iter::once(None).chain(METHODS.iter().map(|m| Some(m.to_string())));
                choices.fold(menu, |menu, choice| {
                    let weak = weak.clone();
                    let label = choice.clone().unwrap_or_else(|| "Any method".into());
                    menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        let choice = choice.clone();
                        Self::update(&weak, cx, |f| f.method = choice);
                    }))
                })
            })
    }

    fn mode_row(&self, weak: &WeakEntity<Self>, danger: Hsla) -> AnyElement {
        let radio = |id: &'static str, label: &'static str, regex: bool| {
            let weak = weak.clone();
            Radio::new(id)
                .label(label)
                .checked(self.use_regex == regex)
                .on_click(move |_, _, cx| Self::update(&weak, cx, |f| f.use_regex = regex))
        };
        row(
            "Match",
            div()
                .flex()
                .gap(px(16.))
                .child(radio("match-wildcard", "Wildcard", false))
                .child(radio("match-regex", "Regex (full URL)", true))
                .into_any_element(),
            None,
            danger,
        )
    }

    fn location_rows(&self, weak: &WeakEntity<Self>, danger: Hsla) -> Vec<AnyElement> {
        if self.use_regex {
            return vec![row(
                "URL regex",
                Input::new(&self.url_regex).into_any_element(),
                self.errors.url_regex.clone(),
                danger,
            )];
        }
        let ignore_query = {
            let weak = weak.clone();
            Checkbox::new("match-ignore-query")
                .label("Ignore query string  (/users also matches /users?page=2)")
                .checked(self.ignore_query)
                .on_click(move |checked, _, cx| {
                    let checked = *checked;
                    Self::update(&weak, cx, |f| f.ignore_query = checked);
                })
        };
        vec![
            row(
                "Host",
                Input::new(&self.host).into_any_element(),
                self.errors.host.clone(),
                danger,
            ),
            row(
                "Path",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(Input::new(&self.path))
                    .child(ignore_query)
                    .into_any_element(),
                self.errors.path.clone(),
                danger,
            ),
        ]
    }

    fn preview(&self, cx: &App) -> String {
        let form = self.form(cx);
        let empty = !form.use_regex
            && form.host.trim().is_empty()
            && form.path.trim().is_empty()
            && form.method.is_none()
            && form.graphql_operation.trim().is_empty();
        match self.empty_matches_everything && empty {
            true => "Matches: everything".into(),
            false => format!("Matches: {}", form.preview()),
        }
    }

    /// «Test URL»: поле и результат (пусто — ничего не показываем).
    fn test_row(&self, cx: &App, danger: Hsla) -> AnyElement {
        let url = self.test_url.read(cx).value().to_string();
        let p = glass(cx);
        let result = match url.trim() {
            "" => None,
            url => Some(match self.form(cx).check_url(url) {
                UrlCheck::Matches { checked } => (
                    p.tone(ToneName::Green).fg,
                    format!("✓ Matches{}", checked_as(url, &checked)),
                ),
                UrlCheck::NoMatch { checked } => (
                    p.tone(ToneName::Red).fg,
                    format!("✗ Doesn't match{}", checked_as(url, &checked)),
                ),
                UrlCheck::Invalid(why) => (cx.theme().muted_foreground, why),
            }),
        };
        let graphql_note = (!self.form(cx).graphql_operation.trim().is_empty())
            .then_some(" — GraphQL operation is checked on live traffic only");
        row(
            "Test URL",
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(Input::new(&self.test_url))
                .when_some(result, |el, (color, text)| {
                    el.child(
                        div()
                            .text_size(px(12.))
                            .text_color(color)
                            .child(format!("{text}{}", graphql_note.unwrap_or_default())),
                    )
                })
                .into_any_element(),
            None,
            danger,
        )
    }
}

impl Render for MatchFields {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (danger, muted) = (theme.danger, theme.muted_foreground);
        let weak = cx.entity().downgrade();
        let body = div().flex().flex_col().gap(px(10.));
        if self.kind == MatchFieldsKind::HostOnly {
            return body.child(row(
                "Host",
                Input::new(&self.host).into_any_element(),
                self.errors.host.clone(),
                danger,
            ));
        }
        body.child(self.mode_row(&weak, danger))
            .children(self.location_rows(&weak, danger))
            .child(row(
                "Method",
                self.method_menu(weak.clone()).into_any_element(),
                None,
                danger,
            ))
            .when(self.kind == MatchFieldsKind::Full, |el| {
                el.child(row(
                    "GraphQL op",
                    Input::new(&self.graphql).into_any_element(),
                    self.errors.graphql_operation.clone(),
                    danger,
                ))
            })
            .child(
                div()
                    .pl(px(LABEL_WIDTH + 8.))
                    .text_size(px(12.))
                    .text_color(muted)
                    .child(self.preview(cx)),
            )
            .child(self.test_row(cx, danger))
    }
}

/// Пояснение, если правило видит URL иначе, чем его ввели (regex сверяется
/// с нормализованным видом).
fn checked_as(typed: &str, checked: &str) -> String {
    match typed.trim() == checked {
        true => String::new(),
        false => format!("  (checked as {checked})"),
    }
}

/// Строка формы: подпись слева, поле, ошибка под полем.
pub fn row(label: &str, field: AnyElement, error: Option<String>, danger: Hsla) -> AnyElement {
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .child(
            div()
                .w(px(LABEL_WIDTH))
                .flex_none()
                .pt(px(6.))
                .text_size(px(13.))
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(field)
                .when_some(error, |el, err| {
                    el.child(div().text_size(px(12.)).text_color(danger).child(err))
                }),
        )
        .into_any_element()
}
