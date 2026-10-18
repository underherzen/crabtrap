//! Вкладки инспектора обмена: Overview | Headers | Query | Body | Cookies |
//! Raw | Hex. Инспектор показывает запрос и ответ рядом — каждая вкладка
//! рисует одну сторону ([`Side`] в [`ExchangeContext`]).
//!
//! Этот файл задаёт общий контекст вкладок — [`ExchangeContext`] — и
//! регистрирует по компоненту на вкладку:
//! - `overview.rs` — сводка/тайминги/TLS/статус;
//! - `headers.rs` — заголовки request/response (таблицей);
//! - `raw.rs` — wire-байты (decoded-рендер заголовков + тело как есть);
//! - `hex.rs` — offset-страницы hex-дампа (через `BodyRepository::get_range`);
//! - `body.rs` — pretty-view: charset-детект, JSON/XML дерево, image preview;
//! - `cookies.rs` — Cookie/Set-Cookie через
//!   `application::dto::inspection::parse_cookies/parse_set_cookie`;
//! - `query.rs` — параметры query string запроса.
//!
//! Все вкладки — `RenderOnce`-компоненты с ЕДИНЫМ пропсом `ExchangeContext`
//! (клонируется в каждый рендер дёшево — всё внутри Arc/Handle).

pub mod body;
pub mod body_text;
pub mod cookies;
pub mod headers;
pub mod hex;
pub mod kv;
pub mod messages;
pub mod overview;
pub mod query;
pub mod raw;

use std::sync::Arc;

use application::dto::inspection::ExchangeDetailDto;
use application::services::BodyDecoderService;
use domain::repositories::BodyRepository;

use std::collections::HashMap;

use domain::value_objects::ExchangeId;
use gpui_kit::{App, Div, Entity, Global, ParentElement, Styled, div, px};

use crate::app_state::AppState;
use crate::ui::theme::Palette;
use crate::ui::theme::metrics::TEXT_SM;

/// Состояния вкладок одного типа для ТЕКУЩЕГО обмена (по стороне).
///
/// Сильные ссылки: элементы кадра Entity не держат (арена кадра чистится),
/// а слабая ссылка умирала бы между кадрами, пока у вкладки нет ни одного
/// обработчика клика — состояние пересоздавалось бы каждый кадр и загрузка
/// тела никогда не завершалась. Смена обмена выбрасывает старые записи.
struct TabStates<S: 'static>(HashMap<(ExchangeId, Side), Entity<S>>);

impl<S: 'static> Global for TabStates<S> {}

/// Как сбросить кэш каждого типа состояний, уже побывавшего в [`tab_state`].
#[derive(Default)]
struct TabStateCleaners(Vec<fn(&mut App)>);

impl Global for TabStateCleaners {}

fn forget<S: 'static>(cx: &mut App) {
    if cx.has_global::<TabStates<S>>() {
        cx.remove_global::<TabStates<S>>();
    }
}

/// Отпустить состояния вкладок (выбор снят, трафик очищен): иначе тело
/// последнего обмена (до десятков МБ) живёт в памяти до следующего выбора.
pub fn forget_tab_states(cx: &mut App) {
    let cleaners = cx
        .try_global::<TabStateCleaners>()
        .map(|c| c.0.clone())
        .unwrap_or_default();
    for clean in cleaners {
        clean(cx);
    }
}

/// Состояние вкладки для стороны `side` обмена `id` (создаёт `create`).
pub fn tab_state<S: 'static>(
    id: ExchangeId,
    side: Side,
    cx: &mut App,
    create: impl FnOnce(&mut App) -> Entity<S>,
) -> Entity<S> {
    let key = (id, side);
    if let Some(entity) = cx
        .try_global::<TabStates<S>>()
        .and_then(|c| c.0.get(&key).cloned())
    {
        return entity;
    }
    let entity = create(cx);
    if !cx.has_global::<TabStates<S>>() {
        cx.default_global::<TabStateCleaners>().0.push(forget::<S>);
    }
    let kept: HashMap<_, _> = cx
        .try_global::<TabStates<S>>()
        .map(|cache| {
            cache
                .0
                .iter()
                .filter(|((other, _), _)| *other == id)
                .map(|(k, v)| (*k, v.clone()))
                .collect()
        })
        .unwrap_or_default();
    let states = kept.into_iter().chain([(key, entity.clone())]).collect();
    cx.set_global(TabStates(states));
    entity
}

/// Заглушка по центру вкладки («нет тела», «ждём ответ»).
pub fn placeholder(message: impl Into<gpui_kit::SharedString>, p: &Palette) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(TEXT_SM))
        .text_color(p.text_secondary)
        .child(message.into())
}

/// Сторона обмена, которую рисует вкладка.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Side {
    #[default]
    Request,
    Response,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Request => "Request",
            Side::Response => "Response",
        }
    }
}

/// Общий контекст вкладки инспектора.
///
/// Выбран «плоский» пропс вместо `Entity<ExchangeCache>`: кэш байт/декодов
/// — внутреннее дело каждой вкладки (свои Entity по мере надобности),
/// а фундамент не должен навязывать стратегию кэша. `Arc<ExchangeDetailDto>`
/// — снапшот деталей на момент выбора строки; при обновлении обмена
/// DetailPane перезагружает снапшот целиком.
#[derive(Clone)]
pub struct ExchangeContext {
    /// Снапшот деталей выбранного обмена (Arc — дешёвый клон).
    pub detail: Arc<ExchangeDetailDto>,
    /// Тела по `BodyRef` (байты живут тут же, spill — в кэше на диске).
    pub body_repo: Arc<dyn BodyRepository>,
    /// Декомпрессия Content-Encoding: gzip/br/deflate (64 MiB guard внутри).
    pub decoder: Arc<dyn BodyDecoderService>,
    /// Tokio-хендл движка для D4-мостов (`ui::async_bridge::spawn_ui_task`).
    pub runtime: tokio::runtime::Handle,
    /// Какую сторону рисует вкладка (Overview — обмен целиком).
    pub side: Side,
    /// Сообщения WebSocket-обмена (подгружаются отдельно от деталей).
    pub websocket: Arc<Vec<domain::entities::websocket::WebSocketMessage>>,
}

impl ExchangeContext {
    pub fn new(
        detail: Arc<ExchangeDetailDto>,
        app: &AppState,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            body_repo: app.body_repository.clone(),
            decoder: app.body_decoder.clone(),
            detail,
            runtime,
            side: Side::Request,
            websocket: Arc::new(Vec::new()),
        }
    }

    /// С сообщениями WebSocket.
    pub fn with_websocket(
        self,
        websocket: Arc<Vec<domain::entities::websocket::WebSocketMessage>>,
    ) -> Self {
        Self { websocket, ..self }
    }

    /// Тот же контекст для другой стороны.
    pub fn on(self, side: Side) -> Self {
        Self { side, ..self }
    }
}
