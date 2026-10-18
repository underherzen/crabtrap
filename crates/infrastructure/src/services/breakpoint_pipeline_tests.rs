use super::*;
use crate::services::{RulesInterceptionPipeline, RustBodyDecoder};
use domain::value_objects::http_vo::HeaderEntry;
use domain::value_objects::{ExchangeId, RuleId, TrafficMatch};

#[derive(Default)]
struct Recorder(Mutex<Vec<ProxyEvent>>);

impl EventPublisher for Recorder {
    fn publish(&self, event: ProxyEvent) {
        self.0.lock().expect("events").push(event);
    }
}

impl Recorder {
    async fn waiting(&self) -> InterceptPreviewDto {
        for _ in 0..500 {
            let found = self.0.lock().expect("events").iter().find_map(|e| match e {
                ProxyEvent::InterceptWaiting { preview, .. } => Some(preview.clone()),
                _ => None,
            });
            if let Some(preview) = found {
                return preview;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("no InterceptWaiting");
    }

    fn resolved(&self) -> usize {
        self.0
            .lock()
            .expect("events")
            .iter()
            .filter(|e| matches!(e, ProxyEvent::InterceptResolved { .. }))
            .count()
    }
}

fn pipeline(
    phase: BreakPhase,
    timeout: Duration,
) -> (Arc<BreakpointInterceptionPipeline>, Arc<Recorder>) {
    let rules = RuleSet {
        breakpoints: vec![BreakpointRule {
            id: RuleId::new(),
            r#match: TrafficMatch {
                host_glob: Some("api.test".into()),
                ..Default::default()
            },
            phase,
            enabled: true,
        }],
        ..Default::default()
    };
    let events = Arc::new(Recorder::default());
    let inner = Arc::new(RulesInterceptionPipeline::new(rules.clone()));
    let pipeline = BreakpointInterceptionPipeline::new(
        inner,
        rules,
        events.clone(),
        Arc::new(RustBodyDecoder::new()),
    )
    .with_timeout(timeout);
    (Arc::new(pipeline), events)
}

fn request(uri: &str) -> InterceptContext {
    InterceptContext {
        exchange_id: ExchangeId::new(),
        phase: BreakPhase::Request,
        message: InterceptableMessage {
            method: Some("POST".into()),
            uri: Some(uri.into()),
            version: None,
            status: None,
            reason: None,
            headers: vec![HeaderEntry {
                name: "accept".into(),
                value: "*/*".into(),
            }],
            body: Some(b"original".to_vec()),
        },
        request: None,
    }
}

fn proceed(outcome: InterceptOutcome) -> InterceptableMessage {
    match outcome {
        InterceptOutcome::Proceed { message, .. } => message,
        other => panic!("expected proceed, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_from_the_window_are_applied() {
    let (pipeline, events) = pipeline(BreakPhase::Request, Duration::from_secs(10));
    let ctx = request("https://api.test/x");
    assert!(pipeline.wants_body(&ctx));
    let parked = tokio::spawn({
        let pipeline = pipeline.clone();
        async move { pipeline.on_request(ctx).await }
    });
    let preview = events.waiting().await;
    assert_eq!(preview.url, "https://api.test/x");
    assert_eq!(preview.message.body.as_deref(), Some(&b"original"[..]));
    let edited = InterceptableMessageDto {
        uri: Some("https://api.test/y".into()),
        body: Some(b"edited".to_vec()),
        headers: vec![HeaderEntry {
            name: "x-edited".into(),
            value: "1".into(),
        }],
        ..preview.message
    };
    pipeline.resolve_intercept(
        preview.intercept_id,
        InterceptResolutionDto::Resume {
            edited: Some(edited),
        },
    );
    let message = proceed(parked.await.expect("task"));
    assert_eq!(message.uri.as_deref(), Some("https://api.test/y"));
    assert_eq!(message.body.as_deref(), Some(&b"edited"[..]));
    assert_eq!(message.headers.len(), 1);
    assert_eq!(events.resolved(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn abort_closes_the_exchange() {
    let (pipeline, events) = pipeline(BreakPhase::Both, Duration::from_secs(10));
    let parked = tokio::spawn({
        let pipeline = pipeline.clone();
        async move { pipeline.on_request(request("https://api.test/x")).await }
    });
    let preview = events.waiting().await;
    pipeline.resolve_intercept(preview.intercept_id, InterceptResolutionDto::Abort);
    assert!(matches!(
        parked.await.expect("task"),
        InterceptOutcome::Abort { .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_resumes_unchanged_and_other_hosts_pass() {
    let (pipeline, events) = pipeline(BreakPhase::Request, Duration::from_millis(50));
    let ctx = request("https://api.test/x");
    let message = proceed(pipeline.on_request(ctx.clone()).await);
    assert_eq!(message, ctx.message);
    assert_eq!(events.resolved(), 1);

    let other = request("https://other.test/x");
    assert!(!pipeline.wants_body(&other));
    assert_eq!(
        proceed(pipeline.on_request(other.clone()).await),
        other.message
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn response_phase_breakpoint_uses_the_request_url() {
    let (pipeline, events) = pipeline(BreakPhase::Response, Duration::from_secs(10));
    let req = request("https://api.test/x");
    assert!(!pipeline.wants_body(&req), "request phase is not covered");
    let response = InterceptContext {
        phase: BreakPhase::Response,
        message: InterceptableMessage {
            method: None,
            uri: None,
            status: Some(200),
            body: Some(b"{}".to_vec()),
            ..req.message.clone()
        },
        request: Some(req.message.clone()),
        ..req
    };
    let parked = tokio::spawn({
        let pipeline = pipeline.clone();
        async move { pipeline.on_response(response).await }
    });
    let preview = events.waiting().await;
    let edited = InterceptableMessageDto {
        status: Some(418),
        ..preview.message
    };
    pipeline.resolve_intercept(
        preview.intercept_id,
        InterceptResolutionDto::Resume {
            edited: Some(edited),
        },
    );
    assert_eq!(proceed(parked.await.expect("task")).status, Some(418));
}
