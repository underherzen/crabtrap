use super::*;

fn header(name: &str, value: &str) -> HeaderEntry {
    HeaderEntry {
        name: name.into(),
        value: value.into(),
    }
}

fn request(url: &str, headers: Vec<HeaderEntry>, body: Option<&[u8]>) -> ScriptRequestDto {
    ScriptRequestDto {
        method: "POST".into(),
        url: url.into(),
        headers,
        body: body.map(<[u8]>::to_vec),
    }
}

fn json_response(body: &str) -> ScriptResponseDto {
    ScriptResponseDto {
        status: 200,
        headers: vec![header("content-type", "application/json; charset=utf-8")],
        body: Some(body.as_bytes().to_vec()),
    }
}

#[test]
fn request_object_has_url_parts_queries_and_headers() {
    let req = request(
        "https://api.test:8443/v1/items?page=1&tag=a&tag=b",
        vec![
            header("accept", "*/*"),
            header("cookie", "a=1"),
            header("cookie", "b=2"),
        ],
        None,
    );
    let js = request_to_js(&req);
    assert_eq!(js["scheme"], "https");
    assert_eq!(js["host"], "api.test");
    assert_eq!(js["port"], 8443);
    assert_eq!(js["path"], "/v1/items");
    assert_eq!(js["queries"]["page"], "1");
    assert_eq!(js["queries"]["tag"], json!(["a", "b"]));
    assert_eq!(js["headers"]["cookie"], json!(["a=1", "b=2"]));
    assert_eq!(js["body"], Value::Null);
}

#[test]
fn untouched_request_roundtrips_exactly() {
    let req = request(
        "http://a.test/x?b=2&a=1",
        vec![header("content-type", "application/json")],
        Some(br#"{ "z": 1,   "a": [1, 2] }"#),
    );
    assert_eq!(request_from_js(&req, &request_to_js(&req)), req);
}

#[test]
fn changed_query_rebuilds_url_unchanged_query_keeps_order() {
    let req = request("http://a.test/x?b=2&a=1", Vec::new(), None);
    let mut js = request_to_js(&req);
    js["queries"]["page"] = json!("3");
    let edited = request_from_js(&req, &js);
    assert!(edited.url.starts_with("http://a.test/x?"), "{}", edited.url);
    assert!(
        edited.url.contains("page=3") && edited.url.contains("b=2"),
        "{}",
        edited.url
    );

    let mut js = request_to_js(&req);
    js["method"] = json!("put");
    let edited = request_from_js(&req, &js);
    assert_eq!(edited.method, "PUT");
    assert_eq!(edited.url, "http://a.test/x?b=2&a=1", "query untouched");
}

#[test]
fn explicit_url_wins_over_parts() {
    let req = request("http://a.test/x", Vec::new(), None);
    let mut js = request_to_js(&req);
    js["url"] = json!("https://b.test/y");
    assert_eq!(request_from_js(&req, &js).url, "https://b.test/y");
}

#[test]
fn header_set_with_other_case_replaces_original() {
    let resp = json_response("{}");
    let mut js = response_to_js(&resp);
    js["headers"]["Content-Type"] = json!("text/plain");
    js["headers"]["X-New"] = json!(1);
    let edited = response_from_js(&resp, &js);
    let names: Vec<_> = edited
        .headers
        .iter()
        .map(|h| (h.name.as_str(), h.value.as_str()))
        .collect();
    assert_eq!(names, vec![("Content-Type", "text/plain"), ("X-New", "1")]);
}

#[test]
fn deleting_a_header_removes_it() {
    let resp = json_response("{}");
    let mut js = response_to_js(&resp);
    js["headers"]
        .as_object_mut()
        .expect("headers")
        .remove("content-type");
    assert!(response_from_js(&resp, &js).headers.is_empty());
}

#[test]
fn json_body_is_an_object_and_edits_serialize() {
    let resp = json_response(r#"{"user":{"id":1}}"#);
    let mut js = response_to_js(&resp);
    assert_eq!(js["body"]["user"]["id"], 1);
    js["body"]["user"]["name"] = json!("crab");
    js["statusCode"] = json!(201);
    let edited = response_from_js(&resp, &js);
    assert_eq!(edited.status, 201);
    let body: Value = serde_json::from_slice(edited.body.as_deref().expect("body")).expect("json");
    assert_eq!(body, json!({"user": {"id": 1, "name": "crab"}}));
}

#[test]
fn form_body_is_an_object_both_ways() {
    let req = request(
        "http://a.test/login",
        vec![header("Content-Type", "application/x-www-form-urlencoded")],
        Some(b"user=crab&pass=a+b"),
    );
    let mut js = request_to_js(&req);
    assert_eq!(js["body"]["pass"], "a b");
    js["body"]["pass"] = json!("secret");
    let body = request_from_js(&req, &js).body.expect("body");
    assert_eq!(body, b"user=crab&pass=secret");
}

#[test]
fn binary_body_is_base64_field_and_roundtrips() {
    let bytes: &[u8] = &[0xff, 0x00, 0xfe];
    let resp = ScriptResponseDto {
        status: 200,
        headers: vec![header("content-type", "image/png")],
        body: Some(bytes.to_vec()),
    };
    let js = response_to_js(&resp);
    assert_eq!(js["body"], Value::Null);
    assert_eq!(js[BODY_BASE64], "/wD+");
    assert_eq!(response_from_js(&resp, &js).body.as_deref(), Some(bytes));

    let mut replaced = js.clone();
    replaced[BODY_BASE64] = json!("AQI=");
    assert_eq!(
        response_from_js(&resp, &replaced).body.as_deref(),
        Some(&[1_u8, 2][..])
    );

    // Текст поверх двоичного тела — как текст, а не base64 («test» — валидный base64).
    let mut text = js.clone();
    text["body"] = json!("test");
    assert_eq!(
        response_from_js(&resp, &text).body.as_deref(),
        Some(&b"test"[..])
    );
}

#[test]
fn top_level_json_string_is_text() {
    let resp = json_response(r#""quoted""#);
    let js = response_to_js(&resp);
    assert_eq!(js["body"], r#""quoted""#);
}

#[test]
fn port_as_string_and_host_with_port() {
    let req = request("http://a.test/x", Vec::new(), None);
    let mut js = request_to_js(&req);
    js["port"] = json!("8081");
    assert_eq!(request_from_js(&req, &js).url, "http://a.test:8081/x");

    let mut js = request_to_js(&req);
    js["host"] = json!("b.test:3000");
    assert_eq!(request_from_js(&req, &js).url, "http://b.test:3000/x");

    let mut js = request_to_js(&req);
    js["port"] = json!("nope");
    assert_eq!(
        request_from_js(&req, &js).url,
        "http://a.test/x",
        "bad port keeps the original"
    );
}

#[test]
fn text_body_replaced_by_string_and_null_empties() {
    let resp = ScriptResponseDto {
        status: 200,
        headers: vec![header("content-type", "text/html")],
        body: Some(b"<p>hi</p>".to_vec()),
    };
    let mut js = response_to_js(&resp);
    js["body"] = json!("<p>bye</p>");
    assert_eq!(
        response_from_js(&resp, &js).body.as_deref(),
        Some(&b"<p>bye</p>"[..])
    );
    js["body"] = Value::Null;
    assert_eq!(response_from_js(&resp, &js).body.as_deref(), Some(&b""[..]));
}

#[test]
fn invalid_status_keeps_original() {
    let resp = json_response("{}");
    let mut js = response_to_js(&resp);
    js["statusCode"] = json!("oops");
    assert_eq!(response_from_js(&resp, &js).status, 200);
}
