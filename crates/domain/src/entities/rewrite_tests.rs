use super::*;
use crate::value_objects::{RuleId, TrafficMatch};

fn h(name: &str, value: &str) -> HeaderEntry {
    header(name, value)
}

fn rule(location: RewriteLocation, host: Option<&str>, ops: Vec<RewriteOp>) -> RewriteRule {
    RewriteRule {
        id: RuleId::new(),
        name: "r".into(),
        enabled: true,
        location,
        r#match: host.map(|host| TrafficMatch {
            host_glob: Some(host.into()),
            ..Default::default()
        }),
        ops,
    }
}

fn facts(host: &str) -> RequestFacts<'_> {
    RequestFacts {
        host,
        path: "/",
        method: "GET",
        ..Default::default()
    }
}

fn target() -> RewriteTarget {
    RewriteTarget {
        url: Some("https://api.test/v1/users?id=1".into()),
        status: None,
        headers: vec![h("Accept", "*/*"), h("Cookie", "a=1"), h("cookie", "b=2")],
        body: Some(b"hello world".to_vec()),
    }
}

#[test]
fn header_ops() {
    let r = rule(
        RewriteLocation::Request,
        None,
        vec![
            RewriteOp::AddHeader {
                name: "X-A".into(),
                value: "1".into(),
            },
            RewriteOp::ReplaceHeader {
                name: "cookie".into(),
                value: "c=3".into(),
            },
            RewriteOp::ReplaceHeader {
                name: "X-New".into(),
                value: "n".into(),
            },
            RewriteOp::RemoveHeader {
                name: "ACCEPT".into(),
            },
        ],
    );
    let out = r.apply(target());
    assert_eq!(
        out.headers,
        vec![h("Cookie", "c=3"), h("X-A", "1"), h("X-New", "n")]
    );
}

#[test]
fn body_and_url_text_and_regex() {
    let r = rule(
        RewriteLocation::Request,
        None,
        vec![
            RewriteOp::BodyFindReplace {
                find: "world".into(),
                replace: "crab".into(),
                regex: false,
            },
            RewriteOp::BodyFindReplace {
                find: r"(\w+) crab".into(),
                replace: "$1, crab!".into(),
                regex: true,
            },
            RewriteOp::UrlRewrite {
                find: "/v1/".into(),
                replace: "/v2/".into(),
                regex: false,
            },
            RewriteOp::UrlRewrite {
                find: r"id=(\d+)".into(),
                replace: "uid=$1".into(),
                regex: true,
            },
        ],
    );
    let out = r.apply(target());
    assert_eq!(out.body.as_deref(), Some(&b"hello, crab!"[..]));
    assert_eq!(out.url.as_deref(), Some("https://api.test/v2/users?uid=1"));
}

#[test]
fn status_only_on_response_and_binary_body_untouched() {
    let r = rule(
        RewriteLocation::Response,
        None,
        vec![
            RewriteOp::SetStatus { status: 503 },
            RewriteOp::BodyFindReplace {
                find: "a".into(),
                replace: "b".into(),
                regex: false,
            },
        ],
    );
    let request = r.apply(target());
    assert_eq!(request.status, None, "request has no status to set");
    let binary = RewriteTarget {
        status: Some(200),
        body: Some(vec![0xff, b'a']),
        ..target()
    };
    let out = r.apply(binary);
    assert_eq!(out.status, Some(503));
    assert_eq!(out.body, Some(vec![0xff, b'a']));
}

#[test]
fn matching_by_phase_host_status_and_switches() {
    let on_request = rule(RewriteLocation::Request, Some("api.test"), vec![]);
    let on_404 = RewriteRule {
        r#match: Some(TrafficMatch {
            status_code: Some(404),
            ..Default::default()
        }),
        ..rule(RewriteLocation::Response, None, vec![])
    };
    let disabled = RewriteRule {
        enabled: false,
        ..rule(RewriteLocation::Request, None, vec![])
    };
    let rules = RuleSet {
        rewrite: vec![on_request.clone(), on_404.clone(), disabled],
        ..Default::default()
    };
    assert_eq!(
        rules.rewrites_for(&facts("api.test"), None),
        vec![&on_request]
    );
    assert!(rules.rewrites_for(&facts("other.test"), None).is_empty());
    let not_found = ResponseFacts {
        status: 404,
        content_type: None,
    };
    assert_eq!(
        rules.rewrites_for(&facts("x"), Some(not_found)),
        vec![&on_404]
    );
    let ok = ResponseFacts {
        status: 200,
        content_type: None,
    };
    assert!(rules.rewrites_for(&facts("x"), Some(ok)).is_empty());

    let off = RuleSet {
        rewrite_enabled: false,
        ..rules
    };
    assert!(off.rewrites_for(&facts("api.test"), None).is_empty());
}
