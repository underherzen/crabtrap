use super::*;
use application::dto::scripting::{ScriptRequestDto, ScriptResponseDto, ScriptSourceDto};
use domain::value_objects::http_vo::HeaderEntry;

fn call(source: &str, response: Option<ScriptResponseDto>) -> ScriptCall {
    ScriptCall {
        script: ScriptSourceDto {
            rule_id: RuleId::new(),
            name: "test".into(),
            source: source.into(),
        },
        exchange_id: None,
        request: ScriptRequestDto {
            method: "GET".into(),
            url: "https://api.test/items?page=1".into(),
            headers: vec![HeaderEntry {
                name: "accept".into(),
                value: "*/*".into(),
            }],
            body: None,
        },
        response,
    }
}

fn json_response() -> ScriptResponseDto {
    ScriptResponseDto {
        status: 200,
        headers: vec![HeaderEntry {
            name: "content-type".into(),
            value: "application/json".into(),
        }],
        body: Some(br#"{"items":[1,2]}"#.to_vec()),
    }
}

fn run(source: &str, response: Option<ScriptResponseDto>) -> ScriptRun {
    Worker::new()
        .expect("worker")
        .run(&call(source, response), &SharedState::default())
}

#[test]
fn on_request_edits_headers_and_queries() {
    let run = run(
        r#"async function onRequest(context, url, request) {
             request.headers["X-Script"] = context.scriptName;
             request.queries.page = "2";
             return request;
           }"#,
        None,
    );
    let Ok(ScriptOutput::Request(request)) = run.output else {
        panic!("{run:?}")
    };
    assert!(request.url.contains("page=2"), "{}", request.url);
    assert!(
        request
            .headers
            .iter()
            .any(|h| h.name == "X-Script" && h.value == "test")
    );
}

#[test]
fn on_response_edits_status_and_json_body() {
    let run = run(
        r#"function onResponse(context, url, request, response) {
             response.statusCode = 500;
             response.body.items.push(3);
             console.log("items", response.body.items);
             return response;
           }"#,
        Some(json_response()),
    );
    let Ok(ScriptOutput::Response(response)) = run.output else {
        panic!("{run:?}")
    };
    assert_eq!(response.status, 500);
    assert_eq!(response.body.as_deref(), Some(&br#"{"items":[1,2,3]}"#[..]));
    assert_eq!(run.logs.len(), 1);
    assert!(run.logs[0].message.starts_with("items ["), "{:?}", run.logs);
}

#[test]
fn mutating_without_return_still_applies() {
    let run = run(
        "function onResponse(c, u, req, res) { res.statusCode = 404; }",
        Some(json_response()),
    );
    let Ok(ScriptOutput::Response(response)) = run.output else {
        panic!("{run:?}")
    };
    assert_eq!(response.status, 404);
}

#[test]
fn missing_phase_function_is_not_defined() {
    let run = run("function onResponse(c, u, q, r) { return r; }", None);
    assert_eq!(run.output, Ok(ScriptOutput::NotDefined));
}

#[test]
fn syntax_and_runtime_errors_are_reported() {
    let syntax = run("function onRequest( {", None);
    assert!(syntax.output.is_err());
    assert_eq!(
        syntax.logs.last().map(|l| l.level),
        Some(ScriptLogLevel::Error)
    );

    let thrown = run("function onRequest() { throw new Error('boom'); }", None);
    assert!(thrown.output.unwrap_err().contains("boom"));
}

#[test]
fn infinite_loop_is_stopped_by_limit() {
    let run = run("function onRequest(c, u, r) { while (true) {} }", None);
    assert!(run.output.is_err(), "{run:?}");
}

#[test]
fn shared_state_persists_between_scripts() {
    let shared = SharedState::default();
    let mut worker = Worker::new().expect("worker");
    let first = worker.run(
        &call(
            "function onResponse(c,u,q,r){ $sharedState.token = 'abc'; return r; }",
            Some(json_response()),
        ),
        &shared,
    );
    assert!(first.output.is_ok(), "{first:?}");
    let second = worker.run(
        &call(
            "function onRequest(c,u,r){ r.headers.Authorization = 'Bearer ' + $sharedState.token; return r; }",
            None,
        ),
        &shared,
    );
    let Ok(ScriptOutput::Request(request)) = second.output else {
        panic!("{second:?}")
    };
    assert!(
        request.headers.iter().any(|h| h.value == "Bearer abc"),
        "{:?}",
        request.headers
    );
}

#[test]
fn scripts_do_not_share_globals() {
    let shared = SharedState::default();
    let mut worker = Worker::new().expect("worker");
    let first = call(
        "var leaked = 1; function onRequest(c,u,r){ return r; }",
        None,
    );
    assert!(worker.run(&first, &shared).output.is_ok());
    let second = call(
        "function onRequest(c,u,r){ r.headers.seen = typeof leaked; return r; }",
        None,
    );
    let Ok(ScriptOutput::Request(request)) = worker.run(&second, &shared).output else {
        panic!("second run")
    };
    assert!(
        request
            .headers
            .iter()
            .any(|h| h.name == "seen" && h.value == "undefined")
    );
}

fn big_numbers_response() -> ScriptResponseDto {
    ScriptResponseDto {
        status: 200,
        headers: vec![HeaderEntry {
            name: "content-type".into(),
            value: "application/json".into(),
        }],
        body: Some(br#"{ "ts": 1700000000000, "id": 1234567890123456789 }"#.to_vec()),
    }
}

#[test]
fn untouched_body_with_big_integers_stays_byte_identical() {
    let original = big_numbers_response();
    let run = run(
        "function onResponse(c,u,q,r){ r.headers['X-A'] = '1'; return r; }",
        Some(original.clone()),
    );
    let Ok(ScriptOutput::Response(response)) = run.output else {
        panic!("{run:?}")
    };
    assert_eq!(response.body, original.body);
}

#[test]
fn edited_body_keeps_integers_integral() {
    let run = run(
        "function onResponse(c,u,q,r){ r.body.extra = 1; return r; }",
        Some(big_numbers_response()),
    );
    let Ok(ScriptOutput::Response(response)) = run.output else {
        panic!("{run:?}")
    };
    let text = String::from_utf8(response.body.expect("body")).expect("utf8");
    assert!(text.starts_with(r#"{"ts":1700000000000,"#), "{text}");
    assert!(!text.contains(".0"), "{text}");
}

#[test]
fn never_settling_promise_is_an_error_not_a_hang() {
    let run = run(
        "async function onRequest(c,u,r){ await new Promise(() => {}); return r; }",
        None,
    );
    assert!(run.output.unwrap_err().contains("never settles"));
}

#[test]
fn rejected_promise_reports_the_reason() {
    let run = run(
        "async function onRequest(){ throw new Error('async boom'); }",
        None,
    );
    assert!(run.output.unwrap_err().contains("async boom"));
}

#[test]
fn too_deep_values_fail_safely() {
    let deep = r#"function onRequest(c,u,r){
        let o = {}; for (let i = 0; i < 100000; i++) { o = { o }; }
        console.log(o); $sharedState.deep = o; r.body = o; return r; }"#;
    let run = run(deep, None);
    assert!(
        run.output.unwrap_err().contains("nested too deeply"),
        "{:?}",
        run.logs
    );
}

#[test]
fn shared_state_visible_to_top_level_code_and_bad_state_warns() {
    let shared = SharedState::default();
    let mut worker = Worker::new().expect("worker");
    let first = call(
        "$sharedState.loaded = true; function onRequest(c,u,r){ $sharedState = 5; return r; }",
        None,
    );
    let run = worker.run(&first, &shared);
    assert!(run.output.is_ok(), "{run:?}");
    assert!(
        run.logs
            .iter()
            .any(|l| l.level == ScriptLogLevel::Warn && l.message.contains("object"))
    );
    assert!(
        shared.snapshot().is_empty(),
        "non-object state is not saved"
    );
}
