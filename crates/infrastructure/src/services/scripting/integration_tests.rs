//! Скрипты целиком: сниппеты компилируются, пробный запуск из редактора
//! находит захваченный обмен и описывает правки.

use std::sync::Arc;
use std::time::Duration;

use application::use_cases::rules::location_form::LocationForm;
use application::use_cases::rules::script_edit::ScriptDraft;
use application::use_cases::scripting::snippets::SNIPPETS;
use application::use_cases::scripting::{TestScriptDeps, TestScriptUseCase};
use boa_engine::{Context, Source};
use chrono::Utc;
use domain::entities::body::BodyDescriptor;
use domain::entities::exchange::{HttpExchange, HttpRequest, HttpResponse};
use domain::repositories::{BodyRepository, ExchangeRepository};
use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
use domain::value_objects::{BodyId, ConnectionId, Host, RuleId};

use super::BoaScriptRuntime;
use crate::repositories::{InMemoryExchangeRepository, MemorySpillBodyStore};
use crate::services::RustBodyDecoder;

#[test]
fn every_snippet_is_valid_inside_a_handler() {
    let mut context = Context::default();
    for snippet in SNIPPETS {
        let code = format!(
            "(async function onResponse(context, url, request, response) {{\n{}\nreturn response; }})",
            snippet.code
        );
        assert!(
            context.eval(Source::from_bytes(&code)).is_ok(),
            "snippet '{}' does not parse",
            snippet.title
        );
    }
}

fn header(name: &str, value: &str) -> HeaderEntry {
    HeaderEntry {
        name: name.into(),
        value: value.into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_run_uses_latest_matching_exchange() {
    let exchanges = Arc::new(InMemoryExchangeRepository::new(16));
    let bodies = Arc::new(MemorySpillBodyStore::new(
        1024,
        u64::MAX,
        tempfile::tempdir().expect("tempdir").keep(),
    ));
    let body = bodies
        .put(
            BodyId::new(),
            br#"{"n":1}"#.to_vec(),
            BodyDescriptor::default(),
        )
        .await
        .expect("body");
    let mut exchange = HttpExchange::new_pending(
        ConnectionId::new(),
        Host::new("api.test").expect("host"),
        HttpRequest {
            method: Method::new("GET"),
            uri: "https://api.test/items?page=1".into(),
            version: HttpVersionVo::Http11,
            headers: vec![header("accept", "*/*")],
            body: None,
            raw_size: 0,
            origin: Default::default(),
        },
        Protocol::Http1,
        Utc::now(),
    );
    exchange.response = Some(HttpResponse {
        status: 200,
        reason: None,
        headers: vec![header("content-type", "application/json")],
        body: Some(body),
        raw_size: 0,
        version: HttpVersionVo::Http11,
    });
    exchanges.append(exchange).await.expect("append");

    let use_case = TestScriptUseCase::new(TestScriptDeps {
        exchanges,
        bodies,
        decoder: Arc::new(RustBodyDecoder::new()),
        runtime: Arc::new(BoaScriptRuntime::with(1, Duration::from_secs(10))),
    });
    let draft = ScriptDraft {
        name: "t".into(),
        location: LocationForm {
            host: "api.test".into(),
            path: "/items*".into(),
            method: None,
            ..LocationForm::default()
        },
        run_on_request: true,
        run_on_response: true,
        mock_api: false,
        source: "function onRequest(c,u,r){ r.headers['X-T'] = '1'; return r; } \
                 function onResponse(c,u,q,r){ console.log(r.body.n); r.statusCode = 201; return r; }"
            .into(),
    };
    let lines: Vec<String> = use_case
        .execute(RuleId::new(), &draft)
        .await
        .expect("report")
        .into_iter()
        .map(|l| l.message)
        .collect();
    assert!(
        lines[0].starts_with("Test on GET https://api.test/items?page=1"),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Request: headers added x-t".to_string()),
        "{lines:?}"
    );
    assert!(lines.contains(&"1".to_string()), "{lines:?}");
    assert!(
        lines.contains(&"Response: status 200 → 201".to_string()),
        "{lines:?}"
    );

    let other = ScriptDraft {
        location: LocationForm {
            host: "other.test".into(),
            ..draft.location.clone()
        },
        ..draft
    };
    assert!(use_case.execute(RuleId::new(), &other).await.is_err());
}
