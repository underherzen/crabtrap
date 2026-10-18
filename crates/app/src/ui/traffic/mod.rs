//! Экран трафика: полоса фильтра → таблица обменов → инспектор.
//!
//! Таблица и инспектор — две «стеклянные» панели в resizable-сплите:
//! инспектор под таблицей или справа от неё (`DetailStore.layout`). Тулбар окна (запись/пауза/очистка, поиск) — в
//! [`crate::ui::toolbar`], полоса результатов поиска — между фильтром и
//! таблицей.
//!
//! - [`store`] — TrafficStore (состояние строк, фильтр, статы).
//! - [`table`] — делегат виртуализированной таблицы gpui-kit `DataTable`.
//! - [`filter_bar`] — поле фильтра.
//! - [`search`] — поиск по заголовкам/телам: поле, results-полоса, ⌘F.

pub mod actions;
pub mod apps;
pub mod context_menu;
pub mod domains;
pub mod filter_bar;
pub mod quick_filter;
pub mod rows;
pub mod scope;
pub mod search;
pub mod store;
pub mod table;

use config::InspectorLayout;
use gpui_kit::base::{h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::input::InputState;
use gpui_kit::component::table::TableState;
use gpui_kit::{App, Entity, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px};

use crate::ui::detail::{DetailPane, DetailStore};
use crate::ui::theme::components::glass_panel;
use crate::ui::theme::glass;
use crate::ui::traffic::filter_bar::FilterBar;
use crate::ui::traffic::search::SearchStore;
use crate::ui::traffic::search::SearchStrip;
use crate::ui::traffic::store::TrafficStore;
use crate::ui::traffic::table::ExchangeTableDelegate;
use crate::ui::traffic::table::traffic_table;

/// Экран целиком: фильтр → полоса поиска → таблица → инспектор.
#[derive(gpui_kit::IntoElement)]
pub struct TrafficPane {
    traffic: Entity<TrafficStore>,
    table: Entity<TableState<ExchangeTableDelegate>>,
    filter_input: Entity<InputState>,
    search_input: Entity<InputState>,
    search: Entity<SearchStore>,
    detail: Entity<DetailStore>,
}

impl TrafficPane {
    pub fn new(
        traffic: Entity<TrafficStore>,
        table: Entity<TableState<ExchangeTableDelegate>>,
        filter_input: Entity<InputState>,
        search_input: Entity<InputState>,
        search: Entity<SearchStore>,
        detail: Entity<DetailStore>,
    ) -> Self {
        Self {
            traffic,
            table,
            filter_input,
            search_input,
            search,
            detail,
        }
    }
}

impl RenderOnce for TrafficPane {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = glass(cx);
        let table_panel = glass_panel(&p)
            .flex()
            .flex_col()
            .size_full()
            .child(FilterBar::new(self.traffic.clone(), self.filter_input))
            // Полоса результатов — только пока поиск активен (иначе пустой div).
            .child(SearchStrip::new(self.search, self.search_input))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .child(traffic_table(&self.table, &self.traffic)),
            );
        let layout = self.detail.read(cx).layout;
        let inspector_panel = glass_panel(&p)
            .size_full()
            .child(DetailPane::new(self.detail));
        let split = match layout {
            InspectorLayout::Bottom => v_resizable("traffic-split")
                .child(resizable_panel().child(div().size_full().pb(px(3.)).child(table_panel)))
                .child(
                    resizable_panel()
                        .size(px(320.))
                        .size_range(px(140.)..px(1600.))
                        .child(div().size_full().pt(px(3.)).child(inspector_panel)),
                )
                .into_any_element(),
            InspectorLayout::Right => h_resizable("traffic-split-right")
                .child(resizable_panel().child(div().size_full().pr(px(3.)).child(table_panel)))
                .child(
                    resizable_panel()
                        .size(px(560.))
                        .size_range(px(320.)..px(1600.))
                        .child(div().size_full().pl(px(3.)).child(inspector_panel)),
                )
                .into_any_element(),
        };
        div().size_full().min_h(px(0.)).child(split)
    }
}
