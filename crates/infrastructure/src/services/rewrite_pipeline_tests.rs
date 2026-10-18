use std::io::Write as _;

use super::*;
use crate::services::{RulesInterceptionPipeline, RustBodyDecoder};
use application::services::RouteDecision;
use domain::entities::rule::{MapRemoteRule, RewriteLocation, RewriteOp};
use domain::value_objects::http_vo::HeaderEntry;
use domain::value_objects::{ExchangeId, RuleId, TrafficMatch};

fn h(name: &str, value: &str) -> HeaderEntry {
    HeaderEntry {
        name: name.into(),
        value: value.into(),
    }
}

fn rule(location: RewriteLocation, ops: Vec<RewriteOp>) -> RewriteRule {
    RewriteRule {
        id: RuleId::new(),
        name: "r".into(),
        enabled: true,
        location,
        r#match: Some(TrafficMatch {
            host_glob: Some("api.test".into()),
            ..Default::default()
        }),
        ops,
    }
}

fn pipeline(rules: RuleSet) -> RewriteInterceptionPipeline {
    let inner = Arc::new(RulesInterceptionPipeline::new(rules.clone()));
    RewriteInterceptionPipeline::new(inner, rules, Arc::new(RustBodyDecoder::new()))
}

fn request(uri: &str) -> InterceptContext {
    InterceptContext {
        exchange_id: ExchangeId::new(),
        phase: BreakPhase::Request,
        message: InterceptableMessage {
            method: Some("GET".into()),
            uri: Some(uri.into()),
            version: None,
            status: None,
            reason: None,
            headers: vec![h("accept", "*/*")],
            body: None,
        },
        request: None,
    }
}

fn response(req: &InterceptContext, headers: Vec<HeaderEntry>, body: &[u8]) -> InterceptContext {
    InterceptContext {
        exchange_id: req.exchange_id,
        phase: BreakPhase::Response,
        message: InterceptableMessage {
            method: None,
            uri: None,
            version: None,
            status: Some(200),
            reason: None,
            headers,
            body: Some(body.to_vec()),
        },
        request: Some(req.message.clone()),
    }
}

fn proceed(outcome: InterceptOutcome) -> (InterceptableMessage, RouteDecision) {
    match outcome {
        InterceptOutcome::Proceed { message, route } => (message, route),
        other => panic!("expected proceed, got {other:?}"),
    }
}

#[tokio::test]
async fn request_rewrite_runs_before_map_remote() {
    let rules = RuleSet {
        rewrite: vec![rule(
            RewriteLocation::Request,
            vec![
                RewriteOp::UrlRewrite {
                    find: "/v1/".into(),
                    replace: "/v2/".into(),
                    regex: false,
                },
                RewriteOp::AddHeader {
                    name: "X-Rewritten".into(),
                    value: "1".into(),
                },
            ],
        )],
        map_remote: vec![MapRemoteRule {
            id: RuleId::new(),
            r#match: TrafficMatch {
                host_glob: Some("api.test".into()),
                path_glob: Some("/v2/*".into()),
                ..Default::default()
            },
            remote: "https://staging.test".into(),
            preserve_host: false,
            enabled: true,
        }],
        ..Default::default()
    };
    let p = pipeline(rules);
    let ctx = request("https://api.test/v1/items");
    assert!(!p.wants_body(&ctx), "no body ops");
    let (message, route) = proceed(p.on_request(ctx).await);
    assert_eq!(message.uri.as_deref(), Some("https://api.test/v2/items"));
    assert!(message.headers.iter().any(|h| h.name == "X-Rewritten"));
    let RouteDecision::MapRemote { url, .. } = route else {
        panic!("{route:?}")
    };
    assert_eq!(url.as_str(), "https://staging.test/v2/items");
}

#[tokio::test]
async fn response_body_rewrite_decodes_gzip_and_sets_status() {
    let rules = RuleSet {
        rewrite: vec![rule(
            RewriteLocation::Response,
            vec![
                RewriteOp::BodyFindReplace {
                    find: "prod".into(),
                    replace: "test".into(),
                    regex: false,
                },
                RewriteOp::SetStatus { status: 299 },
            ],
        )],
        ..Default::default()
    };
    let p = pipeline(rules);
    let req = request("https://api.test/x");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(b"env=prod").expect("gzip");
    let headers = vec![
        h("content-type", "text/plain"),
        h("content-encoding", "gzip"),
    ];
    let ctx = response(&req, headers, &gz.finish().expect("gzip"));
    assert!(p.wants_body(&ctx));
    let (message, _) = proceed(p.on_response(ctx).await);
    assert_eq!(message.status, Some(299));
    assert_eq!(message.body.as_deref(), Some(&b"env=test"[..]));
    assert!(!message.headers.iter().any(|h| h.name == "content-encoding"));
}

#[tokio::test]
async fn unmatched_body_rewrite_keeps_original_bytes() {
    let rules = RuleSet {
        rewrite: vec![rule(
            RewriteLocation::Response,
            vec![RewriteOp::BodyFindReplace {
                find: "absent".into(),
                replace: "x".into(),
                regex: false,
            }],
        )],
        ..Default::default()
    };
    let p = pipeline(rules);
    let req = request("https://api.test/x");
    let ctx = response(&req, vec![h("content-type", "text/plain")], b"same");
    let (message, _) = proceed(p.on_response(ctx.clone()).await);
    assert_eq!(message, ctx.message);
}

#[tokio::test]
async fn rules_hot_reload() {
    let p = pipeline(RuleSet::default());
    let req = request("https://api.test/x");
    let ctx = response(&req, Vec::new(), b"");
    assert!(!p.wants_body(&ctx));
    p.update_rules(RuleSet {
        rewrite: vec![rule(
            RewriteLocation::Response,
            vec![RewriteOp::BodyFindReplace {
                find: "a".into(),
                replace: "b".into(),
                regex: false,
            }],
        )],
        ..Default::default()
    });
    assert!(p.wants_body(&ctx));
}
