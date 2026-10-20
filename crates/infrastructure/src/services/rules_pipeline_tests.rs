//! Тесты RulesInterceptionPipeline: Block (код/обрыв, CONNECT), Map Local
//! (файл/каталог/`..`), Map Remote (замена origin/path/query, preserve_host),
//! горячая перезагрузка правил.

use super::*;
use application::services::InterceptableMessage;
use domain::entities::rule::{BlockRule, MapLocalRule};
use domain::value_objects::http_vo::HeaderEntry;
use domain::value_objects::{ExchangeId, RuleId, TrafficMatch};
use std::path::PathBuf;

fn host_match(glob: &str) -> TrafficMatch {
    TrafficMatch {
        host_glob: Some(glob.into()),
        ..Default::default()
    }
}

fn request(method: &str, uri: &str) -> InterceptContext {
    InterceptContext {
        exchange_id: ExchangeId::new(),
        phase: domain::entities::rule::BreakPhase::Request,
        message: InterceptableMessage {
            method: Some(method.into()),
            uri: Some(uri.into()),
            version: None,
            status: None,
            reason: None,
            headers: Vec::new(),
            body: None,
        },
        request: None,
    }
}

fn run<F: std::future::Future>(f: F) -> F::Output {
    futures::executor::block_on(f)
}

fn route_of(pipeline: &RulesInterceptionPipeline, method: &str, uri: &str) -> RouteDecision {
    match run(pipeline.on_request(request(method, uri))) {
        InterceptOutcome::Proceed { route, .. } => route,
        InterceptOutcome::Abort { mode, .. } => panic!("unexpected abort {mode:?}"),
    }
}

fn block(m: TrafficMatch, action: BlockAction) -> BlockRule {
    BlockRule {
        id: RuleId::new(),
        r#match: m,
        action,
        enabled: true,
    }
}

fn remote(remote: &str, preserve_host: bool) -> MapRemoteRule {
    MapRemoteRule {
        id: RuleId::new(),
        r#match: host_match("api.example.com"),
        remote: remote.into(),
        preserve_host,
        enabled: true,
    }
}

#[test]
fn block_with_status_routes_to_blocked() {
    let p = RulesInterceptionPipeline::new(RuleSet {
        blocks: vec![block(host_match("ads.com"), BlockAction::Status(451))],
        ..Default::default()
    });
    assert_eq!(
        route_of(&p, "GET", "http://ads.com/x"),
        RouteDecision::Blocked(451)
    );
    assert_eq!(
        route_of(&p, "GET", "http://ok.com/x"),
        RouteDecision::Origin
    );
}

#[test]
fn block_with_close_aborts() {
    let p = RulesInterceptionPipeline::new(RuleSet {
        blocks: vec![block(host_match("ads.com"), BlockAction::Close)],
        ..Default::default()
    });
    let outcome = run(p.on_request(request("GET", "https://ads.com/")));
    assert!(matches!(
        outcome,
        InterceptOutcome::Abort {
            mode: AbortMode::Close,
            tag: Some("blocked"),
        }
    ));
}

#[test]
fn method_condition_is_honored() {
    let p = RulesInterceptionPipeline::new(RuleSet {
        blocks: vec![block(
            TrafficMatch {
                host_glob: Some("api.com".into()),
                method: Some("POST".into()),
                ..Default::default()
            },
            BlockAction::Status(403),
        )],
        ..Default::default()
    });
    assert_eq!(
        route_of(&p, "POST", "http://api.com/"),
        RouteDecision::Blocked(403)
    );
    assert_eq!(
        route_of(&p, "GET", "http://api.com/"),
        RouteDecision::Origin
    );
}

#[test]
fn connect_to_host_only_blocked_host_is_blocked() {
    let p = RulesInterceptionPipeline::new(RuleSet {
        blocks: vec![block(host_match("*.ads.com"), BlockAction::Status(403))],
        ..Default::default()
    });
    let blocked = Host::new("x.ads.com").expect("host");
    let ok = Host::new("api.com").expect("host");
    assert_eq!(run(p.on_connect(&blocked, 443)), ConnectDecision::Block);
    assert_eq!(run(p.on_connect(&ok, 443)), ConnectDecision::Passthrough);
}

#[test]
fn map_local_relative_path_is_decoded_and_sanitized() {
    let rel = |p: &str| map_local_relative(p);
    assert_eq!(rel("/static/app.js"), Some(PathBuf::from("static/app.js")));
    assert_eq!(rel("/static/"), Some(PathBuf::from("static/index.html")));
    assert_eq!(rel("/"), Some(PathBuf::from("index.html")));
    assert_eq!(rel("/my%20file.json"), Some(PathBuf::from("my file.json")));
    assert_eq!(rel("/../../etc/passwd"), Some(PathBuf::from("etc/passwd")));
    assert_eq!(
        rel("/a/%2F..%2F/secret"),
        None,
        "encoded separators rejected"
    );
    assert_eq!(rel("/.env"), None, "dotfiles are not served");
    assert_eq!(rel("/.git/config"), None);
}

#[test]
fn map_local_route_carries_root_relative_path_and_response() {
    let root = PathBuf::from("/mocks");
    let p = RulesInterceptionPipeline::new(RuleSet {
        map_local: vec![MapLocalRule {
            id: RuleId::new(),
            r#match: host_match("a.com"),
            local_path: root.clone(),
            status: 503,
            content_type: Some("application/problem+json".into()),
            headers: vec![HeaderEntry::new("Retry-After", "5")],
            enabled: true,
        }],
        ..Default::default()
    });
    assert_eq!(
        route_of(&p, "GET", "http://a.com/v1/users.json?x=1"),
        RouteDecision::MapLocal {
            root,
            relative: Some(PathBuf::from("v1/users.json")),
            status: 503,
            content_type: Some("application/problem+json".into()),
            headers: vec![HeaderEntry::new("Retry-After", "5")],
        }
    );
}

#[test]
fn map_remote_replaces_origin_and_keeps_path_and_query() {
    let original = url::Url::parse("https://api.example.com/v1/users?id=7").expect("url");
    let target =
        map_remote_url(&remote("http://localhost:3000", false), &original).expect("mapped");
    assert_eq!(target.as_str(), "http://localhost:3000/v1/users?id=7");
}

#[test]
fn map_remote_with_path_and_query_overrides_them() {
    let original = url::Url::parse("https://api.example.com/v1/users?id=7").expect("url");
    let target = map_remote_url(
        &remote("https://staging.example.com/v2/mock", false),
        &original,
    )
    .expect("mapped");
    assert_eq!(target.as_str(), "https://staging.example.com/v2/mock?id=7");

    let with_query = map_remote_url(
        &remote("https://s.example.com/m?fixture=1", false),
        &original,
    )
    .expect("mapped");
    assert_eq!(with_query.as_str(), "https://s.example.com/m?fixture=1");
}

#[test]
fn map_remote_route_carries_preserve_host_and_invalid_remote_falls_back() {
    let p = RulesInterceptionPipeline::new(RuleSet {
        map_remote: vec![remote("https://staging.example.com", true)],
        ..Default::default()
    });
    assert_eq!(
        route_of(&p, "GET", "https://api.example.com/a"),
        RouteDecision::MapRemote {
            url: url::Url::parse("https://staging.example.com/a").expect("url"),
            preserve_host: true,
        }
    );

    let broken = RulesInterceptionPipeline::new(RuleSet {
        map_remote: vec![remote("not a url", false)],
        ..Default::default()
    });
    assert_eq!(
        route_of(&broken, "GET", "https://api.example.com/a"),
        RouteDecision::Origin
    );
}

#[test]
fn update_rules_hot_reloads() {
    let p = RulesInterceptionPipeline::new(RuleSet::default());
    assert_eq!(
        route_of(&p, "GET", "http://ads.com/"),
        RouteDecision::Origin
    );
    p.update_rules(RuleSet {
        blocks: vec![block(host_match("ads.com"), BlockAction::Status(403))],
        ..Default::default()
    });
    assert_eq!(
        route_of(&p, "GET", "http://ads.com/"),
        RouteDecision::Blocked(403)
    );
}

#[test]
fn unparsable_uri_routes_to_origin() {
    let p = RulesInterceptionPipeline::new(RuleSet {
        blocks: vec![block(TrafficMatch::default(), BlockAction::Status(403))],
        ..Default::default()
    });
    assert_eq!(route_of(&p, "GET", "::not a uri::"), RouteDecision::Origin);
}
