//! Строка «имя → значение» инспектора (Headers, Query, Cookies): значение
//! видно целиком (переносится, а не обрезается «…»), копируется кликом по
//! строке, кнопкой при наведении или из контекстного меню (значение, имя,
//! пара целиком). Имя и значение выделяются мышью (⌘C копирует выделенное);
//! клик, закончивший выделение, строку целиком не копирует.
//! Копирование подтверждается коротким уведомлением.

use gpui_kit::base::{SelectableText, TextSelection, TextSelectionHandle};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, IconName, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClipboardItem, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, Subscription,
    Window, div, px, relative,
};

use crate::ui::theme::glass;
use crate::ui::theme::metrics::{RADIUS, TEXT_SM};

/// Колонка имён: доля ширины строки, но не шире этого (узкая панель
/// инспектора справа не должна отдавать имени почти всё место).
pub const NAME_COLUMN_MAX: f32 = 200.;
pub const NAME_COLUMN_SHARE: f32 = 0.3;

/// Тип уведомления «скопировано»: новое заменяет предыдущее, а не копится.
struct CopiedNotice;

/// Скопировать текст и коротко подтвердить это.
pub fn copy_text(text: &str, what: &str, window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
    window.push_notification(
        Notification::success(format!("{what} copied"))
            .id::<CopiedNotice>()
            .autohide(true),
        cx,
    );
}

/// Текст, выделяемый мышью. Голый `SelectableText` о смене выделения окну
/// не сообщает — выделение рисовалось только со следующим посторонним
/// перерендером (батч трафика), с заметной задержкой. Здесь handle живёт
/// вместе с подпиской, перерисовывающей окно на каждое изменение.
#[derive(IntoElement)]
pub struct Selectable {
    id: ElementId,
    text: SharedString,
}

impl Selectable {
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
        }
    }
}

/// Handle выделения + подписка на перерисовку (живут, пока элемент на экране).
struct RefreshingHandle {
    handle: TextSelectionHandle,
    _refresh: Subscription,
}

impl RenderOnce for Selectable {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let text = self.text.clone();
        let state = window.use_keyed_state(self.id.clone(), cx, move |window, cx| {
            let handle = TextSelectionHandle::new(text, cx);
            let refresh = handle.refresh_window_on_change(window, cx);
            RefreshingHandle {
                handle,
                _refresh: refresh,
            }
        });
        let handle = state.read(cx).handle.clone();
        handle.set_fallback_copy_text(self.text.clone(), cx);
        SelectableText::with_handle(self.id, handle, self.text)
    }
}

/// Как назвать пару при копировании «целиком».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairStyle {
    /// `Name: value` — заголовки.
    Header,
    /// `name=value` — query и cookies.
    Assignment,
}

impl PairStyle {
    pub fn join(self, name: &str, value: &str) -> String {
        match self {
            Self::Header => format!("{name}: {value}"),
            Self::Assignment => format!("{name}={value}"),
        }
    }
}

#[derive(IntoElement)]
pub struct KvRow {
    id: ElementId,
    name: SharedString,
    value: SharedString,
    pair: PairStyle,
    striped: bool,
    mono_font: SharedString,
    /// Что показать под значением (атрибуты cookie).
    below: Option<AnyElement>,
}

impl KvRow {
    pub fn new(
        id: impl Into<ElementId>,
        name: impl Into<SharedString>,
        value: impl Into<SharedString>,
        mono_font: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            value: value.into(),
            pair: PairStyle::Header,
            striped: false,
            mono_font: mono_font.into(),
            below: None,
        }
    }

    pub fn pair(mut self, pair: PairStyle) -> Self {
        self.pair = pair;
        self
    }

    pub fn striped(mut self, striped: bool) -> Self {
        self.striped = striped;
        self
    }

    pub fn below(mut self, below: impl IntoElement) -> Self {
        self.below = Some(below.into_any_element());
        self
    }
}

impl RenderOnce for KvRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let group: SharedString = format!("kv-{:?}", self.id).into();
        let (name, value, pair) = (self.name.clone(), self.value.clone(), self.pair);
        let copy_value = value.clone();
        let button_value = value.clone();
        let row = div()
            .id(self.id.clone())
            .group(group.clone())
            .flex()
            .items_start()
            .gap(px(8.))
            .px_2()
            .py(px(4.))
            .rounded(px(RADIUS))
            .cursor_pointer()
            .when(self.striped, |this| this.bg(p.stripe))
            .hover(|this| this.bg(p.row_hover))
            .on_click(move |_, window, cx| {
                if !TextSelection::has_selection(window, cx) {
                    copy_text(&copy_value, "Value", window, cx)
                }
            })
            .child(
                div()
                    .w(relative(NAME_COLUMN_SHARE))
                    .max_w(px(NAME_COLUMN_MAX))
                    .flex_none()
                    .min_w(px(0.))
                    .text_size(px(TEXT_SM))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.text_secondary)
                    .cursor_text()
                    .child(Selectable::new("name", self.name)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .font_family(self.mono_font)
                            .text_size(px(TEXT_SM))
                            .text_color(p.text)
                            .cursor_text()
                            .child(Selectable::new("value", self.value)),
                    )
                    .children(self.below),
            )
            .child(
                div()
                    .id("copy")
                    .flex_none()
                    .p(px(2.))
                    .rounded(px(RADIUS))
                    .opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
                    .hover(|style| style.bg(p.control_hover))
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Copy value").build(window, cx)
                    })
                    .child(
                        Icon::new(IconName::Copy)
                            .xsmall()
                            .text_color(p.text_secondary),
                    )
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        copy_text(&button_value, "Value", window, cx)
                    }),
            );
        row.context_menu(move |menu, _, _| {
            let (v, n, both) = (value.clone(), name.clone(), pair.join(&name, &value));
            menu.item(
                PopupMenuItem::new("Copy Value")
                    .on_click(move |_, window, cx| copy_text(&v, "Value", window, cx)),
            )
            .item(
                PopupMenuItem::new("Copy Name")
                    .on_click(move |_, window, cx| copy_text(&n, "Name", window, cx)),
            )
            .item(
                PopupMenuItem::new(match pair {
                    PairStyle::Header => "Copy Header",
                    PairStyle::Assignment => "Copy name=value",
                })
                .on_click(move |_, window, cx| copy_text(&both, "Pair", window, cx)),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_join_in_their_native_syntax() {
        assert_eq!(PairStyle::Header.join("Accept", "*/*"), "Accept: */*");
        assert_eq!(PairStyle::Assignment.join("sid", "a=b"), "sid=a=b");
    }
}
