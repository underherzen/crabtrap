//! Окно Tools (как в Charles): SSL Proxying, Block List, Map Local,
//! Map Remote, Throttle, Rewrite, Breakpoints, Scripting — список слева (с «On» у включённых), содержимое
//! справа, над общим [`store::RulesStore`].
//!
//! Вкладка правил: галочка «Enable …», таблица (Checkbox enabled, Location,
//! Target; клик — выделение, двойной клик — правка), кнопки Add… / Edit… /
//! Duplicate / Remove, Delete/Backspace — удалить выделенное. Добавление и
//! правка — диалог [`dialog`]. Правки — чистые функции
//! `application::use_cases::rules::edit`, сохранение и горячая перезагрузка
//! движка — в store.

pub mod dialog;
pub mod match_fields;
mod rewrite_dialog;
pub mod save_mock;
pub mod store;
mod table;
mod tabs;
mod throttle;
pub mod window;

use application::use_cases::rules::edit::{Tool, tool_enabled, without_rule};
use application::use_cases::rules::location_form::LocationForm;
use application::use_cases::rules::save_response_mock::SavedMock;
use domain::value_objects::RuleId;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Icon;
use gpui_kit::component::Root;
use gpui_kit::component::input::InputState;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement, Render, StatefulInteractiveElement as _, Styled,
    Subscription, Window, div, px,
};

use crate::ui::theme::components::NavItem;
use crate::ui::theme::icons::AppIcon;
use crate::ui::theme::{ToneName, glass};
use crate::ui::window_shell;
use store::RulesStore;

/// Вкладка окна Tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesTab {
    SslProxying,
    AllowList,
    DnsSpoofing,
    BlockList,
    MapLocal,
    MapRemote,
    Throttle,
    Rewrite,
    Breakpoints,
    Scripting,
}

impl RulesTab {
    pub const ALL: [RulesTab; 10] = [
        RulesTab::SslProxying,
        RulesTab::AllowList,
        RulesTab::DnsSpoofing,
        RulesTab::BlockList,
        RulesTab::MapLocal,
        RulesTab::MapRemote,
        RulesTab::Throttle,
        RulesTab::Rewrite,
        RulesTab::Breakpoints,
        RulesTab::Scripting,
    ];

    /// Инструмент правил за вкладкой.
    pub fn tool(self) -> Tool {
        match self {
            RulesTab::SslProxying => Tool::SslProxying,
            RulesTab::AllowList => Tool::AllowList,
            RulesTab::DnsSpoofing => Tool::DnsSpoofing,
            RulesTab::BlockList => Tool::BlockList,
            RulesTab::MapLocal => Tool::MapLocal,
            RulesTab::MapRemote => Tool::MapRemote,
            RulesTab::Throttle => Tool::Throttle,
            RulesTab::Rewrite => Tool::Rewrite,
            RulesTab::Breakpoints => Tool::Breakpoints,
            RulesTab::Scripting => Tool::Scripting,
        }
    }

    pub fn icon(self) -> AppIcon {
        match self {
            RulesTab::SslProxying => AppIcon::ShieldCheck,
            RulesTab::AllowList => AppIcon::Funnel,
            RulesTab::DnsSpoofing => AppIcon::Server,
            RulesTab::BlockList => AppIcon::Ban,
            RulesTab::MapLocal => AppIcon::FolderOpen,
            RulesTab::MapRemote => AppIcon::Globe,
            RulesTab::Throttle => AppIcon::SlidersHorizontal,
            RulesTab::Rewrite => AppIcon::Replace,
            RulesTab::Breakpoints => AppIcon::CirclePause,
            RulesTab::Scripting => AppIcon::FileCode,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RulesTab::SslProxying => "SSL Proxying",
            RulesTab::AllowList => "Allow List",
            RulesTab::DnsSpoofing => "DNS Spoofing",
            RulesTab::BlockList => "Block List",
            RulesTab::MapLocal => "Map Local",
            RulesTab::MapRemote => "Map Remote",
            RulesTab::Throttle => "Throttle",
            RulesTab::Rewrite => "Rewrite",
            RulesTab::Breakpoints => "Breakpoints",
            RulesTab::Scripting => "Scripting",
        }
    }
}

/// Поля своих значений throttle.
struct ThrottleInputs {
    down_kbps: Entity<InputState>,
    up_kbps: Entity<InputState>,
    latency_ms: Entity<InputState>,
}

/// Поля нового правила из трафика: локация и (Save Response & Map Local)
/// сохранённый мок.
#[derive(Debug, Clone)]
pub struct Prefill {
    pub location: LocationForm,
    pub mock: Option<SavedMock>,
}

impl From<LocationForm> for Prefill {
    fn from(location: LocationForm) -> Self {
        Self {
            location,
            mock: None,
        }
    }
}

pub struct RulesScreen {
    store: Entity<RulesStore>,
    tab: RulesTab,
    /// Выделенная строка текущей вкладки (сбрасывается при смене вкладки).
    selected: Option<RuleId>,
    throttle: ThrottleInputs,
    /// Ошибка своих значений throttle (до сохранения).
    throttle_error: Option<String>,
    /// Из трафика: открыть диалог добавления на ближайшем рендере (нужен Window).
    pending_prefill: Option<Prefill>,
    focus: FocusHandle,
    _observe: Subscription,
}

impl RulesScreen {
    pub fn new(store: Entity<RulesStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut input = |placeholder: &str| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let throttle = ThrottleInputs {
            down_kbps: input("e.g. 768"),
            up_kbps: input("e.g. 384"),
            latency_ms: input("e.g. 150"),
        };
        let _observe = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            store,
            tab: RulesTab::SslProxying,
            selected: None,
            throttle,
            throttle_error: None,
            pending_prefill: None,
            focus: cx.focus_handle(),
            _observe,
        }
    }

    /// Открыть вкладку и перечитать правила с диска; `prefill` — сразу
    /// открыть добавление с подставленными полями (правый клик в трафике).
    pub fn open_tab(
        &mut self,
        tab: RulesTab,
        prefill: Option<Prefill>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.switch_tab(tab);
        self.pending_prefill = prefill;
        self.focus.focus(window, cx);
        RulesStore::refresh(&self.store, cx);
        cx.notify();
    }

    fn switch_tab(&mut self, tab: RulesTab) {
        if self.tab != tab {
            self.selected = None;
        }
        self.tab = tab;
        self.throttle_error = None;
    }

    /// Delete / Backspace — удалить выделенное правило.
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Только когда фокус на самом экране: Backspace в поле ввода (диалог,
        // throttle) не должен удалять выделенное правило.
        if !self.focus.is_focused(window) {
            return;
        }
        let key = event.keystroke.key.as_str();
        if !matches!(key, "backspace" | "delete") {
            return;
        }
        if let Some(id) = self.selected.take() {
            RulesStore::apply(&self.store, cx, move |rules| Ok(without_rule(rules, id)));
            cx.notify();
        }
    }
}

impl Render for RulesScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(prefill) = self.pending_prefill.take() {
            self.open_prefilled_dialog(prefill, window, cx);
        }
        let weak = cx.entity().downgrade();
        let p = glass(cx);
        let rules = self.store.read(cx).rules.clone();
        let nav: Vec<AnyElement> = RulesTab::ALL
            .iter()
            .map(|&tab| {
                let weak = weak.clone();
                let item = NavItem::new(
                    ("tools-nav", tab as usize),
                    Icon::new(tab.icon()),
                    tab.label(),
                )
                .selected(tab == self.tab)
                .on_click(move |_, _, cx| {
                    let _ = weak.update(cx, |screen, cx| {
                        screen.switch_tab(tab);
                        cx.notify();
                    });
                });
                if tool_enabled(&rules, tab.tool()) {
                    item.badge("On", p.tone(ToneName::Green)).into_any_element()
                } else {
                    item.into_any_element()
                }
            })
            .collect();

        let content: AnyElement = match self.tab {
            RulesTab::Throttle => self.throttle_tab(cx),
            tab => self.tool_tab(tab, cx),
        };
        let error = self.store.read(cx).error.clone();
        let danger = cx.theme().danger;
        let body = div()
            // Прокручиваемая колонка; дети flex_none — длинный список
            // правил переполняет её, а не сжимается.
            .id("rules-content")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(16.))
            .when_some(error, |el, err| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(13.))
                        .text_color(danger)
                        .child(format!("⚠ {err}")),
                )
            })
            .child(div().flex_none().child(content))
            .into_any_element();
        let screen = div()
            .id("rules-screen")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .child(window_shell::shell(
                format!("Tools — {}", self.tab.label()),
                nav,
                body,
                cx,
            ));
        // gpui-component Root НЕ монтирует слой диалогов сам: без этого
        // Add…/Edit… открывали невидимый диалог. Слой — вне key-обработчика.
        div()
            .size_full()
            .child(screen)
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}
