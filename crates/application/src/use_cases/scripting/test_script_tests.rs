use super::*;

fn header(name: &str, value: &str) -> HeaderEntry {
    HeaderEntry {
        name: name.into(),
        value: value.into(),
    }
}

#[test]
fn request_changes_are_summarized() {
    let before = ScriptRequestDto {
        method: "GET".into(),
        url: "https://a.test/x".into(),
        headers: vec![header("accept", "*/*"), header("cookie", "a=1")],
        body: Some(Vec::new()),
    };
    let after = ScriptRequestDto {
        method: "POST".into(),
        headers: vec![header("Accept", "application/json"), header("x-new", "1")],
        body: Some(b"{}".to_vec()),
        ..before.clone()
    };
    let lines = describe_request(&before, &after);
    assert_eq!(
        lines,
        [
            "Request: method GET → POST; headers added x-new; removed cookie; changed accept; body 0 B → 2 B"
        ]
    );
    assert_eq!(describe_request(&before, &before), ["Request: unchanged"]);
}

#[test]
fn response_changes_are_summarized() {
    let before = ScriptResponseDto {
        status: 200,
        headers: Vec::new(),
        body: None,
    };
    let after = ScriptResponseDto {
        status: 500,
        body: Some(b"x".to_vec()),
        ..before.clone()
    };
    assert_eq!(
        describe_response("Response", &before, &after),
        ["Response: status 200 → 500; body not loaded → 1 B"]
    );
}
