//! Окно crabtrap (Liquid Glass): тулбар в тайтл-баре ([`toolbar`]),
//! боковая панель ([`sidebar`]), трафик ([`traffic`] + инспектор
//! [`detail`]), нижняя панель состояния ([`status_bar`]). Меню — в строке
//! меню macOS ([`app_menu`]); Tools и Settings — отдельные окна
//! ([`rules`], [`settings`]); цвета и компоненты — [`theme`].

pub mod app_menu;
pub mod async_bridge;
pub mod breakpoint;
pub mod certificates;
pub mod clients;
pub mod compose;
pub mod detail;
pub mod export;
pub mod group_box;
pub mod inspector;
pub mod network;
pub mod notify;
pub mod prefs;
pub mod rules;
pub mod scripting;
pub mod settings;
pub mod sidebar;
pub mod ssl_proxying;
pub mod status_bar;
pub mod system_proxy;
pub mod theme;
pub mod toolbar;
pub mod traffic;
pub mod updates;
pub mod window_shell;

use gpui_kit::component::TitleBar;
use gpui_kit::component::input::InputState;
use gpui_kit::component::table::TableState;
use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement, Render, Styled, Window,
    div, px,
};

use crate::app_state::AppState;
use detail::DetailStore;
use gpui_kit::base::{h_resizable, resizable_panel};
use sidebar::{Sidebar, SidebarStore};
use status_bar::{EngineStatusStore, StatusBar};
use theme::glass;
use theme::metrics::{SIDEBAR_WIDTH, TITLE_BAR_HEIGHT, TRAFFIC_LIGHTS_INSET};
use toolbar::MainToolbar;
use traffic::TrafficPane;
use traffic::filter_bar::FocusFilter;
use traffic::search::{FocusSearch, SearchStore};
use traffic::store::TrafficStore;
use traffic::table::ExchangeTableDelegate;

/// Корневой вид окна.
pub struct CrabtrapRoot {
    state: AppState,
    status: Entity<EngineStatusStore>,
    traffic: Entity<TrafficStore>,
    table: Entity<TableState<ExchangeTableDelegate>>,
    filter_input: Entity<InputState>,
    search_input: Entity<InputState>,
    search: Entity<SearchStore>,
    runtime: tokio::runtime::Handle,
    /// Панель деталей обмена (Phase 5).
    detail: Entity<DetailStore>,
    /// Системный прокси macOS (кнопка «macOS Proxy»).
    sys_proxy: Entity<system_proxy::SystemProxyStore>,
    /// SSL Proxying — MITM-тоггл (кнопка в тулбаре трафика).
    ssl_proxying: Entity<ssl_proxying::SslProxyingStore>,
    /// Боковая панель (домены, закреплённые).
    sidebar: Entity<SidebarStore>,
}

impl CrabtrapRoot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: AppState,
        status: Entity<EngineStatusStore>,
        traffic: Entity<TrafficStore>,
        table: Entity<TableState<ExchangeTableDelegate>>,
        filter_input: Entity<InputState>,
        search_input: Entity<InputState>,
        search: Entity<SearchStore>,
        runtime: tokio::runtime::Handle,
        detail: Entity<DetailStore>,
        sys_proxy: Entity<system_proxy::SystemProxyStore>,
        ssl_proxying: Entity<ssl_proxying::SslProxyingStore>,
        sidebar: Entity<SidebarStore>,
    ) -> Self {
        Self {
            state,
            status,
            traffic,
            table,
            filter_input,
            search_input,
            search,
            runtime,
            detail,
            sys_proxy,
            ssl_proxying,
            sidebar,
        }
    }
}

impl Render for CrabtrapRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = glass(cx);

        // Основная область: боковая панель + трафик.
        let traffic = div().size_full().pl(px(8.)).child(TrafficPane::new(
            self.traffic.clone(),
            self.table.clone(),
            self.filter_input.clone(),
            self.search_input.clone(),
            self.search.clone(),
            self.detail.clone(),
        ));
        let main_area = if self.sidebar.read(cx).visible {
            div().size_full().child(
                h_resizable("main-split")
                    .child(
                        resizable_panel()
                            .size(px(SIDEBAR_WIDTH))
                            .size_range(px(160.)..px(420.))
                            .child(
                                div()
                                    .size_full()
                                    .pl(px(8.))
                                    .child(Sidebar::new(self.sidebar.clone())),
                            ),
                    )
                    .child(resizable_panel().child(traffic)),
            )
        } else {
            traffic
        };

        // Тулбар в тайтл-баре: «светофоры» macOS слева, TitleBar двигает
        // окно и ловит двойной клик (zoom).
        let title_bar = TitleBar::new()
            .h(px(TITLE_BAR_HEIGHT))
            .pl(px(TRAFFIC_LIGHTS_INSET))
            .bg(gpui_kit::transparent_black())
            .border_color(gpui_kit::transparent_black())
            .child(MainToolbar {
                sidebar: self.sidebar.clone(),
                detail: self.detail.clone(),
                state: self.state.clone(),
                runtime: self.runtime.clone(),
                status: self.status.clone(),
                traffic: self.traffic.clone(),
                ssl_proxying: self.ssl_proxying.clone(),
                sys_proxy: self.sys_proxy.clone(),
                search: self.search.clone(),
                search_input: self.search_input.clone(),
            });

        // ⌘F — фокус поля поиска (биндинг в `main`, ловим на корневом div:
        // действие всплывает до корня из любого фокуса в окне).
        let search_input_for_action = self.search_input.clone();
        let filter_input_for_action = self.filter_input.clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .key_context(app_menu::MAIN_WINDOW_CONTEXT)
            .bg(p.window)
            .text_color(p.text)
            .on_action(move |_: &FocusSearch, window, cx| {
                search_input_for_action.update(cx, |input, cx| input.focus(window, cx));
            })
            .on_action(move |_: &FocusFilter, window, cx| {
                filter_input_for_action.update(cx, |input, cx| input.focus(window, cx));
            })
            .child(title_bar)
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .pr(px(8.))
                    .pb(px(8.))
                    .child(main_area),
            )
            .child(StatusBar::new(
                self.status.clone(),
                self.traffic.clone(),
                self.sys_proxy.clone(),
            ))
            // Root не монтирует слои сам: уведомления («About») и диалоги.
            .children(gpui_kit::component::Root::render_dialog_layer(window, cx))
            .children(gpui_kit::component::Root::render_notification_layer(
                window, cx,
            ))
    }
}
