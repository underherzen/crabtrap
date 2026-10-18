//! Интеграционные тесты движка: локальные origin-серверы на raw TCP +
//! клиент тоже raw — полный контроль над проводом (заголовки как байты).

use std::time::Duration;

use application::dto::capture::ProxyEvent;
use domain::repositories::BodyRepository as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use engine::EngineConfig;

mod common;
use common::*;

// ==== проводные помощники ====

struct WireResponse {
    status: u16,
    head: String,
    body: Vec<u8>,
}

/// Прочитать HTTP-ответ: голова до \r\n\r\n, тело по Content-Length
/// (или до EOF, если длины нет).
async fn read_response(stream: &mut TcpStream) -> WireResponse {
    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    let mut buf = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 8 * 1024];
    loop {
        if tokio::time::Instant::now() > deadline {
            panic!("timeout reading response head");
        }
        let head_end = find_head_end(&buf);
        if let Some(idx) = head_end {
            let head = String::from_utf8_lossy(&buf[..idx]).into_owned();
            let mut rest = buf.split_off(idx + 4);
            let status = head
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<u16>().ok())
                .expect("status code in status line");
            let content_length = head
                .lines()
                .find_map(|l| {
                    let (n, v) = l.split_once(':')?;
                    if n.eq_ignore_ascii_case("content-length") {
                        v.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(usize::MAX);
            while rest.len() < content_length {
                let n = tokio::time::timeout_at(deadline, stream.read(&mut chunk))
                    .await
                    .expect("body read timeout")
                    .expect("body read");
                if n == 0 {
                    break;
                }
                rest.extend_from_slice(&chunk[..n]);
            }
            return WireResponse {
                status,
                head,
                body: rest,
            };
        }
        let n = tokio::time::timeout_at(deadline, stream.read(&mut chunk))
            .await
            .expect("head read timeout")
            .expect("head read");
        if n == 0 {
            panic!(
                "EOF before response head (got {:?})",
                String::from_utf8_lossy(&buf)
            );
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Прочитать запрос-голову (до \r\n\r\n) + тело по Content-Length.
/// Возвращает (голова строкой, тело в байтах — длина важна для бинарных PUT).
async fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    let mut buf = Vec::with_capacity(8 * 1024);
    let mut chunk = [0u8; 64 * 1024];
    let head_end = loop {
        if let Some(idx) = find_head_end(&buf) {
            break idx;
        }
        let n = tokio::time::timeout_at(deadline, stream.read(&mut chunk))
            .await
            .expect("request head timeout")
            .expect("request head read");
        if n == 0 {
            panic!("EOF before request head");
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let content_length = head
        .lines()
        .find_map(|l| {
            let (n, v) = l.split_once(':')?;
            if n.eq_ignore_ascii_case("content-length") {
                v.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body = buf.split_off(head_end + 4);
    while body.len() < content_length {
        let n = tokio::time::timeout_at(deadline, stream.read(&mut chunk))
            .await
            .expect("request body timeout")
            .expect("request body read");
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    (head, body)
}

/// Origin, отвечающий canned-ответами; запросы отдаёт в канал.
async fn spawn_canned_origin(
    responses: Vec<Vec<u8>>,
) -> (std::net::SocketAddr, async_channel::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("origin bind");
    let addr = listener.local_addr().expect("origin addr");
    let (tx, rx) = async_channel::unbounded();
    tokio::spawn(async move {
        for response in responses {
            let (mut sock, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            let (head, body) = read_request(&mut sock).await;
            let mut raw = head.into_bytes();
            raw.extend_from_slice(b"\r\n\r\n");
            raw.extend_from_slice(&body);
            tx.send(raw).await.ok();
            sock.write_all(&response).await.ok();
            // Один запрос-один ответ-одно соединение: закрываем.
            sock.shutdown().await.ok();
        }
    });
    (addr, rx)
}

// ==== тесты ====

#[tokio::test(flavor = "multi_thread")]
async fn proxies_absolute_form_and_emits_exchange_events() {
    init_tracing();
    let body = b"hello from origin";
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    let (origin_addr, origin_rx) = spawn_canned_origin(vec![response]).await;

    let (engine_addr, events, cmd_tx) = spawn_engine().await;

    let mut client = TcpStream::connect(&engine_addr)
        .await
        .expect("connect engine");
    let request = format!(
        "GET http://{origin_addr}/base/path?q=1 HTTP/1.1\r\nHost: {origin_addr}\r\nUser-Agent: crabtrap-test\r\nConnection: close\r\n\r\n"
    );
    client
        .write_all(request.as_bytes())
        .await
        .expect("write req");

    let resp = read_response(&mut client).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, body);

    // Origin получил origin-form + Host, а не absolute-form.
    let seen = origin_rx.recv().await.expect("origin request");
    let seen = String::from_utf8_lossy(&seen);
    assert!(
        seen.starts_with("GET /base/path?q=1 HTTP/1.1\r\n"),
        "origin must receive origin-form, got: {seen}"
    );
    assert!(seen.contains(&format!("Host: {origin_addr}")));

    // События: Started → ResponseHeaders → Completed.
    let snapshot = events.snapshot();
    let started = snapshot
        .iter()
        .find_map(|e| match e {
            ProxyEvent::ExchangeStarted { exchange, .. } => Some(exchange.clone()),
            _ => None,
        })
        .expect("ExchangeStarted event");
    assert_eq!(started.method, "GET");
    assert_eq!(started.scheme, "http");
    assert_eq!(started.host, origin_addr.ip().to_string());
    assert_eq!(started.path, "/base/path?q=1");
    assert_eq!(started.connection_id.to_string().len(), 36); // uuid populated
    assert!(started.req_size > 0);

    assert!(
        snapshot
            .iter()
            .any(|e| matches!(e, ProxyEvent::ResponseHeaders { status: 200, .. }))
    );
    let completed = snapshot
        .iter()
        .find_map(|e| match e {
            ProxyEvent::ExchangeCompleted { raw_size, .. } => Some(*raw_size),
            _ => None,
        })
        .expect("ExchangeCompleted event");
    assert_eq!(completed, body.len() as u64);

    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::Stop)
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_tunnel_passthrough_echo() {
    // Echo-origin: всё, что пришло, возвращается назад.
    let echo = TcpListener::bind("127.0.0.1:0").await.expect("echo bind");
    let echo_addr = echo.local_addr().expect("echo addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = echo.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });

    let (engine_addr, events, cmd_tx) = spawn_engine().await;

    let mut client = TcpStream::connect(&engine_addr)
        .await
        .expect("connect engine");
    let connect = format!("CONNECT {echo_addr} HTTP/1.1\r\nHost: {echo_addr}\r\n\r\n");
    client
        .write_all(connect.as_bytes())
        .await
        .expect("write CONNECT");

    // Для CONNECT читаем только голову: после 200 соединение становится
    // туннелем, «тела ответа» не существует.
    let connect_head = read_head_only(&mut client).await;
    let status: u16 = connect_head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("CONNECT status code");
    assert_eq!(status, 200, "CONNECT must establish tunnel: {connect_head}");

    // Туннель работает в обе стороны (читаем ровно ожидаемое число байт —
    // один read может прийти частями).
    async fn read_n(stream: &mut TcpStream, n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        tokio::time::timeout(TEST_TIMEOUT, stream.read_exact(&mut out))
            .await
            .expect("tunnel read timeout")
            .expect("tunnel read");
        out
    }
    client.write_all(b"ping-1").await.expect("tunnel write");
    assert_eq!(read_n(&mut client, 6).await, b"ping-1".to_vec());

    client
        .write_all(b"ping-2-longer")
        .await
        .expect("tunnel write 2");
    assert_eq!(read_n(&mut client, 13).await, b"ping-2-longer".to_vec());

    // Обменов на туннеле нет, коннект — есть.
    let snapshot = events.snapshot();
    assert!(
        snapshot
            .iter()
            .any(|e| matches!(e, ProxyEvent::ConnectionOpened { .. }))
    );

    drop(client);
    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::Stop)
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn hop_by_hop_headers_are_stripped_both_ways() {
    // Ответ несёт Connection со списком токенов + сами эти заголовки:
    // клиент не должен получить ни Connection, ни X-Resp-Token.
    let body = b"ok";
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: X-Resp-Token\r\nX-Resp-Token: must-not-leak\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    let (origin_addr, origin_rx) = spawn_canned_origin(vec![response]).await;

    let (engine_addr, _events, cmd_tx) = spawn_engine().await;

    let mut client = TcpStream::connect(&engine_addr)
        .await
        .expect("connect engine");
    // Клиент НЕ шлёт Connection: close — иначе hyper-сервер движка
    // обязан ответить встречным `connection: close` (это корректный HTTP,
    // а не утечка hop-by-hop).
    let request = format!(
        "GET http://{origin_addr}/ HTTP/1.1\r\nHost: {origin_addr}\r\nProxy-Connection: keep-alive\r\nKeep-Alive: timeout=30\r\nProxy-Authorization: Basic zzz\r\n\r\n"
    );
    client
        .write_all(request.as_bytes())
        .await
        .expect("write req");

    let resp = read_response(&mut client).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, body);

    // Клиент не получил hop-by-hop ответа.
    for header in [
        "connection:",
        "x-resp-token:",
        "proxy-connection:",
        "keep-alive:",
    ] {
        assert!(
            !resp.head.to_ascii_lowercase().contains(header),
            "client must not receive {header}: {}",
            resp.head
        );
    }

    // Origin не получил hop-by-hop запроса.
    let seen = origin_rx.recv().await.expect("origin request");
    let seen = String::from_utf8_lossy(&seen);
    let lower = seen.to_ascii_lowercase();
    for header in [
        "proxy-connection:",
        "keep-alive:",
        "proxy-authorization:",
        "connection:",
    ] {
        assert!(
            !lower.contains(header),
            "origin must not receive {header}: {seen}"
        );
    }
    assert!(lower.contains(&format!("host: {origin_addr}")));

    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::Stop)
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_large_bodies_without_aggregation() {
    // 8 MiB ответ + 2 MiB запрос: контроль целостности и отсутствия
    // таймаутов (стриминговая семантика, без агрегации в памяти).
    const RESP_LEN: usize = 8 * 1024 * 1024;
    const REQ_LEN: usize = 2 * 1024 * 1024;

    // Источник данных: предсказуемый паттерн (i mod 251).
    fn pattern_byte(i: usize) -> u8 {
        (i % 251) as u8
    }
    let resp_head = format!("HTTP/1.1 200 OK\r\nContent-Length: {RESP_LEN}\r\n\r\n").into_bytes();

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stream origin bind");
    let origin_addr = listener.local_addr().expect("stream origin addr");
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<usize>();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("stream origin accept");
        let (_head, req_body) = read_request(&mut sock).await;
        let req_len = req_body.len();
        sock.write_all(&resp_head).await.expect("resp head");
        let mut chunk = vec![0u8; 16 * 1024];
        let mut sent = 0usize;
        while sent < RESP_LEN {
            let take = chunk.len().min(RESP_LEN - sent);
            for (k, b) in chunk[..take].iter_mut().enumerate() {
                *b = pattern_byte(sent + k);
            }
            sock.write_all(&chunk[..take]).await.expect("resp chunk");
            sent += take;
        }
        sock.shutdown().await.ok();
        let _ = done_tx.send(req_len);
    });

    let (engine_addr, events, cmd_tx) = spawn_engine().await;

    // PUT с большим телом через прокси.
    let mut client = TcpStream::connect(&engine_addr)
        .await
        .expect("connect engine");
    let request_head = format!(
        "PUT http://{origin_addr}/upload HTTP/1.1\r\nHost: {origin_addr}\r\nContent-Length: {REQ_LEN}\r\n\r\n"
    );
    client
        .write_all(request_head.as_bytes())
        .await
        .expect("req head");
    let payload: Vec<u8> = (0..REQ_LEN).map(|i| ((i * 7 + 3) % 253) as u8).collect();
    for piece in payload.chunks(64 * 1024) {
        client.write_all(piece).await.expect("req chunk");
    }

    let resp = tokio::time::timeout(TEST_TIMEOUT * 3, read_response(&mut client))
        .await
        .expect("streamed response must finish without aggregation timeout");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body.len(), RESP_LEN, "body must arrive complete");
    assert!(
        resp.body
            .iter()
            .enumerate()
            .all(|(i, b)| *b == pattern_byte(i)),
        "body integrity"
    );

    // Origin получил всё тело запроса.
    let origin_seen = tokio::time::timeout(TEST_TIMEOUT, done_rx)
        .await
        .expect("origin done")
        .expect("origin result");
    assert_eq!(
        origin_seen, REQ_LEN,
        "origin must receive full request body"
    );

    // Событие Completed с точным размером тела ответа.
    let completed = events
        .snapshot()
        .iter()
        .find_map(|e| match e {
            ProxyEvent::ExchangeCompleted { raw_size, .. } => Some(*raw_size),
            _ => None,
        })
        .expect("ExchangeCompleted");
    assert_eq!(completed, RESP_LEN as u64);

    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::Stop)
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn origin_form_direct_hit_answers_running_banner() {
    let (engine_addr, _events, cmd_tx) = spawn_engine().await;

    let mut client = TcpStream::connect(&engine_addr)
        .await
        .expect("connect engine");
    let request =
        format!("GET /status HTTP/1.1\r\nHost: {engine_addr}\r\nConnection: close\r\n\r\n");
    client
        .write_all(request.as_bytes())
        .await
        .expect("write req");

    let resp = read_response(&mut client).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"crabtrap proxy is running");

    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::Stop)
        .await
        .ok();
}

// ==== Phase 4: tee-захват тел (интеграционные) ====

/// Полный прогон: origin с одним ответом, клиент шлёт POST с телом,
/// возвращает (ответ клиенту, канал seen-запросов origin).
async fn drive_post(
    engine_addr: &str,
    origin: (std::net::SocketAddr, async_channel::Receiver<Vec<u8>>),
    req_body: &[u8],
) -> WireResponse {
    let (origin_addr, origin_rx) = origin;
    let mut client = TcpStream::connect(engine_addr)
        .await
        .expect("connect engine");
    let request = format!(
        "POST http://{origin_addr}/submit HTTP/1.1\r\nHost: {origin_addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        req_body.len()
    );
    client
        .write_all(request.as_bytes())
        .await
        .expect("write head");
    client.write_all(req_body).await.expect("write body");
    let resp = read_response(&mut client).await;
    drop(client);
    let _ = origin_rx;
    resp
}

#[tokio::test(flavor = "multi_thread")]
async fn body_capture_stores_request_and_response_with_descriptor() {
    init_tracing();
    // Запрос: 32 байта JSON (ниже spill-порога 1024 → память).
    let req_body: Vec<u8> = br#"{"hello":"world","n":42}"#.to_vec();
    // Ответ: 2 KiB text/plain (выше порога → spill на диск).
    let resp_body = vec![b'x'; 2048];
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
        resp_body.len()
    )
    .into_bytes();
    response.extend_from_slice(&resp_body);
    let origin = spawn_canned_origin(vec![response]).await;

    let bodies = body_store();
    let cfg = EngineConfig::new("127.0.0.1:0", 16).with_body_capture(true, 1024 * 1024);
    let (engine_addr, events, cmd_tx) = spawn_engine_with(cfg, bodies.clone()).await;

    let resp = drive_post(&engine_addr, origin, &req_body).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body.len(), 2048, "client receives full body");

    // Тело запроса сохранено с дескриптором из заголовков.
    let req_ref = wait_event(&events, |e| match e {
        ProxyEvent::RequestBodyDone { body: Some(b), .. } => Some(b.clone()),
        _ => None,
    })
    .await;
    assert_eq!(req_ref.size, req_body.len() as u64);
    assert_eq!(req_ref.content_type.as_deref(), Some("application/json"));
    let stored_req = futures::executor::block_on(bodies.get(&req_ref))
        .expect("get")
        .expect("request body bytes");
    assert_eq!(stored_req, req_body);

    // Тело ответа сохранено; больше порога → на диске, файл читается.
    let resp_ref = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeCompleted { body: Some(b), .. } => Some(b.clone()),
        _ => None,
    })
    .await;
    assert_eq!(resp_ref.size, 2048);
    assert_eq!(resp_ref.content_type.as_deref(), Some("text/plain"));
    let stored_resp = futures::executor::block_on(bodies.get(&resp_ref))
        .expect("get")
        .expect("response body bytes");
    assert_eq!(stored_resp.len(), 2048);

    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::Stop)
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn body_capture_over_cap_stores_truncated_prefix_and_streams_on() {
    init_tracing();
    // Tee-cap движка — 16 байт: сохраняется префикс, поток идёт целиком.
    let req_body = vec![0xA5u8; 64];
    let resp_body = vec![0x5Au8; 48];
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
        resp_body.len()
    )
    .into_bytes();
    response.extend_from_slice(&resp_body);
    let origin = spawn_canned_origin(vec![response]).await;

    let bodies = body_store();
    let cfg = EngineConfig::new("127.0.0.1:0", 16).with_body_capture(true, 16);
    let (engine_addr, events, cmd_tx) = spawn_engine_with(cfg, bodies).await;

    let resp = drive_post(&engine_addr, origin, &req_body).await;
    assert_eq!(resp.body.len(), 48, "client stream is NOT truncated");

    // Запрос: raw_size — полный поток, сохранённый BodyRef — префикс 16.
    let (req_raw, req_stored) = wait_event(&events, |e| match e {
        ProxyEvent::RequestBodyDone { body, raw_size, .. } => {
            Some((*raw_size, body.as_ref().map(|b| b.size)))
        }
        _ => None,
    })
    .await;
    assert_eq!(req_raw, 64, "raw counter sees the full stream");
    assert_eq!(req_stored, Some(16), "stored prefix is capped at 16");

    let (resp_raw, resp_stored) = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeCompleted { body, raw_size, .. } => {
            Some((*raw_size, body.as_ref().map(|b| b.size)))
        }
        _ => None,
    })
    .await;
    assert_eq!(resp_raw, 48);
    assert_eq!(resp_stored, Some(16));

    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::Stop)
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn body_capture_disabled_stores_nothing() {
    init_tracing();
    let req_body = vec![b'q'; 24];
    let resp_body = vec![b'r'; 24];
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
        resp_body.len()
    )
    .into_bytes();
    response.extend_from_slice(&resp_body);
    let origin = spawn_canned_origin(vec![response]).await;

    let bodies = body_store();
    let cfg = EngineConfig::new("127.0.0.1:0", 16).with_body_capture(false, 1024);
    let (engine_addr, events, cmd_tx) = spawn_engine_with(cfg, bodies).await;

    let resp = drive_post(&engine_addr, origin, &req_body).await;
    assert_eq!(resp.body.len(), 24);

    // События едут (счётчики живы), но BodyRef нет.
    let req_body_evt = wait_event(&events, |e| match e {
        ProxyEvent::RequestBodyDone { body, .. } => Some(body.is_none()),
        _ => None,
    })
    .await;
    assert!(req_body_evt, "no BodyRef when capture disabled");
    let done_body_none = wait_event(&events, |e| match e {
        ProxyEvent::ExchangeCompleted { body, .. } => Some(body.is_none()),
        _ => None,
    })
    .await;
    assert!(done_body_none);

    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::Stop)
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn body_capture_toggle_runtime_command_flips_capture() {
    init_tracing();
    let req_body = vec![b't'; 12];
    let resp_body = vec![b'u'; 12];
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
        resp_body.len()
    )
    .into_bytes();
    response.extend_from_slice(&resp_body);
    let origin = spawn_canned_origin(vec![response.clone(), response]).await;

    let bodies = body_store();
    let cfg = EngineConfig::new("127.0.0.1:0", 16).with_body_capture(true, 1024);
    let (engine_addr, events, cmd_tx) = spawn_engine_with(cfg, bodies).await;

    // Первый запрос — с захватом.
    let origin2 = (origin.0, origin.1.clone());
    let resp = drive_post(&engine_addr, origin2, &req_body).await;
    assert_eq!(resp.body.len(), 12);
    let first = wait_event(&events, |e| match e {
        ProxyEvent::RequestBodyDone { body, .. } => Some(body.is_some()),
        _ => None,
    })
    .await;
    assert!(first, "capture on by config");

    // Выключаем захват на лету: следующее тело не сохраняется.
    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::SetBodyCapture { enabled: false })
        .await
        .expect("send toggle");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let resp = drive_post(&engine_addr, origin, &req_body).await;
    assert_eq!(resp.body.len(), 12);
    // Ждём второй RequestBodyDone: после выключения body = None.
    let deadline = tokio::time::Instant::now() + TEST_TIMEOUT;
    let mut second_off = false;
    while tokio::time::Instant::now() < deadline {
        let seen_done = events
            .snapshot()
            .iter()
            .filter(|e| matches!(e, ProxyEvent::RequestBodyDone { .. }))
            .count();
        if seen_done >= 2 {
            let none_count = events
                .snapshot()
                .iter()
                .filter(|e| matches!(e, ProxyEvent::RequestBodyDone { body: None, .. }))
                .count();
            if none_count >= 1 {
                second_off = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(second_off, "after SetBodyCapture(false) no body is stored");

    cmd_tx
        .send(application::dto::proxy_config::EngineCommand::Stop)
        .await
        .ok();
}
