//! Поиск по захваченному трафику (заголовки и тела).
//!
//! Обмены читаются страницами через `ExchangeRepository::list` (свежие
//! сначала — порядок хитов совпадает с таблицей); тела — лениво через
//! [`BodyRepository::get`] по `BodyRef`, только пока тело не превышает
//! [`BODY_SEARCH_CAP_BYTES`] (больше — молча пропускаем: поиск не должен
//! тащить гигабайтные загрузки в память).
//!
//! Матчер (подстрока или regex) компилируется один раз на `execute`
//! (regex-режим с битым паттерном → `DomainError::ValidationError`).
//! Подстрока реализована как escaped-regex с флагом case-insensitivity —
//! один код-путь и полнобуквенный (Unicode) регистронезависимый поиск.
//!
//! Тела ищутся так же, как их видит инспектор: сначала снимается
//! Content-Encoding (gzip/br/deflate — через [`BodyDecoderService`], если он
//! подключён), затем текст декодируется по charset из Content-Type/BOM.

use std::sync::Arc;

use domain::entities::body::BodyRef;
use domain::entities::exchange::HttpExchange;
use domain::repositories::{BodyRepository, ExchangeRepository};
use domain::value_objects::filter_query::FilterQuery;
use domain::{DomainError, ExchangeId};

use super::charset::{decode_charset, split_content_type};
use crate::services::BodyDecoderService;

/// Где искать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    Headers,
    Body,
    All,
}

/// Тела больше этого размера поиск не читает (skip, не ошибка).
pub const BODY_SEARCH_CAP_BYTES: u64 = 10 * 1024 * 1024;

/// Размер страницы выборки обменов (обход репозитория без count()).
const LIST_PAGE: usize = 256;

/// Полуширина окна сниппета вокруг совпадения (в символах).
const SNIPPET_RADIUS: usize = 48;

/// Вход поиска.
#[derive(Debug, Clone)]
pub struct SearchQueryDto {
    /// Паттерн: подстрока или regex (когда `regex = true`).
    pub text: String,
    pub case_sensitive: bool,
    pub regex: bool,
    pub scope: SearchScope,
    /// Максимум результатов (защита от огромных выдач).
    pub limit: usize,
}

/// Найденное совпадение.
#[derive(Debug, Clone)]
pub struct SearchHitDto {
    pub exchange_id: ExchangeId,
    /// Где нашли: "header: <name> (request|response)",
    /// "body: request" | "body: response".
    pub field: String,
    /// Короткий фрагмент вокруг совпадения.
    pub snippet: String,
}

/// Скомпилированный паттерн (подстрока → escaped regex).
struct Matcher(regex::Regex);

impl Matcher {
    fn build(query: &SearchQueryDto) -> Result<Self, DomainError> {
        if query.text.is_empty() {
            return Err(DomainError::ValidationError(
                "search text must not be empty".into(),
            ));
        }
        let pattern = if query.regex {
            query.text.as_str()
        } else {
            &regex::escape(&query.text)
        };
        regex::RegexBuilder::new(pattern)
            .case_insensitive(!query.case_sensitive)
            .build()
            .map(Matcher)
            .map_err(|e| DomainError::ValidationError(format!("invalid search regex: {e}")))
    }

    /// Первое совпадение в haystack.
    fn find<'h>(&self, haystack: &'h str) -> Option<regex::Match<'h>> {
        self.0.find(haystack)
    }
}

pub struct SearchExchangesUseCase {
    exchange_repository: Arc<dyn ExchangeRepository>,
    body_repository: Arc<dyn BodyRepository>,
    /// Снятие Content-Encoding; None — тела ищутся как сохранены.
    body_decoder: Option<Arc<dyn BodyDecoderService>>,
}

impl SearchExchangesUseCase {
    pub fn new(
        exchange_repository: Arc<dyn ExchangeRepository>,
        body_repository: Arc<dyn BodyRepository>,
    ) -> Self {
        Self {
            exchange_repository,
            body_repository,
            body_decoder: None,
        }
    }

    /// Подключить декодер Content-Encoding (иначе gzip/br-тела не находятся).
    pub fn with_decoder(self, body_decoder: Arc<dyn BodyDecoderService>) -> Self {
        Self {
            body_decoder: Some(body_decoder),
            ..self
        }
    }

    /// Пройти все обмены (свежие сначала) и собрать до `limit` хитов.
    /// Останавливается досрочно, как только лимит достигнут.
    pub async fn execute(&self, query: SearchQueryDto) -> Result<Vec<SearchHitDto>, DomainError> {
        let matcher = Matcher::build(&query)?;
        let mut hits: Vec<SearchHitDto> = Vec::new();
        let mut offset = 0_usize;
        loop {
            let page = self
                .exchange_repository
                .list(FilterQuery::default(), offset, LIST_PAGE)
                .await?;
            if page.is_empty() {
                break;
            }
            let fetched = page.len();
            for exchange in &page {
                self.search_exchange(exchange, &matcher, query.scope, &mut hits, query.limit)
                    .await?;
                if hits.len() >= query.limit {
                    break;
                }
            }
            if hits.len() >= query.limit || fetched < LIST_PAGE {
                break;
            }
            offset += fetched;
        }
        Ok(hits)
    }

    /// Все хиты одного обмена: заголовки (request, затем response),
    /// затем тела (request, затем response) — порядок вкладок инспектора.
    async fn search_exchange(
        &self,
        exchange: &HttpExchange,
        matcher: &Matcher,
        scope: SearchScope,
        hits: &mut Vec<SearchHitDto>,
        limit: usize,
    ) -> Result<(), DomainError> {
        if scope != SearchScope::Body {
            let no_headers: Vec<domain::value_objects::http_vo::HeaderEntry> = Vec::new();
            let (req_headers, resp_headers) = match &exchange.response {
                Some(resp) => (&exchange.request.headers, &resp.headers),
                None => (&exchange.request.headers, &no_headers),
            };
            for (direction, headers) in [("request", req_headers), ("response", resp_headers)] {
                for header in headers {
                    if hits.len() >= limit {
                        return Ok(());
                    }
                    // Матчимся по имени ИЛИ значению (не через склейку —
                    // иначе «o: v» ловил бы стык), сниппет — из "name: value".
                    let combined = format!("{}: {}", header.name, header.value);
                    let start = matcher.find(&header.name).map(|m| m.start()).or_else(|| {
                        matcher
                            .find(&header.value)
                            .map(|m| header.name.len() + 2 + m.start())
                    });
                    if let Some(start) = start {
                        hits.push(SearchHitDto {
                            exchange_id: exchange.id,
                            field: format!("header: {} ({direction})", header.name),
                            snippet: snippet(&combined, start),
                        });
                    }
                }
            }
        }

        if scope != SearchScope::Headers {
            if let Some(hit) = match &exchange.request.body {
                Some(body_ref) => {
                    self.search_body(exchange.id, body_ref, "request", matcher)
                        .await?
                }
                None => None,
            } {
                hits.push(hit);
                if hits.len() >= limit {
                    return Ok(());
                }
            }
            if let Some(hit) = match exchange.response.as_ref().and_then(|r| r.body.as_ref()) {
                Some(body_ref) => {
                    self.search_body(exchange.id, body_ref, "response", matcher)
                        .await?
                }
                None => None,
            } {
                hits.push(hit);
            }
        }
        Ok(())
    }

    /// Тело: не сохранено (`size == 0`) или больше капа → skip без ошибки.
    async fn search_body(
        &self,
        exchange_id: ExchangeId,
        body_ref: &BodyRef,
        direction: &str,
        matcher: &Matcher,
    ) -> Result<Option<SearchHitDto>, DomainError> {
        if body_ref.size == 0 || body_ref.size > BODY_SEARCH_CAP_BYTES {
            return Ok(None);
        }
        let Some(bytes) = self.body_repository.get(body_ref).await? else {
            return Ok(None);
        };
        let bytes = self.decode_content_encoding(bytes, body_ref).await;
        let charset = body_ref
            .content_type
            .as_deref()
            .and_then(|ct| split_content_type(ct).1);
        let (text, _, _) = decode_charset(&bytes, charset.as_deref());
        Ok(matcher.find(&text).map(|m| SearchHitDto {
            exchange_id,
            field: format!("body: {direction}"),
            snippet: snippet(&text, m.start()),
        }))
    }
}

impl SearchExchangesUseCase {
    /// Снять Content-Encoding; сбой декодера — ищем по сохранённым байтам
    /// (битое сжатие не должно прятать тело из поиска целиком).
    async fn decode_content_encoding(&self, bytes: Vec<u8>, body_ref: &BodyRef) -> Vec<u8> {
        let (Some(decoder), Some(encoding)) = (&self.body_decoder, body_ref.encoding.as_deref())
        else {
            return bytes;
        };
        match decoder.decode(&bytes, encoding).await {
            Ok(decoded) => decoded,
            Err(e) => {
                tracing::debug!(error = %e, encoding, "search: body decode failed, using raw bytes");
                bytes
            }
        }
    }
}

/// Сниппет вокруг совпадения: окно ±[`SNIPPET_RADIUS`] от начала матча,
/// «…» по краям при обрезке, переводы строк (CRLF/CR/LF) → один «⏎».
fn snippet(haystack: &str, match_start: usize) -> String {
    let len = haystack.len();
    let lo = floor_boundary(haystack, match_start.saturating_sub(SNIPPET_RADIUS));
    let hi = ceil_boundary(haystack, (match_start + SNIPPET_RADIUS).min(len));
    let mut out = String::new();
    if lo > 0 {
        out.push('…');
    }
    let mut chars = haystack[lo..hi].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                out.push('⏎');
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            }
            '\n' => out.push('⏎'),
            _ => out.push(c),
        }
    }
    if hi < len {
        out.push('…');
    }
    out
}

/// Начало символа UTF-8, не правее `ix` (у `str` нет стабильного
/// floor_char_boundary на всех тулчейнах проекта — делаем руками).
fn floor_boundary(haystack: &str, ix: usize) -> usize {
    let mut ix = ix.min(haystack.len());
    while ix > 0 && !haystack.is_char_boundary(ix) {
        ix -= 1;
    }
    ix
}

/// Начало символа UTF-8, не левее `ix`.
fn ceil_boundary(haystack: &str, ix: usize) -> usize {
    let mut ix = ix.min(haystack.len());
    while ix < haystack.len() && !haystack.is_char_boundary(ix) {
        ix += 1;
    }
    ix
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use domain::entities::exchange::{HttpRequest, HttpResponse};
    use domain::value_objects::http_vo::{HeaderEntry, HttpVersionVo, Method, Protocol};
    use domain::value_objects::{BodyId, ConnectionId, Host};
    use std::collections::HashMap;
    use std::sync::Mutex;

    // --- In-memory репозитории (паттерн get_exchange_detail) ------------

    struct MemExchanges(Mutex<Vec<HttpExchange>>);

    #[async_trait]
    impl ExchangeRepository for MemExchanges {
        async fn append(&self, exchange: HttpExchange) -> Result<(), DomainError> {
            self.0.lock().unwrap().push(exchange);
            Ok(())
        }
        async fn update_response(
            &self,
            _id: &ExchangeId,
            _response: HttpResponse,
            _timing: domain::value_objects::timing::TimingPhases,
        ) -> Result<(), DomainError> {
            Ok(())
        }
        async fn append_ws_message(
            &self,
            _id: &ExchangeId,
            _message: domain::entities::websocket::WebSocketMessage,
        ) -> Result<(), DomainError> {
            Ok(())
        }

        async fn update_request_head(
            &self,
            _id: &ExchangeId,
            _head: domain::repositories::RequestHead,
            _client_body: domain::entities::ClientBody,
        ) -> Result<(), DomainError> {
            Ok(())
        }

        async fn update_request_body(
            &self,
            _id: &ExchangeId,
            _body: Option<BodyRef>,
            _raw_size: u64,
        ) -> Result<(), DomainError> {
            Ok(())
        }
        async fn update_status(
            &self,
            _id: &ExchangeId,
            _status: domain::entities::ExchangeStatus,
        ) -> Result<(), DomainError> {
            Ok(())
        }
        async fn get(&self, id: &ExchangeId) -> Result<Option<HttpExchange>, DomainError> {
            Ok(self.0.lock().unwrap().iter().find(|e| e.id == *id).cloned())
        }
        async fn list(
            &self,
            _filter: FilterQuery,
            offset: usize,
            limit: usize,
        ) -> Result<Vec<HttpExchange>, DomainError> {
            // Свежие сначала — как InMemoryExchangeRepository.
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .rev()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }
        async fn tag(&self, _id: &ExchangeId, _tag: String) -> Result<(), DomainError> {
            Ok(())
        }
        async fn set_note(&self, _id: &ExchangeId, _note: String) -> Result<(), DomainError> {
            Ok(())
        }
        async fn clear(&self) -> Result<(), DomainError> {
            self.0.lock().unwrap().clear();
            Ok(())
        }
        async fn stats(&self) -> Result<domain::repositories::TrafficStats, DomainError> {
            Ok(Default::default())
        }
        async fn count(&self) -> Result<u64, DomainError> {
            Ok(self.0.lock().unwrap().len() as u64)
        }
    }

    struct MemBodies(Mutex<HashMap<BodyId, Vec<u8>>>);

    #[async_trait]
    impl BodyRepository for MemBodies {
        async fn put(
            &self,
            id: BodyId,
            data: Vec<u8>,
            _descriptor: domain::entities::body::BodyDescriptor,
        ) -> Result<BodyRef, DomainError> {
            self.0.lock().unwrap().insert(id, data.clone());
            Ok(BodyRef {
                id,
                size: data.len() as u64,
                content_type: Some("text/plain".into()),
                encoding: None,
                storage: Default::default(),
            })
        }
        async fn get(&self, body_ref: &BodyRef) -> Result<Option<Vec<u8>>, DomainError> {
            Ok(self.0.lock().unwrap().get(&body_ref.id).cloned())
        }
        async fn get_range(
            &self,
            _body_ref: &BodyRef,
            _offset: u64,
            _len: u64,
        ) -> Result<Option<Vec<u8>>, DomainError> {
            Ok(None)
        }
        async fn delete(&self, id: BodyId) -> Result<(), DomainError> {
            self.0.lock().unwrap().remove(&id);
            Ok(())
        }
        async fn total_size(&self) -> Result<u64, DomainError> {
            Ok(0)
        }
        async fn clear(&self) -> Result<(), DomainError> {
            self.0.lock().unwrap().clear();
            Ok(())
        }
    }

    // --- Фикстуры -------------------------------------------------------

    type Fixture = (Arc<MemExchanges>, Arc<MemBodies>);

    fn fixture() -> Fixture {
        (
            Arc::new(MemExchanges(Mutex::new(Vec::new()))),
            Arc::new(MemBodies(Mutex::new(HashMap::new()))),
        )
    }

    fn use_case((exchanges, bodies): &Fixture) -> SearchExchangesUseCase {
        SearchExchangesUseCase::new(exchanges.clone() as _, bodies.clone() as _)
    }

    fn query(text: &str) -> SearchQueryDto {
        SearchQueryDto {
            text: text.into(),
            case_sensitive: false,
            regex: false,
            scope: SearchScope::All,
            limit: 100,
        }
    }

    fn exchange(host: &str) -> HttpExchange {
        HttpExchange::new_pending(
            ConnectionId::new(),
            Host::new(host).expect("test host"),
            HttpRequest {
                method: Method::new("GET"),
                uri: format!("https://{host}/x"),
                version: HttpVersionVo::Http11,
                headers: Vec::new(),
                body: None,
                raw_size: 10,
                origin: Default::default(),
            },
            Protocol::Http1,
            Utc::now(),
        )
    }

    fn with_headers(
        mut e: HttpExchange,
        req: &[(&str, &str)],
        resp: &[(&str, &str)],
    ) -> HttpExchange {
        e.request.headers = req.iter().map(|(n, v)| HeaderEntry::new(*n, *v)).collect();
        e.response = Some(HttpResponse {
            status: 200,
            reason: None,
            headers: resp.iter().map(|(n, v)| HeaderEntry::new(*n, *v)).collect(),
            body: None,
            raw_size: 0,
            version: HttpVersionVo::Http11,
        });
        e
    }

    /// Прикрепить тела: (bytes, BodyRef.size) — size отдельной, чтобы
    /// имитировать «не сохранено» (0) и «больше капа» (CAP+1).
    fn with_bodies(
        f: &Fixture,
        mut e: HttpExchange,
        req: Option<(&[u8], u64)>,
        resp: Option<(&[u8], u64)>,
    ) -> HttpExchange {
        let attach = |f: &Fixture, data: Option<(&[u8], u64)>| {
            data.map(|(bytes, size)| {
                let id = BodyId::new();
                f.1.0.lock().unwrap().insert(id, bytes.to_vec());
                BodyRef {
                    id,
                    size,
                    content_type: Some("text/plain".into()),
                    encoding: None,
                    storage: Default::default(),
                }
            })
        };
        e.request.body = attach(f, req);
        if let Some(response) = &mut e.response {
            response.body = attach(f, resp);
        } else {
            let mut response = HttpResponse::empty();
            response.body = attach(f, resp);
            e.response = Some(response);
        }
        e
    }

    fn push(f: &Fixture, e: HttpExchange) -> ExchangeId {
        let id = e.id;
        f.0.0.lock().unwrap().push(e);
        id
    }

    fn run(f: &Fixture, q: SearchQueryDto) -> Result<Vec<SearchHitDto>, DomainError> {
        futures::executor::block_on(use_case(f).execute(q))
    }

    // --- Тесты ----------------------------------------------------------

    #[test]
    fn substring_matches_header_case_insensitive_and_sensitive() {
        let f = fixture();
        let id = push(
            &f,
            with_headers(exchange("a.com"), &[("X-Trace", "CrabTrap-Alpha")], &[]),
        );

        let hits = run(&f, query("crabtrap")).expect("hits");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].exchange_id, id);
        assert_eq!(hits[0].field, "header: X-Trace (request)");
        assert!(hits[0].snippet.contains("X-Trace: CrabTrap-Alpha"));

        // Регистрозависимо — «crabtrap» не совпадает с «CrabTrap».
        let strict = run(
            &f,
            SearchQueryDto {
                case_sensitive: true,
                ..query("crabtrap")
            },
        )
        .expect("hits");
        assert!(strict.is_empty());
    }

    #[test]
    fn regex_mode_matches_and_invalid_regex_is_validation_error() {
        let f = fixture();
        push(
            &f,
            with_headers(exchange("a.com"), &[("X-Num", "crab-42")], &[]),
        );

        let hits = run(
            &f,
            SearchQueryDto {
                regex: true,
                case_sensitive: true,
                ..query(r"crab-\d+")
            },
        )
        .expect("hits");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].field, "header: X-Num (request)");
        assert!(hits[0].snippet.contains("crab-42"));

        let err = run(
            &f,
            SearchQueryDto {
                regex: true,
                ..query("(unclosed")
            },
        )
        .unwrap_err();
        assert!(matches!(err, DomainError::ValidationError(_)), "{err:?}");
    }

    #[test]
    fn empty_text_is_validation_error() {
        let f = fixture();
        let err = run(&f, query("")).unwrap_err();
        assert!(matches!(err, DomainError::ValidationError(_)));
    }

    #[test]
    fn scopes_split_headers_and_bodies() {
        let f = fixture();
        push(
            &f,
            with_bodies(
                &f,
                with_headers(exchange("a.com"), &[("X-Trace", "needle-header")], &[]),
                Some((b"needle-body-req", 15)),
                Some((b"plain", 5)),
            ),
        );

        let headers_only = run(
            &f,
            SearchQueryDto {
                scope: SearchScope::Headers,
                ..query("needle")
            },
        )
        .expect("hits");
        assert_eq!(headers_only.len(), 1);
        assert!(headers_only[0].field.starts_with("header: "));

        let bodies_only = run(
            &f,
            SearchQueryDto {
                scope: SearchScope::Body,
                ..query("needle")
            },
        )
        .expect("hits");
        assert_eq!(bodies_only.len(), 1);
        assert_eq!(bodies_only[0].field, "body: request");

        let all = run(&f, query("needle")).expect("hits");
        assert_eq!(all.len(), 2, "All = headers + bodies");
    }

    /// Тестовый декодер: `x-reverse` — байты задом наперёд.
    struct ReverseDecoder;

    #[async_trait]
    impl crate::services::BodyDecoderService for ReverseDecoder {
        async fn decode(&self, data: &[u8], encoding: &str) -> Result<Vec<u8>, DomainError> {
            if encoding == "x-reverse" {
                Ok(data.iter().rev().copied().collect())
            } else {
                Ok(data.to_vec())
            }
        }
        async fn supported(&self, encoding: &str) -> bool {
            encoding == "x-reverse"
        }
    }

    /// Обмен с одним телом ответа заданных content-type/encoding.
    fn with_response_body(
        f: &Fixture,
        bytes: &[u8],
        content_type: &str,
        encoding: Option<&str>,
    ) -> HttpExchange {
        let id = BodyId::new();
        f.1.0.lock().unwrap().insert(id, bytes.to_vec());
        let mut response = HttpResponse::empty();
        response.body = Some(BodyRef {
            id,
            size: bytes.len() as u64,
            content_type: Some(content_type.into()),
            encoding: encoding.map(str::to_string),
            storage: Default::default(),
        });
        let mut e = exchange("enc.com");
        e.response = Some(response);
        e
    }

    #[test]
    fn content_encoded_body_is_decoded_before_matching() {
        let f = fixture();
        let encoded: Vec<u8> = b"hello world".iter().rev().copied().collect();
        push(
            &f,
            with_response_body(&f, &encoded, "text/plain", Some("x-reverse")),
        );

        let uc = use_case(&f).with_decoder(Arc::new(ReverseDecoder));
        let hits = futures::executor::block_on(uc.execute(query("hello world"))).expect("hits");
        assert_eq!(hits.len(), 1, "decoded body must match");
        assert_eq!(hits[0].field, "body: response");
    }

    #[test]
    fn body_charset_from_content_type_is_honored() {
        let f = fixture();
        let (cp1251, _, _) = encoding_rs::WINDOWS_1251.encode("привет мир");
        push(
            &f,
            with_response_body(&f, &cp1251, "text/html; charset=windows-1251", None),
        );
        let hits = run(&f, query("ПРИВЕТ")).expect("hits");
        assert_eq!(hits.len(), 1, "cp1251 body found by unicode query");
        assert!(hits[0].snippet.contains("привет мир"));
    }

    #[test]
    fn body_hits_label_request_and_response() {
        let f = fixture();
        push(
            &f,
            with_bodies(
                &f,
                with_headers(exchange("a.com"), &[], &[]),
                Some((b"payload hello", 13)),
                Some((b"payload world", 13)),
            ),
        );

        let hits = run(&f, query("payload")).expect("hits");
        let fields: Vec<_> = hits.iter().map(|h| h.field.as_str()).collect();
        assert_eq!(fields, vec!["body: request", "body: response"]);
        assert!(hits[0].snippet.contains("payload hello"));
    }

    #[test]
    fn snippet_window_is_ellipsized_and_newlines_collapsed() {
        let f = fixture();
        let body = format!("{}\r\nneedle\r{}", "A".repeat(200), "B".repeat(200));
        push(
            &f,
            with_bodies(
                &f,
                with_headers(exchange("a.com"), &[], &[]),
                Some((body.as_bytes(), body.len() as u64)),
                None,
            ),
        );

        let hits = run(&f, query("needle")).expect("hits");
        assert_eq!(hits.len(), 1);
        let snippet = &hits[0].snippet;
        assert!(snippet.starts_with('…'), "обрезан слева: {snippet}");
        assert!(snippet.ends_with('…'), "обрезан справа: {snippet}");
        assert!(snippet.contains("needle"));
        // CRLF до матча и одиночный CR после — каждый свёрнут в один «⏎»,
        // сырых \r/\n в сниппете нет.
        assert_eq!(snippet.matches('⏎').count(), 2);
        assert!(!snippet.contains('\r') && !snippet.contains('\n'));
        // Окно ±48 + матч + «…»: 300-символьное тело не влезает целиком.
        assert!(snippet.chars().count() < 130, "{snippet}");
        // 48 «A» до матча — не больше радиуса.
        let a_count = snippet.matches('A').count();
        assert!(a_count <= 48, "{a_count} A's in snippet");
    }

    #[test]
    fn oversized_and_missing_bodies_are_skipped_silently() {
        let f = fixture();
        // size > капа: байты «есть», но читать нельзя.
        push(
            &f,
            with_bodies(
                &f,
                with_headers(exchange("big.com"), &[], &[]),
                Some((b"needle-huge", BODY_SEARCH_CAP_BYTES + 1)),
                None,
            ),
        );
        // size == 0: тело не сохранено (cap захвата) — get → None.
        push(
            &f,
            with_bodies(
                &f,
                with_headers(exchange("zero.com"), &[], &[]),
                Some((b"needle-zero", 0)),
                None,
            ),
        );
        // BodyRef есть, байты из стора удалены.
        let mut orphan = with_headers(exchange("gone.com"), &[], &[]);
        orphan.request.body = Some(BodyRef {
            id: BodyId::new(),
            size: 100,
            content_type: None,
            encoding: None,
            storage: Default::default(),
        });
        push(&f, orphan);

        let hits = run(&f, query("needle")).expect("no errors, no hits");
        assert!(hits.is_empty());
    }

    #[test]
    fn limit_stops_early() {
        let f = fixture();
        for i in 0..3 {
            push(
                &f,
                with_headers(
                    exchange(&format!("h{i}.com")),
                    &[("X-Trace", "needle")],
                    &[],
                ),
            );
        }

        let hits = run(
            &f,
            SearchQueryDto {
                limit: 2,
                ..query("needle")
            },
        )
        .expect("hits");
        assert_eq!(hits.len(), 2);
        // Свежие сначала: лимит съедает самые поздние обмены.
        assert_eq!(hits[0].field, "header: X-Trace (request)");
    }

    #[test]
    fn multiple_hits_per_exchange() {
        let f = fixture();
        push(
            &f,
            with_bodies(
                &f,
                with_headers(
                    exchange("a.com"),
                    &[("X-One", "needle-1"), ("Other", "nope")],
                    &[("X-Two", "needle-2")],
                ),
                Some((b"needle-3", 8)),
                Some((b"needle-4", 8)),
            ),
        );

        let hits = run(&f, query("needle")).expect("hits");
        let fields: Vec<_> = hits.iter().map(|h| h.field.as_str()).collect();
        assert_eq!(
            fields,
            vec![
                "header: X-One (request)",
                "header: X-Two (response)",
                "body: request",
                "body: response",
            ]
        );
    }

    #[test]
    fn paging_covers_all_exchanges() {
        let f = fixture();
        // Больше LIST_PAGE (256) — обход в несколько страниц.
        for i in 0..600 {
            push(
                &f,
                with_headers(
                    exchange(&format!("h{i}.com")),
                    &[("X-Page", &format!("needle-{i}"))],
                    &[],
                ),
            );
        }

        let hits = run(
            &f,
            SearchQueryDto {
                limit: 1000,
                ..query("needle")
            },
        )
        .expect("hits");
        assert_eq!(hits.len(), 600);
        // Свежие сначала: первый хит — последний добавленный обмен.
        assert!(hits[0].snippet.contains("needle-599"));
        assert!(hits[599].snippet.contains("needle-0"));
    }
}
