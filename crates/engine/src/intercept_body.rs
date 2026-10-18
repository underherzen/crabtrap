//! Тела и правки для пайплайна перехвата (Track S1, основа Phase 7).
//!
//! Пайплайн просит тело через `InterceptionPipeline::wants_body`; движок
//! собирает его в память с капом ([`load`]). Тело больше капа (объявленное
//! Content-Length или фактически прочитанное) идёт дальше потоком без
//! правок: прочитанный префикс проигрывается заново ([`PrefixedBody`]).
//! Отредактированное сообщение применяется к частям запроса/ответа
//! ([`apply_request`], [`apply_response`]); заменённое тело получает
//! точный Content-Length ([`finish_body`]).

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use application::services::InterceptableMessage;
use bytes::Bytes;
use domain::value_objects::http_vo::HeaderEntry;
use http::header::{CONTENT_LENGTH, HeaderMap, HeaderName, HeaderValue, TRANSFER_ENCODING};
use http_body_util::BodyExt;
use hyper::body::{Body as HttpBody, Frame, SizeHint};

/// Тело на пути через пайплайн (стёртый тип: Incoming, локальный ответ).
pub(crate) type PipeBody = http_body_util::combinators::UnsyncBoxBody<Bytes, io::Error>;

/// Стереть тип тела.
pub(crate) fn boxed<B>(body: B) -> PipeBody
where
    B: HttpBody + Send + 'static,
    B::Data: Into<Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    body.map_frame(|frame| frame.map_data(Into::into))
        .map_err(io::Error::other)
        .boxed_unsync()
}

/// Тело из буфера.
pub(crate) fn full(data: Bytes) -> PipeBody {
    http_body_util::Full::new(data)
        .map_err(|never| match never {})
        .boxed_unsync()
}

/// Буферное тело и, если были, трейлеры после него.
fn with_trailers(data: Bytes, trailers: Option<HeaderMap>) -> PipeBody {
    let Some(trailers) = trailers else {
        return full(data);
    };
    let frames: Vec<Result<Frame<Bytes>, io::Error>> =
        vec![Ok(Frame::data(data)), Ok(Frame::trailers(trailers))];
    http_body_util::StreamBody::new(futures::stream::iter(frames)).boxed_unsync()
}

/// Результат сбора тела для пайплайна.
pub(crate) enum Loaded {
    /// Тело целиком в памяти (+ трейлеры: gRPC `grpc-status` и т.п.).
    Full(Bytes, Option<HeaderMap>),
    /// Не загружено (не просили / больше капа / ошибка чтения) — поток.
    Streaming(PipeBody),
}

impl Loaded {
    /// Байты для `InterceptableMessage.body`.
    pub(crate) fn message_body(&self) -> Option<Vec<u8>> {
        match self {
            Self::Full(data, _) => Some(data.to_vec()),
            Self::Streaming(_) => None,
        }
    }
}

/// Сколько ждём следующий кадр тела при сборе: дольше — тело считается
/// потоковым (long-poll, медленная выгрузка) и идёт дальше без правок.
const LOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
/// Потолок на сбор всего тела.
const LOAD_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Собрать тело в память, если оно не больше `cap` и приходит без пауз.
/// Потоковые типы (SSE, multipart/x-mixed-replace) не собираются никогда:
/// их конец может не наступить. Прочитанный до отказа префикс
/// проигрывается заново. Трейлеры у загруженного тела теряются (тело
/// уходит буфером с Content-Length).
pub(crate) async fn load(mut body: PipeBody, headers: &HeaderMap, cap: u64) -> Loaded {
    if is_stream_type(headers) || declared_length(headers).is_some_and(|length| length > cap) {
        return Loaded::Streaming(body);
    }
    let deadline = tokio::time::Instant::now() + LOAD_TOTAL_TIMEOUT;
    let mut chunks = VecDeque::new();
    let mut total = 0_u64;
    let mut trailers: Option<HeaderMap> = None;
    loop {
        let idle = tokio::time::Instant::now() + LOAD_IDLE_TIMEOUT;
        let next = tokio::time::timeout_at(idle.min(deadline), body.frame()).await;
        match next {
            Err(_elapsed) => {
                tracing::debug!(total, "body is streaming, pipeline gets no body");
                return Loaded::Streaming(PrefixedBody::new(chunks, None, body).boxed_unsync());
            }
            Ok(None) => return Loaded::Full(concat(chunks, total), trailers),
            Ok(Some(Ok(frame))) => match frame.into_data() {
                Ok(data) => {
                    total += data.len() as u64;
                    chunks.push_back(data);
                    if total > cap {
                        return Loaded::Streaming(
                            PrefixedBody::new(chunks, None, body).boxed_unsync(),
                        );
                    }
                }
                Err(frame) => {
                    if let Ok(map) = frame.into_trailers() {
                        trailers.get_or_insert_with(HeaderMap::new).extend(map);
                    }
                }
            },
            Ok(Some(Err(error))) => {
                let replay = PrefixedBody::new(chunks, Some(error), body);
                return Loaded::Streaming(replay.boxed_unsync());
            }
        }
    }
}

/// Тип тела, у которого может не быть конца.
fn is_stream_type(headers: &HeaderMap) -> bool {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ct| {
            ct.starts_with("text/event-stream") || ct.starts_with("multipart/x-mixed-replace")
        })
}

fn declared_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn concat(chunks: VecDeque<Bytes>, total: u64) -> Bytes {
    if chunks.len() == 1 {
        return chunks.into_iter().next().unwrap_or_default();
    }
    let mut out = Vec::with_capacity(usize::try_from(total).unwrap_or(0));
    for chunk in chunks {
        out.extend_from_slice(&chunk);
    }
    Bytes::from(out)
}

/// Тело после пайплайна. `replaced` — пайплайн вернул другое тело (нужен
/// новый Content-Length); `discarded` — незагруженный оригинал, который
/// заменили: его надо дочитать, а не бросить (иначе клиент, ещё
/// отправляющий тело, получит сброс соединения).
pub(crate) struct FinishedBody {
    pub body: PipeBody,
    pub replaced: Option<u64>,
    pub discarded: Option<PipeBody>,
}

/// Итоговое тело: замена из пайплайна, загруженный оригинал или поток.
pub(crate) fn finish_body(loaded: Loaded, edited: Option<Vec<u8>>) -> FinishedBody {
    let keep = |body| FinishedBody {
        body,
        replaced: None,
        discarded: None,
    };
    match (loaded, edited) {
        (Loaded::Full(original, trailers), Some(new)) if original.as_ref() == new.as_slice() => {
            keep(with_trailers(original, trailers))
        }
        (Loaded::Full(_, trailers), Some(new)) => FinishedBody {
            replaced: Some(new.len() as u64),
            body: with_trailers(Bytes::from(new), trailers),
            discarded: None,
        },
        (Loaded::Streaming(stream), Some(new)) => FinishedBody {
            replaced: Some(new.len() as u64),
            body: full(Bytes::from(new)),
            discarded: Some(stream),
        },
        (Loaded::Full(original, trailers), None) => keep(with_trailers(original, trailers)),
        (Loaded::Streaming(stream), None) => keep(stream),
    }
}

/// Можно ли ответу с этим статусом нести тело/Content-Length (RFC 9110:
/// не у 1xx, 204 и 304).
pub(crate) fn status_has_body(status: http::StatusCode) -> bool {
    !(status.is_informational()
        || status == http::StatusCode::NO_CONTENT
        || status == http::StatusCode::NOT_MODIFIED)
}

/// Длина заменённого тела: точный Content-Length, без chunked.
pub(crate) fn set_length(headers: &mut HeaderMap, length: u64) {
    headers.remove(TRANSFER_ENCODING);
    headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
}

/// Заголовки из сообщения пайплайна. Значения, совпавшие с исходными
/// (в их lossy-UTF-8 виде), берутся из оригинала байт-в-байт — правка
/// одного заголовка не портит остальные (не-ASCII, не-UTF-8). Невалидные
/// имена/значения пропускаются с предупреждением.
pub(crate) fn merge_headers(original: &HeaderMap, entries: &[HeaderEntry]) -> HeaderMap {
    let mut unused: Vec<(&HeaderName, &HeaderValue)> = original.iter().collect();
    let mut map = HeaderMap::with_capacity(entries.len());
    for entry in entries {
        let reused = unused.iter().position(|(name, value)| {
            name.as_str().eq_ignore_ascii_case(entry.name.trim())
                && String::from_utf8_lossy(value.as_bytes()) == entry.value
        });
        if let Some(ix) = reused {
            let (name, value) = unused.swap_remove(ix);
            map.append(name.clone(), value.clone());
            continue;
        }
        match (
            HeaderName::from_bytes(entry.name.trim().as_bytes()),
            HeaderValue::from_bytes(entry.value.as_bytes()),
        ) {
            (Ok(name), Ok(value)) => {
                map.append(name, value);
            }
            _ => tracing::warn!(name = %entry.name, "pipeline header is not valid HTTP, skipped"),
        }
    }
    map
}

/// Что поменялось в запросе (для Host и тегов).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RequestEdits {
    /// Пайплайн сам выставил заголовок Host — движок его не перетирает.
    pub host_header: bool,
}

/// Применить отличающиеся от `seen` method/uri/headers запроса.
/// Невалидный метод или не-absolute URI игнорируются (остаётся исходное).
pub(crate) fn apply_request(
    parts: &mut http::request::Parts,
    seen: &InterceptableMessage,
    edited: &InterceptableMessage,
) -> RequestEdits {
    if edited.method != seen.method
        && let Some(method) = edited.method.as_deref()
    {
        match http::Method::from_bytes(method.trim().as_bytes()) {
            Ok(method) => parts.method = method,
            Err(_) => tracing::warn!(method, "pipeline method is invalid, kept original"),
        }
    }
    if edited.uri != seen.uri
        && let Some(uri) = edited.uri.as_deref()
    {
        match uri.parse::<http::Uri>() {
            Ok(uri) if uri.authority().is_some() && uri.scheme().is_some() => parts.uri = uri,
            _ => tracing::warn!(uri, "pipeline URI is not absolute, kept original"),
        }
    }
    if edited.headers == seen.headers {
        return RequestEdits::default();
    }
    parts.headers = merge_headers(&parts.headers, &edited.headers);
    RequestEdits {
        host_header: header_value(&edited.headers, "host") != header_value(&seen.headers, "host"),
    }
}

fn header_value<'a>(headers: &'a [HeaderEntry], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

/// Применить отличающиеся от `seen` status/headers ответа. Статус
/// принимается только финальный (200–599): 1xx как итоговый ответ ломает
/// HTTP/1.
pub(crate) fn apply_response(
    parts: &mut http::response::Parts,
    seen: &InterceptableMessage,
    edited: &InterceptableMessage,
) {
    if edited.status != seen.status
        && let Some(status) = edited.status
    {
        match http::StatusCode::from_u16(status) {
            Ok(code) if (200..=599).contains(&status) => parts.status = code,
            _ => tracing::warn!(
                status,
                "pipeline status is not a final status, kept original"
            ),
        }
    }
    if edited.headers != seen.headers {
        parts.headers = merge_headers(&parts.headers, &edited.headers);
    }
}

/// Префикс уже прочитанных кадров (и, возможно, ошибка чтения), затем
/// остаток исходного тела.
pub(crate) struct PrefixedBody {
    prefix: VecDeque<Bytes>,
    error: Option<io::Error>,
    rest: PipeBody,
}

impl PrefixedBody {
    fn new(prefix: VecDeque<Bytes>, error: Option<io::Error>, rest: PipeBody) -> Self {
        Self {
            prefix,
            error,
            rest,
        }
    }
}

impl HttpBody for PrefixedBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        if let Some(data) = this.prefix.pop_front() {
            return Poll::Ready(Some(Ok(Frame::data(data))));
        }
        if let Some(error) = this.error.take() {
            return Poll::Ready(Some(Err(error)));
        }
        Pin::new(&mut this.rest).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.prefix.is_empty() && self.error.is_none() && self.rest.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        let buffered: u64 = self.prefix.iter().map(|b| b.len() as u64).sum();
        let rest = self.rest.size_hint();
        let mut hint = SizeHint::new();
        hint.set_lower(rest.lower() + buffered);
        if let Some(upper) = rest.upper() {
            hint.set_upper(upper + buffered);
        }
        hint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(chunks: &[&'static [u8]]) -> PipeBody {
        let frames: Vec<Result<Frame<Bytes>, io::Error>> = chunks
            .iter()
            .map(|c| Ok(Frame::data(Bytes::from_static(c))))
            .collect();
        http_body_util::StreamBody::new(futures::stream::iter(frames)).boxed_unsync()
    }

    async fn drain_all(body: PipeBody) -> Vec<u8> {
        body.collect().await.expect("collect").to_bytes().to_vec()
    }

    #[tokio::test]
    async fn loads_body_within_cap() {
        let loaded = load(stream(&[b"ab", b"cd"]), &HeaderMap::new(), 4).await;
        assert_eq!(loaded.message_body().as_deref(), Some(&b"abcd"[..]));
    }

    #[tokio::test]
    async fn trailers_survive_loading_and_replacement() {
        let mut grpc = HeaderMap::new();
        grpc.insert("grpc-status", HeaderValue::from_static("0"));
        let frames: Vec<Result<Frame<Bytes>, io::Error>> = vec![
            Ok(Frame::data(Bytes::from_static(b"msg"))),
            Ok(Frame::trailers(grpc.clone())),
        ];
        let body = http_body_util::StreamBody::new(futures::stream::iter(frames)).boxed_unsync();
        let loaded = load(body, &HeaderMap::new(), 1024).await;
        let finished = finish_body(loaded, Some(b"new".to_vec()));
        let collected = finished.body.collect().await.expect("collect");
        assert_eq!(collected.trailers(), Some(&grpc));
        assert_eq!(collected.to_bytes().as_ref(), b"new");
    }

    #[tokio::test]
    async fn over_cap_replays_prefix_and_rest() {
        let loaded = load(stream(&[b"abc", b"def", b"g"]), &HeaderMap::new(), 4).await;
        let Loaded::Streaming(body) = loaded else {
            panic!("must stream")
        };
        assert_eq!(drain_all(body).await, b"abcdefg");
    }

    #[tokio::test]
    async fn declared_length_over_cap_is_not_read() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_LENGTH, HeaderValue::from(100_u64));
        let loaded = load(stream(&[b"x"]), &headers, 4).await;
        assert!(loaded.message_body().is_none());
    }

    #[test]
    fn finish_body_marks_only_real_replacement() {
        let same = finish_body(
            Loaded::Full(Bytes::from_static(b"a"), None),
            Some(b"a".to_vec()),
        );
        assert_eq!(same.replaced, None);
        let new = finish_body(
            Loaded::Full(Bytes::from_static(b"a"), None),
            Some(b"bcd".to_vec()),
        );
        assert_eq!(new.replaced, Some(3));
        let streamed = finish_body(Loaded::Streaming(stream(&[b"z"])), None);
        assert_eq!(streamed.replaced, None);
        let swapped = finish_body(Loaded::Streaming(stream(&[b"z"])), Some(b"q".to_vec()));
        assert_eq!(swapped.replaced, Some(1));
        assert!(
            swapped.discarded.is_some(),
            "unread original must be drained"
        );
    }

    #[test]
    fn untouched_headers_keep_original_bytes() {
        let mut original = HeaderMap::new();
        let raw = HeaderValue::from_bytes(b"attachment; filename=\"r\xc3\xa9sum\xc3\xa9.pdf\"")
            .expect("value");
        original.insert("content-disposition", raw.clone());
        original.insert("x-old", HeaderValue::from_static("1"));
        let entries = vec![
            HeaderEntry {
                name: "content-disposition".into(),
                value: String::from_utf8_lossy(raw.as_bytes()).into_owned(),
            },
            HeaderEntry {
                name: "x-new".into(),
                value: "ü".into(),
            },
        ];
        let merged = merge_headers(&original, &entries);
        assert_eq!(merged["content-disposition"], raw);
        assert_eq!(merged["x-new"].as_bytes(), "ü".as_bytes());
        assert!(merged.get("x-old").is_none(), "removed by pipeline");
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_body_streams_after_idle_timeout() {
        let (mut tx, rx) = futures::channel::mpsc::channel::<Result<Frame<Bytes>, io::Error>>(4);
        tx.try_send(Ok(Frame::data(Bytes::from_static(b"head"))))
            .expect("send");
        let body = http_body_util::StreamBody::new(rx).boxed_unsync();
        let loaded = load(body, &HeaderMap::new(), 1024).await;
        let Loaded::Streaming(body) = loaded else {
            panic!("stalled body must stream")
        };
        drop(tx);
        assert_eq!(drain_all(body).await, b"head");
    }

    #[tokio::test]
    async fn event_stream_is_never_loaded() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        let loaded = load(stream(&[b"data: 1"]), &headers, 1024).await;
        assert!(loaded.message_body().is_none());
    }

    #[test]
    fn invalid_pipeline_values_keep_originals() {
        let (mut parts, ()) = http::Request::builder()
            .method("GET")
            .uri("http://a.test/x")
            .body(())
            .expect("request")
            .into_parts();
        let seen = InterceptableMessage {
            method: Some("GET".into()),
            uri: Some("http://a.test/x".into()),
            version: None,
            status: None,
            reason: None,
            headers: Vec::new(),
            body: None,
        };
        let edited = InterceptableMessage {
            method: Some("BAD METHOD".into()),
            uri: Some("/relative".into()),
            version: None,
            status: None,
            reason: None,
            headers: vec![
                HeaderEntry {
                    name: "x-ok".into(),
                    value: "1".into(),
                },
                HeaderEntry {
                    name: "bad name".into(),
                    value: "2".into(),
                },
            ],
            body: None,
        };
        apply_request(&mut parts, &seen, &edited);
        assert_eq!(parts.method, http::Method::GET);
        assert_eq!(parts.uri, "http://a.test/x");
        assert_eq!(parts.headers.len(), 1);
        assert_eq!(parts.headers["x-ok"], "1");
    }
}
