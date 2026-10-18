//! Тесты [`super`] (TrafficStore и apply_event): вынесены, чтобы store.rs
//! оставался обозримым.

use super::*;
use chrono::Utc;
use domain::value_objects::http_vo::Protocol;
use domain::value_objects::timing::TimingPhases;

fn summary(host: &str, path: &str, method: &str) -> ExchangeSummaryDto {
    ExchangeSummaryDto {
        id: ExchangeId::new(),
        connection_id: domain::value_objects::ConnectionId::new(),
        started_at: Utc::now(),
        method: method.into(),
        scheme: "http".into(),
        host: host.into(),
        path: path.into(),
        status: None,
        req_size: 0,
        resp_size: 0,
        duration_ms: 0.0,
        protocol: Protocol::Http1,
        tls_mitm: None,
        tags: Vec::new(),
    }
}

fn started(s: ExchangeSummaryDto) -> ProxyEvent {
    ProxyEvent::ExchangeStarted {
        exchange: s,
        tls: None,
        request_headers: Vec::new(),
        request_version: Default::default(),
    }
}

fn fresh() -> (RowBuffer, ()) {
    (RowBuffer::default(), ())
}

/// Старый вид вызова в тестах: (строки, индекс, пауза, событие) → изменилось ли.
fn apply(rows: &mut RowBuffer, _index: &mut (), paused: bool, event: &ProxyEvent) -> bool {
    apply_event(rows, paused, event).changed()
}

#[test]
fn started_appends_then_headers_and_completed_update_in_place() {
    let (mut rows, mut index) = fresh();
    let s = summary("a.com", "/x", "GET");
    let id = s.id;
    assert!(apply(&mut rows, &mut index, false, &started(s)));
    assert_eq!(rows.len(), 1);
    assert!(rows[0].summary.status.is_none());

    assert!(apply(
        &mut rows,
        &mut index,
        false,
        &ProxyEvent::ResponseHeaders {
            id,
            status: 200,
            version: Default::default(),
            headers: Vec::new(),
            ts: Utc::now(),
        }
    ));
    assert_eq!(rows[0].summary.status, Some(200));
    assert!(rows[0].responded);

    assert!(apply(
        &mut rows,
        &mut index,
        false,
        &ProxyEvent::ExchangeCompleted {
            id,
            timing: TimingPhases {
                total_ms: 42.5,
                ..Default::default()
            },
            body: None,
            raw_size: 1024,
        }
    ));
    assert_eq!(rows[0].summary.resp_size, 1024);
    assert_eq!(rows[0].summary.duration_ms, 42.5);
    assert_eq!(rows.len(), 1, "no new row on completion");
}

#[test]
fn failed_marks_row() {
    let (mut rows, mut index) = fresh();
    let s = summary("b.com", "/boom", "POST");
    let id = s.id;
    apply(&mut rows, &mut index, false, &started(s));
    assert!(apply(
        &mut rows,
        &mut index,
        false,
        &ProxyEvent::ExchangeFailed {
            id,
            error: "upstream refused".into(),
        }
    ));
    assert_eq!(rows[0].failed.as_deref(), Some("upstream refused"));
}

#[test]
fn progress_grows_sizes_monotonically() {
    let (mut rows, mut index) = fresh();
    let s = summary("c.com", "/big", "GET");
    let id = s.id;
    apply(&mut rows, &mut index, false, &started(s));
    let prog = |dir, n| ProxyEvent::BodyProgress {
        id,
        direction: dir,
        bytes_so_far: n,
    };
    assert!(apply(
        &mut rows,
        &mut index,
        false,
        &prog(Direction::Request, 100)
    ));
    assert!(apply(
        &mut rows,
        &mut index,
        false,
        &prog(Direction::Request, 300)
    ));
    assert!(
        !apply(&mut rows, &mut index, false, &prog(Direction::Request, 50),),
        "regressing progress must not shrink the cell"
    );
    assert_eq!(rows[0].summary.req_size, 300);
}

#[test]
fn paused_skips_new_rows_but_updates_existing() {
    let (mut rows, mut index) = fresh();
    let s1 = summary("d.com", "/1", "GET");
    let id1 = s1.id;
    apply(&mut rows, &mut index, false, &started(s1));
    let s2 = summary("d.com", "/2", "GET");
    assert!(!apply(&mut rows, &mut index, true, &started(s2)));
    assert_eq!(rows.len(), 1, "paused view does not grow");
    // Существующая строка продолжает жить.
    assert!(apply(
        &mut rows,
        &mut index,
        true,
        &ProxyEvent::ResponseHeaders {
            id: id1,
            status: 201,
            version: Default::default(),
            headers: Vec::new(),
            ts: Utc::now(),
        }
    ));
    assert_eq!(rows[0].summary.status, Some(201));
}

#[test]
fn unknown_events_are_ignored_silently() {
    let (mut rows, mut index) = fresh();
    assert!(!apply(
        &mut rows,
        &mut index,
        false,
        &ProxyEvent::ExchangeFailed {
            id: ExchangeId::new(),
            error: "no such row".into(),
        }
    ));
    assert!(rows.is_empty());
}

#[test]
fn store_stats_count_all_even_paused() {
    let mut store = TrafficStore::new(true);
    let s = summary("e.com", "/s", "GET");
    let id = s.id;
    store.apply_batch(&[started(s)]);
    store.paused = true;
    let s2 = summary("e.com", "/t", "GET");
    store.apply_batch(&[
        started(s2),
        ProxyEvent::RequestBodyDone {
            id,
            body: None,
            raw_size: 10,
        },
        ProxyEvent::ExchangeCompleted {
            id,
            timing: TimingPhases::default(),
            body: None,
            raw_size: 90,
        },
    ]);
    assert_eq!(store.stats.exchanges, 2, "counter sees paused rows too");
    assert_eq!(store.stats.bytes, 100);
    assert_eq!(store.rows.len(), 1, "view stayed at pre-pause size");
}

#[test]
fn filter_narrows_by_text_host_method_status() {
    let mut store = TrafficStore::new(true);
    let mut a = summary("api.example.com", "/v1/users", "GET");
    a.status = Some(200);
    let mut b = summary("cdn.example.com", "/asset.js", "POST");
    b.status = Some(500);
    store.apply_batch(&[started(a), started(b)]);
    assert_eq!(store.visible_rows().len(), 2);

    store.set_filter_text(Some("users".into()));
    assert_eq!(store.visible_rows(), &[0]);

    store.set_filter(FilterQuery {
        hosts: vec!["cdn.*".into()],
        ..Default::default()
    });
    assert_eq!(store.visible_rows(), &[1]);

    store.set_filter(FilterQuery {
        methods: vec!["post".into()],
        ..Default::default()
    });
    assert_eq!(store.visible_rows(), &[1]);

    store.set_filter(FilterQuery {
        status_codes: vec![200],
        ..Default::default()
    });
    assert_eq!(store.visible_rows(), &[0]);
}

#[test]
fn capacity_evicts_oldest_rows_like_repository_ring() {
    let mut store = TrafficStore::new(true).with_capacity(3);
    let summaries: Vec<_> = (0..5)
        .map(|i| summary("c.com", &format!("/{i}"), "GET"))
        .collect();
    let ids: Vec<_> = summaries.iter().map(|s| s.id).collect();
    store.selected = None;
    store.apply_batch(
        &summaries[..2]
            .iter()
            .cloned()
            .map(started)
            .collect::<Vec<_>>(),
    );
    store.selected = Some(ids[0]);
    store.apply_batch(
        &summaries[2..]
            .iter()
            .cloned()
            .map(started)
            .collect::<Vec<_>>(),
    );

    let paths: Vec<_> = store.rows.iter().map(|r| r.summary.path.clone()).collect();
    assert_eq!(paths, vec!["/2", "/3", "/4"], "oldest evicted");
    assert!(
        store.row(&ids[0]).is_none(),
        "evicted row unreachable by id"
    );
    assert_eq!(
        store.row(&ids[4]).map(|r| r.summary.path.as_str()),
        Some("/4")
    );
    assert_eq!(store.visible_rows(), &[0, 1, 2]);
    assert!(
        store.selected.is_none(),
        "selection of evicted row is dropped"
    );

    // Индекс после вытеснения валиден: события по живым строкам применяются.
    store.apply_batch(&[ProxyEvent::ResponseHeaders {
        id: ids[3],
        status: 204,
        version: Default::default(),
        headers: Vec::new(),
        ts: Utc::now(),
    }]);
    assert_eq!(store.rows[1].summary.status, Some(204));
}

fn response_headers(id: ExchangeId, content_type: &str) -> ProxyEvent {
    ProxyEvent::ResponseHeaders {
        id,
        status: 200,
        version: Default::default(),
        headers: vec![application::dto::capture::HeaderEntryDto {
            name: "Content-Type".into(),
            value: content_type.into(),
        }],
        ts: Utc::now(),
    }
}

#[test]
fn filter_text_tokens_are_anded() {
    let mut store = TrafficStore::new(true);
    store.apply_batch(&[
        started(summary("api.example.com", "/v1/users", "GET")),
        started(summary("api.example.com", "/v1/orders", "GET")),
    ]);
    store.set_filter_text(Some("api users".into()));
    assert_eq!(store.visible_rows(), &[0]);
}

#[test]
fn filter_by_response_content_type() {
    let mut store = TrafficStore::new(true);
    let a = summary("a.com", "/data", "GET");
    let b = summary("a.com", "/logo", "GET");
    let (ida, idb) = (a.id, b.id);
    store.apply_batch(&[
        started(a),
        started(b),
        response_headers(ida, "application/json; charset=utf-8"),
        response_headers(idb, "image/png"),
    ]);
    store.set_filter(FilterQuery {
        content_types: vec!["JSON".into()],
        ..Default::default()
    });
    assert_eq!(store.visible_rows(), &[0]);
}

#[test]
fn filter_by_regex_over_full_url_and_invalid_regex_hides_nothing() {
    let mut store = TrafficStore::new(true);
    store.apply_batch(&[
        started(summary("api.example.com", "/v1/users", "GET")),
        started(summary("cdn.example.com", "/v1/users", "GET")),
    ]);
    store.set_filter(FilterQuery {
        regex: Some(r"^https?://api\.".into()),
        ..Default::default()
    });
    assert_eq!(store.visible_rows(), &[0]);
    assert!(store.filter_error.is_none());

    store.set_filter(FilterQuery {
        regex: Some("([".into()),
        ..Default::default()
    });
    assert_eq!(store.visible_rows(), &[0, 1], "broken regex is ignored");
    assert!(store.filter_error.is_some(), "but reported to the UI");
}

#[test]
fn tag_event_adds_tag_once() {
    let mut store = TrafficStore::new(true);
    let s = summary("t.com", "/", "GET");
    let id = s.id;
    let tag = || ProxyEvent::ExchangeTagged {
        id,
        tag: "map-local".into(),
    };
    store.apply_batch(&[started(s), tag(), tag()]);
    assert_eq!(store.rows[0].summary.tags, vec!["map-local".to_string()]);
}

#[test]
fn clear_resets_everything() {
    let mut store = TrafficStore::new(true);
    store.apply_batch(&[started(summary("f.com", "/", "GET"))]);
    store.selected = Some(store.rows[0].summary.id);
    store.clear();
    assert!(store.rows.is_empty());
    assert!(store.selected.is_none());
    assert_eq!(store.stats, TrafficStatsView::default());
}

#[test]
fn search_ids_narrow_and_compose_with_filter() {
    let mut store = TrafficStore::new(true);
    let a = summary("api.example.com", "/v1/users", "GET");
    let b = summary("cdn.example.com", "/asset.js", "GET");
    let c = summary("other.org", "/x", "GET");
    store.apply_batch(&[started(a), started(b), started(c)]);
    assert_eq!(store.visible_rows().len(), 3);

    let id_a = store.rows[0].summary.id;
    let id_b = store.rows[1].summary.id;
    store.set_search_ids(Some(HashSet::from([id_a, id_b])));
    assert_eq!(store.visible_rows(), &[0, 1]);

    // Пересечение с текстовым фильтром: «users» есть только в первой.
    store.set_filter_text(Some("users".into()));
    assert_eq!(store.visible_rows(), &[0]);

    // Снятие поиска не трогает текстовый фильтр.
    store.set_search_ids(None);
    assert_eq!(store.visible_rows(), &[0]);

    store.set_filter_text(None);
    assert_eq!(store.visible_rows(), &[0, 1, 2]);
}

#[test]
fn search_ids_hide_new_rows_outside_set() {
    let mut store = TrafficStore::new(true);
    let s = summary("e.com", "/1", "GET");
    let id = s.id;
    store.apply_batch(&[started(s)]);
    store.set_search_ids(Some(HashSet::from([id])));
    assert_eq!(store.visible_rows().len(), 1);

    // Новые обмены вне множества поиска скрыты, но копятся в rows.
    store.apply_batch(&[started(summary("e.com", "/2", "GET"))]);
    assert_eq!(store.rows.len(), 2);
    assert_eq!(store.visible_rows(), &[0]);
}

#[test]
fn scope_composes_with_filter_and_domains_track_new_rows() {
    let mut store = TrafficStore::new(true);
    store.apply_batch(&[
        started(summary("api.test", "/v1/users", "GET")),
        started(summary("cdn.test", "/app.js", "GET")),
        started(summary("api.test", "/v2/users", "POST")),
    ]);
    assert_eq!(store.domains().len(), 2);
    assert_eq!(store.domains()[0].count, 2);

    store.set_scope(Scope::Host("api.test".into()));
    assert_eq!(store.visible_rows(), &[0, 2]);
    store.set_filter(FilterQuery {
        methods: vec!["POST".into()],
        ..Default::default()
    });
    assert_eq!(store.visible_rows(), &[2]);

    store.set_filter(FilterQuery::default());
    store.set_scope(Scope::Pinned);
    assert!(store.visible_rows().is_empty(), "nothing pinned yet");
    store.set_pinned(BTreeSet::from(["cdn.test".to_string()]));
    assert_eq!(store.visible_rows(), &[1]);

    store.clear();
    assert!(store.domains().is_empty());
}

#[test]
fn view_generation_tracks_view_changes_not_events() {
    let mut store = TrafficStore::new(true);
    let a = summary("a.test", "/", "GET");
    let id = a.id;
    store.apply_batch(&[started(a), started(summary("b.test", "/", "GET"))]);
    let before = store.view_generation;
    store.apply_batch(&[started(summary("c.test", "/", "GET"))]);
    assert_eq!(
        store.view_generation, before,
        "new rows are not a view change"
    );
    store.selected = Some(id);
    assert_eq!(store.selected_view_ix(), Some(0));
    store.set_scope(Scope::Host("b.test".into()));
    assert!(store.view_generation > before);
    assert_eq!(store.selected_view_ix(), None, "selected row is hidden");
}

#[test]
fn clear_resets_search_ids() {
    let mut store = TrafficStore::new(true);
    let s = summary("f.com", "/", "GET");
    let id = s.id;
    store.apply_batch(&[started(s)]);
    store.set_search_ids(Some(HashSet::from([id])));
    store.clear();
    assert!(store.search_ids.is_none());

    store.apply_batch(&[started(summary("f.com", "/2", "GET"))]);
    assert_eq!(store.visible_rows().len(), 1, "после Clear поиска нет");
}

#[test]
fn clients_sign_existing_and_future_rows_of_their_connection() {
    let mut store = TrafficStore::new(true);
    let a = summary("a.test", "/", "GET");
    let conn = a.connection_id;
    store.apply_batch(&[started(a)]);
    let app = std::sync::Arc::new(crate::ui::traffic::apps::ClientApp {
        name: "Safari".into(),
        icon_key: None,
        remote: false,
    });
    store.set_clients(vec![(conn, app)]);
    assert_eq!(
        store.rows[0].client.as_ref().map(|c| c.name.as_str()),
        Some("Safari")
    );
    assert_eq!(store.apps().len(), 1);

    let mut b = summary("a.test", "/next", "GET");
    b.connection_id = conn;
    store.apply_batch(&[started(b)]);
    assert!(
        store.rows[1].client.is_some(),
        "keep-alive request of a known connection"
    );

    store.set_scope(Scope::App("Safari".into()));
    assert_eq!(store.visible_rows(), &[0, 1]);
    store.clear();
    assert!(store.apps().is_empty());
}

/// Детерминированный «случайный» поток событий: новые обмены, ответы с
/// разными статусами и Content-Type, провалы — с активным фильтром и
/// вытеснением. После каждой пачки инкрементальный вид и счётчики доменов
/// обязаны совпадать с полным пересчётом.
#[test]
fn incremental_view_matches_full_recompute_under_eviction() {
    let mut store = TrafficStore::new(true).with_capacity(40);
    store.set_filter(FilterQuery {
        status_codes: vec![200, 404],
        ..Default::default()
    });
    store.set_quick(
        crate::ui::traffic::quick_filter::QuickFilter::default()
            .toggle_kind(crate::ui::theme::semantic::ContentKind::Json),
    );
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let hosts = ["a.test", "b.test", "c.test"];
    let mut ids: Vec<ExchangeId> = Vec::new();
    for _ in 0..60 {
        let mut batch = Vec::new();
        for _ in 0..(next() % 12 + 1) {
            match next() % 4 {
                0 | 1 => {
                    let s = summary(
                        hosts[(next() % 3) as usize],
                        &format!("/p{}/x", next() % 4),
                        "GET",
                    );
                    ids.push(s.id);
                    batch.push(started(s));
                }
                2 if !ids.is_empty() => {
                    let id = ids[(next() as usize) % ids.len()];
                    let ct = if next() % 2 == 0 {
                        "application/json"
                    } else {
                        "text/html"
                    };
                    let status = [200, 404, 500][(next() % 3) as usize];
                    batch.push(ProxyEvent::ResponseHeaders {
                        id,
                        status,
                        version: Default::default(),
                        headers: vec![application::dto::capture::HeaderEntryDto {
                            name: "Content-Type".into(),
                            value: ct.into(),
                        }],
                        ts: Utc::now(),
                    });
                }
                _ if !ids.is_empty() => {
                    let id = ids[(next() as usize) % ids.len()];
                    batch.push(ProxyEvent::ExchangeFailed {
                        id,
                        error: "reset".into(),
                    });
                }
                _ => {}
            }
        }
        store.apply_batch(&batch);
        let incremental = store.visible_rows();
        let domains = store.domains().to_vec();
        store.recompute_visible();
        assert_eq!(incremental, store.visible_rows(), "visible rows drifted");
        let rows: Vec<TrafficRow> = store.rows.iter().cloned().collect();
        assert_eq!(
            domains,
            crate::ui::traffic::domains::build_domains(&rows),
            "domain counts drifted"
        );
        assert!(store.rows.len() <= 40);
    }
}

#[test]
fn selected_view_ix_follows_eviction_and_filter() {
    let mut store = TrafficStore::new(true).with_capacity(5);
    let summaries: Vec<_> = (0..8)
        .map(|i| summary("s.test", &format!("/{i}"), "GET"))
        .collect();
    let ids: Vec<_> = summaries.iter().map(|s| s.id).collect();
    store.apply_batch(
        &summaries[..4]
            .iter()
            .cloned()
            .map(started)
            .collect::<Vec<_>>(),
    );
    store.selected = Some(ids[3]);
    assert_eq!(store.selected_view_ix(), Some(3));
    store.apply_batch(
        &summaries[4..]
            .iter()
            .cloned()
            .map(started)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        store.selected_view_ix(),
        Some(0),
        "three older rows evicted"
    );
    store.set_filter(FilterQuery {
        text: Some("/7".into()),
        ..Default::default()
    });
    assert_eq!(store.selected_view_ix(), None, "hidden by filter");
    store.apply_batch(&[started(summary("s.test", "/9", "GET"))]);
    assert_eq!(store.visible_len(), 1);
}

#[test]
fn request_edited_updates_method_and_path_keeps_host() {
    let (mut rows, mut index) = fresh();
    let s = summary("a.com", "/a?x=1", "GET");
    let id = s.id;
    apply(&mut rows, &mut index, false, &started(s));
    let edited = ProxyEvent::RequestEdited {
        id,
        method: "PUT".into(),
        uri: "http://b.com/b?y=2".into(),
        path: "/b?y=2".into(),
        headers: Vec::new(),
    };
    assert!(apply(&mut rows, &mut index, false, &edited));
    assert_eq!(rows[0].summary.method, "PUT");
    assert_eq!(rows[0].summary.path, "/b?y=2");
    assert_eq!(
        rows[0].summary.host, "a.com",
        "row stays under the requested host"
    );
    assert!(
        !apply(&mut rows, &mut index, false, &edited),
        "same edit is a no-op"
    );
}
