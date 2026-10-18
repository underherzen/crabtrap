use std::io::Write as _;
use std::time::Duration;

use super::*;
use crate::services::{BoaScriptRuntime, RulesInterceptionPipeline, RustBodyDecoder};
use domain::entities::rule::{BlockAction, BlockRule, MapRemoteRule};
use domain::value_objects::http_vo::HeaderEntry;
use domain::value_objects::{RuleId, TrafficMatch};

#[derive(Default)]
struct Recorder(Mutex<Vec<ProxyEvent>>);

impl EventPublisher for Recorder {
    fn publish(&self, event: ProxyEvent) {
        self.0.lock().expect("events").push(event);
    }
}

impl Recorder {
    fn tags(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("events")
            .iter()
            .filter_map(|e| match e {
                ProxyEvent::ExchangeTagged { tag, .. } => Some(tag.clone()),
                _ => None,
            })
            .collect()
    }

    fn logs(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("events")
            .iter()
            .filter_map(|e| match e {
                ProxyEvent::ScriptLog { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }
}

fn host(glob: &str) -> TrafficMatch {
    TrafficMatch {
        host_glob: Some(glob.into()),
        ..Default::default()
    }
}

fn script(source: &str) -> ScriptRule {
    ScriptRule {
        id: RuleId::new(),
        name: "test".into(),
        r#match: host("api.test"),
        enabled: true,
        run_on_request: true,
        run_on_response: true,
        mock_api: false,
        source: source.into(),
    }
}

fn pipeline(rules: RuleSet) -> (ScriptingInterceptionPipeline, Arc<Recorder>) {
    let events = Arc::new(Recorder::default());
    let inner = Arc::new(RulesInterceptionPipeline::new(rules.clone()));
    let deps = ScriptingDeps {
        runtime: Arc::new(BoaScriptRuntime::with(1, Duration::from_secs(10))),
        decoder: Arc::new(RustBodyDecoder::new()),
        events: events.clone(),
    };
    (
        ScriptingInterceptionPipeline::new(inner, rules, deps),
        events,
    )
}

fn header(name: &str, value: &str) -> HeaderEntry {
    HeaderEntry {
        name: name.into(),
        value: value.into(),
    }
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
            headers: vec![header("accept", "*/*")],
            body: None,
        },
        request: None,
    }
}

fn response(
    request: &InterceptContext,
    headers: Vec<HeaderEntry>,
    body: &[u8],
) -> InterceptContext {
    InterceptContext {
        exchange_id: request.exchange_id,
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
        request: Some(request.message.clone()),
    }
}

fn proceed(outcome: InterceptOutcome) -> (InterceptableMessage, RouteDecision) {
    match outcome {
        InterceptOutcome::Proceed { message, route } => (message, route),
        other => panic!("expected proceed, got {other:?}"),
    }
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).expect("gzip");
    encoder.finish().expect("gzip")
}

#[tokio::test(flavor = "multi_thread")]
async fn wants_body_only_for_matching_scripts() {
    let rules = RuleSet {
        scripts: vec![ScriptRule {
            run_on_request: false,
            ..script("")
        }],
        ..Default::default()
    };
    let (pipeline, _) = pipeline(rules);
    let matched = request("https://api.test/x");
    assert!(!pipeline.wants_body(&matched), "request phase is off");
    assert!(pipeline.wants_body(&response(&matched, Vec::new(), b"")));
    assert!(!pipeline.wants_body(&request("https://other.test/x")));
}

#[tokio::test(flavor = "multi_thread")]
async fn request_script_edits_and_map_remote_sees_edited_url() {
    let rules = RuleSet {
        scripts: vec![script(
            "function onRequest(c, url, r) { r.path = '/v2' + r.path; r.headers['X-Script'] = '1'; return r; }",
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
    let (pipeline, events) = pipeline(rules);
    let (message, route) = proceed(pipeline.on_request(request("https://api.test/items")).await);
    assert_eq!(message.uri.as_deref(), Some("https://api.test/v2/items"));
    assert!(message.headers.iter().any(|h| h.name == "X-Script"));
    let RouteDecision::MapRemote { url, .. } = route else {
        panic!("{route:?}")
    };
    assert_eq!(url.as_str(), "https://staging.test/v2/items");
    assert_eq!(events.tags(), vec!["scripted"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_request_never_reaches_the_script() {
    let rules = RuleSet {
        scripts: vec![script(
            "function onRequest(c,u,r){ console.log('ran'); return r; }",
        )],
        blocks: vec![BlockRule {
            id: RuleId::new(),
            r#match: host("api.test"),
            action: BlockAction::Status(403),
            enabled: true,
        }],
        ..Default::default()
    };
    let (pipeline, events) = pipeline(rules);
    let (_, route) = proceed(pipeline.on_request(request("https://api.test/x")).await);
    assert_eq!(route, RouteDecision::Blocked(403));
    assert!(events.logs().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn gzip_body_is_decoded_for_script_and_sent_plain_when_changed() {
    let rules = RuleSet {
        scripts: vec![script(
            "function onResponse(c,u,q,r){ console.log(r.body.n); r.body.n = 2; return r; }",
        )],
        ..Default::default()
    };
    let (pipeline, events) = pipeline(rules);
    let req = request("https://api.test/x");
    let headers = vec![
        header("content-type", "application/json"),
        header("content-encoding", "gzip"),
    ];
    let ctx = response(&req, headers, &gzip(br#"{"n":1}"#));
    let (message, _) = proceed(pipeline.on_response(ctx).await);
    assert_eq!(message.body.as_deref(), Some(&br#"{"n":2}"#[..]));
    assert!(
        !message
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("content-encoding"))
    );
    assert_eq!(events.logs(), vec!["1"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn untouched_gzip_body_stays_compressed() {
    let rules = RuleSet {
        scripts: vec![script(
            "function onResponse(c,u,q,r){ r.statusCode = 202; return r; }",
        )],
        ..Default::default()
    };
    let (pipeline, _) = pipeline(rules);
    let req = request("https://api.test/x");
    let compressed = gzip(b"hello");
    let headers = vec![
        header("content-type", "text/plain"),
        header("content-encoding", "gzip"),
    ];
    let (message, _) = proceed(
        pipeline
            .on_response(response(&req, headers, &compressed))
            .await,
    );
    assert_eq!(message.status, Some(202));
    assert_eq!(message.body, Some(compressed));
    assert!(message.headers.iter().any(|h| h.name == "content-encoding"));
}

#[tokio::test(flavor = "multi_thread")]
async fn mock_api_answers_and_skips_response_script() {
    let rules = RuleSet {
        scripts: vec![ScriptRule {
            mock_api: true,
            ..script(
                "function onResponse(c,u,q,r){ console.log('mock'); r.statusCode = 201; \
                 r.headers['Content-Type'] = 'application/json'; r.body = {ok: true}; return r; }",
            )
        }],
        ..Default::default()
    };
    let (pipeline, events) = pipeline(rules);
    let req = request("https://api.test/m");
    let (_, route) = proceed(pipeline.on_request(req.clone()).await);
    let RouteDecision::Respond {
        status,
        headers,
        body,
    } = route
    else {
        panic!("{route:?}")
    };
    assert_eq!(status, 201);
    assert_eq!(body, br#"{"ok":true}"#);
    assert!(headers.iter().any(|h| h.value == "application/json"));

    // Ответ mock'а проходит фазу ответа без повторного скрипта.
    let local = response(&req, headers, &body);
    assert!(!pipeline.wants_body(&local));
    proceed(pipeline.on_response(local).await);
    assert_eq!(events.logs(), vec!["mock"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn failing_script_leaves_exchange_unchanged_and_logs_error() {
    let rules = RuleSet {
        scripts: vec![script("function onRequest(){ throw new Error('nope'); }")],
        ..Default::default()
    };
    let (pipeline, events) = pipeline(rules);
    let ctx = request("https://api.test/x");
    let (message, route) = proceed(pipeline.on_request(ctx.clone()).await);
    assert_eq!(message, ctx.message);
    assert_eq!(route, RouteDecision::Origin);
    assert!(events.logs().iter().any(|l| l.contains("nope")));
    assert!(events.tags().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn hot_reload_replaces_scripts() {
    let (pipeline, _) = pipeline(RuleSet::default());
    let ctx = request("https://api.test/x");
    assert!(!pipeline.wants_body(&ctx));
    pipeline.update_rules(RuleSet {
        scripts: vec![script("function onRequest(c,u,r){ return r; }")],
        ..Default::default()
    });
    assert!(pipeline.wants_body(&ctx));
}

#[tokio::test(flavor = "multi_thread")]
async fn response_phase_uses_the_script_chosen_on_request() {
    let rules = RuleSet {
        scripts: vec![script(
            "function onRequest(c,u,r){ r.host = 'staging.test'; return r; } \
             function onResponse(c,u,q,r){ r.statusCode = 299; return r; }",
        )],
        ..Default::default()
    };
    let (pipeline, _) = pipeline(rules);
    let req = request("https://api.test/x");
    let (edited, _) = proceed(pipeline.on_request(req.clone()).await);
    assert_eq!(edited.uri.as_deref(), Some("https://staging.test/x"));
    // Итоговый запрос уже на другом хосте — правило всё равно то же.
    let ctx = InterceptContext {
        request: Some(edited),
        ..response(&req, vec![header("content-type", "text/plain")], b"ok")
    };
    assert!(pipeline.wants_body(&ctx));
    let (message, _) = proceed(pipeline.on_response(ctx).await);
    assert_eq!(message.status, Some(299));
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_content_encoding_without_body_change_keeps_it() {
    let rules = RuleSet {
        scripts: vec![script(
            "function onResponse(c,u,q,r){ delete r.headers['content-encoding']; return r; }",
        )],
        ..Default::default()
    };
    let (pipeline, _) = pipeline(rules);
    let req = request("https://api.test/x");
    let compressed = gzip(b"hello");
    let headers = vec![
        header("content-type", "text/plain"),
        header("content-encoding", "gzip"),
    ];
    let (message, _) = proceed(
        pipeline
            .on_response(response(&req, headers, &compressed))
            .await,
    );
    assert_eq!(message.body, Some(compressed));
    assert!(
        message
            .headers
            .iter()
            .any(|h| h.name == "content-encoding" && h.value == "gzip")
    );
}
