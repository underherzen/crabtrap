//! TrafficStore — состояние таблицы трафика (Phase 4).
//!
//! Entity живёт рядом с [`crate::ui::status_bar::EngineStatusStore`]: pump
//! (`crate::runtime::pump_events`) применяет пачки событий через
//! [`TrafficStore::apply_batch`], таблица читает строки делегатом.
//!
//! Применение событий вынесено в чистую функцию [`apply_event`] — она
//! тестируется без GPUI; сам store — тонкая обёртка с фильтром/статами.
//! Строки — кольцо [`RowBuffer`] (номера + индекс по id); вся работа на
//! пачку событий пропорциональна пачке, а не числу строк (10k+ строк при
//! непрерывном вытеснении не должны занимать UI-поток).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use application::dto::capture::{Direction, ExchangeSummaryDto, ProxyEvent};
use domain::value_objects::filter_query::FilterQuery;
use domain::value_objects::match_pattern::glob_match;
use domain::value_objects::{ConnectionId, ExchangeId};
use regex::Regex;

use super::apps::{AppCounts, AppNode, ClientApp};
use super::domains::{DomainCounts, DomainNode};
use super::quick_filter::QuickFilter;
use super::rows::RowBuffer;
use super::scope::{Scope, scope_matches};

/// Строка таблицы: DTO сводки + служебные поля, которых нет в DTO.
#[derive(Debug, Clone)]
pub struct TrafficRow {
    pub summary: ExchangeSummaryDto,
    /// Ответ получен (статус известен) — красим статус, а не «pending».
    pub responded: bool,
    /// Обмен оборвался/упал — красим строку и колонку Status.
    pub failed: Option<String>,
    /// Content-Type ответа (из ResponseHeaders) — для фильтра `type:`.
    pub content_type: Option<String>,
    /// Клиент соединения (приложение/устройство), когда он уже известен.
    pub client: Option<Arc<ClientApp>>,
    /// Сообщений WebSocket пришло (инспектор подгружает новые).
    pub ws_messages: usize,
}

impl TrafficRow {
    fn new(summary: ExchangeSummaryDto) -> Self {
        Self {
            summary,
            responded: false,
            failed: None,
            content_type: None,
            client: None,
            ws_messages: 0,
        }
    }
}

/// Что событие сделало со строками.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Unchanged,
    /// Новая строка с этим номером.
    Added(u64),
    /// Изменилась строка с этим номером.
    Updated(u64),
}

impl Applied {
    #[cfg(test)]
    pub fn changed(self) -> bool {
        self != Self::Unchanged
    }
}

/// Изменить строку обмена `id`; `f` возвращает, поменялось ли что-то.
fn update(
    rows: &mut RowBuffer,
    id: &ExchangeId,
    f: impl FnOnce(&mut TrafficRow) -> bool,
) -> Applied {
    let Some(seq) = rows.seq_of(id) else {
        return Applied::Unchanged;
    };
    match rows.by_seq_mut(seq).map(f) {
        Some(true) => Applied::Updated(seq),
        _ => Applied::Unchanged,
    }
}

/// Применить одно событие к строкам. Чистая функция (тесты без GPUI).
///
/// `paused = true` — новые обмены не добавляются (режим паузы UI);
/// события уже показанных строк продолжают применяться.
pub fn apply_event(rows: &mut RowBuffer, paused: bool, event: &ProxyEvent) -> Applied {
    match event {
        ProxyEvent::ExchangeStarted { exchange, .. } => {
            if paused || rows.contains(&exchange.id) {
                return Applied::Unchanged;
            }
            Applied::Added(rows.push(TrafficRow::new(exchange.clone())))
        }
        ProxyEvent::ResponseHeaders {
            id,
            status,
            headers,
            ..
        } => {
            let content_type = headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("content-type"))
                .map(|h| h.value.clone());
            update(rows, id, |row| {
                if row.summary.status == Some(*status)
                    && row.responded
                    && row.content_type == content_type
                {
                    return false;
                }
                row.summary.status = Some(*status);
                row.responded = true;
                row.content_type = content_type;
                true
            })
        }
        // WebSocket: размер ответа растёт с сообщениями (инспектор видит
        // изменение строки и перечитывает сообщения).
        ProxyEvent::WebSocketMessage { id, message } => update(rows, id, |row| {
            row.summary.resp_size += message.size;
            row.ws_messages += 1;
            true
        }),
        ProxyEvent::RequestEdited {
            id, method, path, ..
        } => update(rows, id, |row| {
            let changed = row.summary.method != *method || row.summary.path != *path;
            row.summary.method = method.clone();
            row.summary.path = path.clone();
            changed
        }),
        ProxyEvent::RequestBodyDone { id, raw_size, .. } => update(rows, id, |row| {
            let changed = row.summary.req_size != *raw_size;
            row.summary.req_size = *raw_size;
            changed
        }),
        // Живой размер во время стрима: req до ответа, resp после.
        ProxyEvent::BodyProgress {
            id,
            direction,
            bytes_so_far,
        } => update(rows, id, |row| {
            let field = match direction {
                Direction::Request => &mut row.summary.req_size,
                Direction::Response => &mut row.summary.resp_size,
            };
            let grew = *field < *bytes_so_far;
            if grew {
                *field = *bytes_so_far;
            }
            grew
        }),
        ProxyEvent::ExchangeCompleted {
            id,
            timing,
            raw_size,
            ..
        } => update(rows, id, |row| {
            row.summary.resp_size = *raw_size;
            row.summary.duration_ms = timing.total_ms;
            true
        }),
        ProxyEvent::ExchangeTagged { id, tag } => update(rows, id, |row| {
            if row.summary.tags.contains(tag) {
                return false;
            }
            row.summary.tags.push(tag.clone());
            true
        }),
        ProxyEvent::ExchangeFailed { id, error } => update(rows, id, |row| {
            let changed = row.failed.as_deref() != Some(error.as_str());
            row.failed = Some(error.clone());
            changed
        }),
        // Коннекты/статус движка/брейкпоинты таблицу не меняют.
        ProxyEvent::ConnectionOpened { .. }
        | ProxyEvent::ConnectionClosed { .. }
        | ProxyEvent::InterceptWaiting { .. }
        | ProxyEvent::InterceptResolved { .. }
        | ProxyEvent::ScriptLog { .. }
        | ProxyEvent::EngineStatus { .. }
        | ProxyEvent::ReverseProxyStatus { .. } => Applied::Unchanged,
    }
}

/// Сводка для статус-бара (bytes = суммарный объём увиденных обменов).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TrafficStatsView {
    pub exchanges: u64,
    pub bytes: u64,
    pub failed: u64,
}

/// Сколько клиентов соединений держим, прежде чем выбросить клиентов
/// соединений, чьих строк уже нет.
const MAX_TRACKED_CLIENTS: usize = 4096;

/// Видимые строки. Без фильтров список не материализуется вовсе; с
/// фильтром — номера (seq) подходящих строк по возрастанию, которые
/// поддерживаются инкрементально (новые/изменённые строки, вытеснение), а
/// целиком пересчитываются только при смене вида.
#[derive(Debug)]
enum Visible {
    All,
    Seqs(VecDeque<u64>),
}

/// Состояние таблицы (GPUI-Entity; поля публичны только для чтения).
pub struct TrafficStore {
    pub rows: RowBuffer,
    visible: Visible,
    /// Выбранный обмен (клик по строке → инспектор).
    pub selected: Option<ExchangeId>,
    /// Пауза UI: новые строки не добавляются (захват продолжается).
    pub paused: bool,
    /// Тумблер записи тел (отражение EngineCommand::SetBodyCapture).
    pub recording: bool,
    /// View-фильтр строки (host/path/status/method — subset FilterQuery).
    pub filter: FilterQuery,
    /// Транзиентный id-фильтр поиска (Phase 5): `Some` — таблица показывает
    /// только обмены из множества. Комбинируется с `filter` (пересечение),
    /// не заменяет его; сбрасывается `clear` и очисткой поиска.
    pub search_ids: Option<HashSet<ExchangeId>>,
    /// Выбор боковой панели (все / закреплённые / домен / раздел).
    pub scope: Scope,
    /// Закреплённые домены (персистятся в config.toml, см. `ui::sidebar`).
    pub pinned: BTreeSet<String>,
    /// Чипы над таблицей (протокол, тип содержимого).
    pub quick: QuickFilter,
    /// Дерево доменов для боковой панели (инкрементальные счётчики).
    domains: DomainCounts,
    /// Известные клиенты соединений (см. `ui::clients`).
    clients: HashMap<ConnectionId, Arc<ClientApp>>,
    /// Приложения для боковой панели (инкрементальные счётчики).
    apps: AppCounts,
    /// Растёт при каждой смене фильтра/чипов/выбора панели/поиска (не при
    /// новых событиях): таблица по нему переносит подсветку выбранной строки.
    pub view_generation: u64,
    /// Счётчики, живущие независимо от фильтра/паузы.
    pub stats: TrafficStatsView,
    /// Скомпилированный `filter.regex` (None — нет regex или он битый).
    filter_regex: Option<Regex>,
    /// Ошибка разбора фильтра (битый regex) — показывается у поля фильтра.
    pub filter_error: Option<String>,
    /// Максимум строк (= `capture.buffer_len`, как у ExchangeRepository):
    /// старейшие вытесняются, иначе в таблице висят обмены, детали которых
    /// репозиторий уже выбросил.
    capacity: usize,
}

impl TrafficStore {
    pub fn new(recording: bool) -> Self {
        Self {
            rows: RowBuffer::default(),
            visible: Visible::All,
            selected: None,
            paused: false,
            recording,
            filter: FilterQuery::default(),
            search_ids: None,
            scope: Scope::All,
            pinned: BTreeSet::new(),
            quick: QuickFilter::default(),
            domains: DomainCounts::default(),
            clients: HashMap::new(),
            apps: AppCounts::default(),
            view_generation: 0,
            stats: TrafficStatsView::default(),
            filter_regex: None,
            filter_error: None,
            capacity: usize::MAX,
        }
    }

    /// Ограничить число строк (ring buffer, как у репозитория обменов).
    pub fn with_capacity(self, capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            ..self
        }
    }

    /// Применить пачку событий pump'а; true — таблице нужно перерисоваться.
    ///
    /// Работа пропорциональна пачке, а не числу строк: новые строки
    /// добавляются в счётчики и видимый список, изменённые — перепроверяются
    /// фильтром, вытесненные — вычитаются.
    pub fn apply_batch(&mut self, events: &[ProxyEvent]) -> bool {
        let mut added: Vec<u64> = Vec::new();
        let mut updated: Vec<u64> = Vec::new();
        for event in events {
            // Статистика считается по всем событиям, даже на паузе.
            match event {
                ProxyEvent::ExchangeStarted { .. } => self.stats.exchanges += 1,
                ProxyEvent::RequestBodyDone { raw_size, .. } => self.stats.bytes += *raw_size,
                ProxyEvent::ExchangeCompleted { raw_size, .. } => self.stats.bytes += *raw_size,
                ProxyEvent::ExchangeFailed { .. } => self.stats.failed += 1,
                _ => {}
            }
            match apply_event(&mut self.rows, self.paused, event) {
                Applied::Added(seq) => added.push(seq),
                Applied::Updated(seq) => updated.push(seq),
                Applied::Unchanged => {}
            }
        }
        if added.is_empty() && updated.is_empty() {
            return false;
        }
        for &seq in &added {
            // Клиент соединения мог стать известен раньше её первого обмена.
            let Some(row) = self.rows.by_seq_mut(seq) else {
                continue;
            };
            row.client = self.clients.get(&row.summary.connection_id).cloned();
            let row = &*row;
            self.domains.add(row);
            self.apps.add(row);
        }
        self.evict_overflow();
        self.sync_visible(&added, &updated);
        self.domains.refresh();
        self.apps.refresh();
        true
    }

    /// Вытеснить старейшие строки сверх `capacity` (O(вытесненных)).
    fn evict_overflow(&mut self) {
        while self.rows.len() > self.capacity {
            let Some(row) = self.rows.pop_front() else {
                break;
            };
            self.domains.remove(&row);
            self.apps.remove(&row);
            if self.selected == Some(row.summary.id) {
                self.selected = None;
            }
        }
        if let Visible::Seqs(seqs) = &mut self.visible {
            let base = self.rows.base();
            while seqs.front().is_some_and(|&seq| seq < base) {
                seqs.pop_front();
            }
        }
    }

    /// Поправить видимый список после пачки (только при активном фильтре).
    fn sync_visible(&mut self, added: &[u64], updated: &[u64]) {
        if matches!(self.visible, Visible::All) {
            return;
        }
        let base = self.rows.base();
        // Изменённые строки могли начать или перестать подходить (статус,
        // Content-Type, провал).
        for &seq in updated
            .iter()
            .filter(|&&seq| seq >= base && !added.contains(&seq))
        {
            let matches = self
                .rows
                .by_seq(seq)
                .is_some_and(|row| self.row_visible(row));
            let Visible::Seqs(seqs) = &mut self.visible else {
                return;
            };
            match (seqs.binary_search(&seq), matches) {
                (Err(at), true) => seqs.insert(at, seq),
                (Ok(at), false) => {
                    seqs.remove(at);
                }
                _ => {}
            }
        }
        for &seq in added.iter().filter(|&&seq| seq >= base) {
            let matches = self
                .rows
                .by_seq(seq)
                .is_some_and(|row| self.row_visible(row));
            if let (true, Visible::Seqs(seqs)) = (matches, &mut self.visible) {
                seqs.push_back(seq);
            }
        }
    }

    fn row_visible(&self, row: &TrafficRow) -> bool {
        scope_matches(row, &self.scope, &self.pinned)
            && self.quick.matches(row)
            && row_matches(row, &self.filter, self.filter_regex.as_ref())
            && self
                .search_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&row.summary.id))
    }

    /// Сколько строк видно с текущим фильтром.
    pub fn visible_len(&self) -> usize {
        match &self.visible {
            Visible::All => self.rows.len(),
            Visible::Seqs(seqs) => seqs.len(),
        }
    }

    /// Позиции (в `rows`) видимых строк — для тестов; таблица пользуется
    /// [`Self::visible_len`]/[`Self::visible_row`].
    #[cfg(test)]
    pub fn visible_rows(&self) -> Vec<usize> {
        match &self.visible {
            Visible::All => (0..self.rows.len()).collect(),
            Visible::Seqs(seqs) => seqs
                .iter()
                .filter_map(|&seq| self.rows.position(seq))
                .collect(),
        }
    }

    /// Видимая строка по позиции в таблице.
    pub fn visible_row(&self, view_ix: usize) -> Option<&TrafficRow> {
        match &self.visible {
            Visible::All => self.rows.by_position(view_ix),
            Visible::Seqs(seqs) => self.rows.by_seq(*seqs.get(view_ix)?),
        }
    }

    /// Сменить фильтр целиком и пересобрать вид. Битый regex не прячет
    /// строки (иначе таблица «пустеет» на каждом недопечатанном символе),
    /// а уходит в `filter_error`.
    pub fn set_filter(&mut self, filter: FilterQuery) {
        if self.filter == filter {
            return;
        }
        let (filter_regex, filter_error) = match filter.regex.as_deref().map(Regex::new) {
            Some(Ok(re)) => (Some(re), None),
            Some(Err(e)) => (None, Some(format!("bad regex: {e}"))),
            None => (None, None),
        };
        self.filter = filter;
        self.filter_regex = filter_regex;
        self.filter_error = filter_error;
        self.view_changed();
    }

    /// Сменить только freetext-часть фильтра (прочие поля не трогает).
    #[cfg(test)]
    pub fn set_filter_text(&mut self, text: Option<String>) {
        let text = text.filter(|t| !t.is_empty());
        if self.filter.text == text {
            return;
        }
        self.filter.text = text;
        self.recompute_visible();
    }

    /// Транзиентный id-фильтр поиска: `None` — сброс. Композируется с
    /// `filter` (видно только то, что прошло ОБА условия).
    pub fn set_search_ids(&mut self, ids: Option<HashSet<ExchangeId>>) {
        if self.search_ids == ids {
            return;
        }
        self.search_ids = ids;
        self.view_changed();
    }

    /// Дерево доменов для боковой панели.
    pub fn domains(&self) -> &[DomainNode] {
        self.domains.snapshot()
    }

    /// Приложения-клиенты для боковой панели.
    pub fn apps(&self) -> &[AppNode] {
        self.apps.snapshot()
    }

    /// Клиенты соединений стали известны: подписать их строки.
    pub fn set_clients(&mut self, resolved: Vec<(ConnectionId, Arc<ClientApp>)>) {
        if resolved.is_empty() {
            return;
        }
        self.clients.extend(resolved);
        let mut touched = false;
        for row in self.rows.iter_mut().filter(|r| r.client.is_none()) {
            if let Some(client) = self.clients.get(&row.summary.connection_id) {
                row.client = Some(client.clone());
                self.apps.add(row);
                touched = true;
            }
        }
        self.prune_clients();
        if touched {
            self.apps.refresh();
            if matches!(self.scope, Scope::App(_)) {
                self.recompute_visible();
            }
        }
    }

    /// Не копить клиентов вытесненных строк бесконечно.
    fn prune_clients(&mut self) {
        if self.clients.len() <= MAX_TRACKED_CLIENTS {
            return;
        }
        let live: HashSet<ConnectionId> =
            self.rows.iter().map(|r| r.summary.connection_id).collect();
        self.clients.retain(|conn, _| live.contains(conn));
    }

    /// Сменить выбор боковой панели.
    pub fn set_scope(&mut self, scope: Scope) {
        if self.scope != scope {
            self.scope = scope;
            self.view_changed();
        }
    }

    /// Сменить набор закреплённых доменов.
    pub fn set_pinned(&mut self, pinned: BTreeSet<String>) {
        if self.pinned != pinned {
            self.pinned = pinned;
            // Вид меняется, только если показаны закреплённые.
            if self.scope == Scope::Pinned {
                self.view_changed();
            }
        }
    }

    /// Сменить чипы быстрого фильтра.
    pub fn set_quick(&mut self, quick: QuickFilter) {
        if self.quick != quick {
            self.quick = quick;
            self.view_changed();
        }
    }

    /// Вид пересобран не из-за новых событий (фильтр, чипы, панель, поиск).
    fn view_changed(&mut self) {
        self.view_generation += 1;
        self.recompute_visible();
    }

    /// Позиция выбранного обмена среди видимых строк (O(log n)).
    pub fn selected_view_ix(&self) -> Option<usize> {
        let seq = self.rows.seq_of(&self.selected?)?;
        match &self.visible {
            Visible::All => self.rows.position(seq),
            Visible::Seqs(seqs) => seqs.binary_search(&seq).ok(),
        }
    }

    fn recompute_visible(&mut self) {
        let unfiltered = self.scope == Scope::All
            && self.quick.is_empty()
            && self.filter.is_empty()
            && self.search_ids.is_none();
        self.visible = if unfiltered {
            Visible::All
        } else {
            Visible::Seqs(
                self.rows
                    .iter_seq()
                    .filter(|(_, row)| self.row_visible(row))
                    .map(|(seq, _)| seq)
                    .collect(),
            )
        };
    }

    /// Строка по id.
    pub fn row(&self, id: &ExchangeId) -> Option<&TrafficRow> {
        self.rows.get(id)
    }

    /// Очистить таблицу (кнопка Clear; сами тела чистит ClearTrafficUseCase).
    /// Сбрасывает и поисковый id-фильтр — результатов больше нет.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.selected = None;
        self.search_ids = None;
        self.domains.clear();
        self.apps.clear();
        self.clients.clear();
        self.stats = TrafficStatsView::default();
        self.view_changed();
    }
}

/// Строка для тестов соседних модулей (scope, domains).
#[cfg(test)]
pub fn test_row(host: &str, path: &str) -> TrafficRow {
    TrafficRow::new(ExchangeSummaryDto {
        id: ExchangeId::new(),
        connection_id: domain::value_objects::ConnectionId::new(),
        started_at: chrono::Utc::now(),
        method: "GET".into(),
        scheme: "http".into(),
        host: host.into(),
        path: path.into(),
        status: None,
        req_size: 0,
        resp_size: 0,
        duration_ms: 0.0,
        protocol: domain::value_objects::http_vo::Protocol::Http1,
        tls_mitm: None,
        tags: Vec::new(),
    })
}

/// View-фильтр строки: host-glob / method / status / Content-Type ответа /
/// regex по полному URL / freetext (каждое слово — подстрока host+path, AND).
/// `regex` — уже скомпилированный `filter.regex` (битый не передаётся).
pub fn row_matches(row: &TrafficRow, filter: &FilterQuery, regex: Option<&Regex>) -> bool {
    let s = &row.summary;
    if !filter.hosts.is_empty()
        && !filter
            .hosts
            .iter()
            .any(|glob| glob_match(glob, &s.host, false))
    {
        return false;
    }
    if !filter.methods.is_empty()
        && !filter
            .methods
            .iter()
            .any(|m| m.eq_ignore_ascii_case(&s.method))
    {
        return false;
    }
    if !filter.status_codes.is_empty() && !filter.status_codes.contains(&s.status.unwrap_or(0)) {
        return false;
    }
    if !filter.content_types.is_empty() {
        let content_type = row.content_type.as_deref().unwrap_or("").to_lowercase();
        if !filter
            .content_types
            .iter()
            .any(|ct| content_type.contains(&ct.to_lowercase()))
        {
            return false;
        }
    }
    if let Some(re) = regex
        && !re.is_match(&format!("{}://{}{}", s.scheme, s.host, s.path))
    {
        return false;
    }
    if let Some(text) = &filter.text {
        let haystack = format!("{}{}", s.host, s.path).to_lowercase();
        if !text
            .split_whitespace()
            .all(|word| haystack.contains(&word.to_lowercase()))
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
