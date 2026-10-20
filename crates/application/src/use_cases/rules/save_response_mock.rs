//! «Save Response & Map Local…»: тело захваченного ответа (без
//! Content-Encoding) → файл в каталоге моков, ответ правила (status,
//! Content-Type, заголовки) — из захваченного. Правило не создаётся: UI
//! открывает диалог Map Local с этими значениями, пользователь жмёт Save.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use domain::entities::ExchangeStatus;
use domain::entities::exchange::HttpResponse;
use domain::errors::DomainError;
use domain::repositories::{BodyRepository, ExchangeRepository};
use domain::value_objects::ExchangeId;
use domain::value_objects::http_vo::HeaderEntry;

use super::edit::MapLocalResponse;
use super::response_form::{MAP_LOCAL_STATUS_RANGE, content_type_error, map_local_header_error};
use crate::services::{BodyDecoderService, MockFileStore};

/// Заголовки ответа, которые не переносятся в мок: тело в файле уже
/// распаковано; куки, дата, ETag и Alt-Svc (уводит клиента на HTTP/3 мимо
/// прокси) описывают тот ответ, а не мок.
const DROPPED_MOCK_HEADERS: [&str; 5] =
    ["content-encoding", "set-cookie", "date", "alt-svc", "etag"];

/// Имя файла для пути, оканчивающегося на `/`.
const INDEX_FILE_STEM: &str = "index";

/// Расширение для неизвестного Content-Type у имени без расширения.
const UNKNOWN_TYPE_EXTENSION: &str = "bin";

/// Длина сегмента пути в символах (ASCII после очистки): с суффиксом `-N`
/// и расширением имя остаётся под лимитом 255 байт файловой системы.
const MAX_SEGMENT_CHARS: usize = 100;

/// Что сохранено: файл и ответ для диалога Map Local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedMock {
    pub local_path: PathBuf,
    pub response: MapLocalResponse,
}

pub struct SaveResponseMockUseCase {
    exchanges: Arc<dyn ExchangeRepository>,
    bodies: Arc<dyn BodyRepository>,
    decoder: Arc<dyn BodyDecoderService>,
    files: Arc<dyn MockFileStore>,
}

impl SaveResponseMockUseCase {
    pub fn new(
        exchanges: Arc<dyn ExchangeRepository>,
        bodies: Arc<dyn BodyRepository>,
        decoder: Arc<dyn BodyDecoderService>,
        files: Arc<dyn MockFileStore>,
    ) -> Self {
        Self {
            exchanges,
            bodies,
            decoder,
            files,
        }
    }

    /// Сохранить тело ответа обмена. Нет ответа или тело не захвачено —
    /// ошибка (урезанный мок хуже никакого).
    pub async fn execute(&self, id: &ExchangeId) -> Result<SavedMock, DomainError> {
        let exchange = self
            .exchanges
            .get(id)
            .await?
            .ok_or_else(|| DomainError::NotFound(format!("exchange {id}")))?;
        let response = exchange
            .response
            .as_ref()
            .filter(|_| exchange.status == ExchangeStatus::Completed)
            .ok_or_else(|| {
                DomainError::ValidationError(
                    "the response is not complete yet (still loading or failed)".into(),
                )
            })?;
        let body = self.captured_body(response).await?;
        let relative = mock_relative_path(
            exchange.host.as_str(),
            &url_path(&exchange.request.uri),
            header(&response.headers, "content-type"),
        );
        let local_path = self.files.save(&relative, &body).await?;
        Ok(SavedMock {
            local_path,
            response: mock_response(response),
        })
    }

    /// Распакованное тело ответа; пустое — если тела нет и по проводу ничего.
    async fn captured_body(&self, response: &HttpResponse) -> Result<Vec<u8>, DomainError> {
        let Some(body_ref) = &response.body else {
            return match response.raw_size {
                0 => Ok(Vec::new()),
                _ => Err(not_captured()),
            };
        };
        if body_ref.size == 0 && response.raw_size > 0 {
            return Err(not_captured());
        }
        let bytes = self.bodies.get(body_ref).await?.ok_or_else(not_captured)?;
        match body_ref.encoding.as_deref() {
            None => Ok(bytes),
            Some(encoding) if is_identity_encoding(encoding) => Ok(bytes),
            // Неизвестную кодировку декодер пропускает как есть — такой файл
            // без Content-Encoding отдавал бы мусор.
            Some(encoding) if !self.decoder.supported(encoding).await => {
                Err(DomainError::ValidationError(format!(
                    "the response uses Content-Encoding '{encoding}', which crabtrap can't decode"
                )))
            }
            Some(encoding) => self.decoder.decode(&bytes, encoding).await,
        }
    }
}

/// Кодировка не меняет байты: пусто или только `identity`.
fn is_identity_encoding(encoding: &str) -> bool {
    encoding
        .split(',')
        .map(str::trim)
        .all(|codec| codec.is_empty() || codec.eq_ignore_ascii_case("identity"))
}

fn not_captured() -> DomainError {
    DomainError::ValidationError(
        "the response body was not captured (body recording is off or it is too large)".into(),
    )
}

fn header<'a>(headers: &'a [HeaderEntry], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

/// Path запроса без query: из absolute-form или origin-form URI.
fn url_path(uri: &str) -> String {
    let without_query = uri.split(['?', '#']).next().unwrap_or_default();
    match without_query.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]).to_string(),
        None => without_query.to_string(),
    }
}

/// Относительный путь файла мока: `<host>/<сегменты path>`; путь на `/` —
/// `index`. Расширение — по Content-Type (если имя им ещё не оканчивается),
/// для неизвестного типа без расширения — `.bin`: у файла всегда есть
/// расширение, и `/users` (файл `users.bin`) не занимает имя каталога для
/// `/users/1`. Сегменты сводятся к `[A-Za-z0-9._-]` и укорачиваются,
/// `.`/`..` выбрасываются, ведущая точка заменяется — путь не выходит за
/// каталог моков и не прячет файл.
pub fn mock_relative_path(host: &str, url_path: &str, content_type: Option<&str>) -> PathBuf {
    let mut relative = PathBuf::from(safe_segment(host).unwrap_or_else(|| "host".into()));
    let segments: Vec<String> = url_path.split('/').filter_map(safe_segment).collect();
    let file = match (url_path.ends_with('/'), segments.split_last()) {
        (false, Some((last, dirs))) => {
            relative.extend(dirs);
            last.clone()
        }
        _ => {
            relative.extend(&segments);
            INDEX_FILE_STEM.to_string()
        }
    };
    relative.push(with_extension(file, content_type.and_then(extension_for)));
    relative
}

/// Имя файла с расширением типа (`known`) или, без него, хотя бы `.bin`.
fn with_extension(file: String, known: Option<&str>) -> String {
    let current = Path::new(&file)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match (known, current) {
        (Some(ext), Some(current)) if current == ext => file,
        (Some(ext), _) => format!("{file}.{ext}"),
        (None, Some(_)) => file,
        (None, None) => format!("{file}.{UNKNOWN_TYPE_EXTENSION}"),
    }
}

/// Сегмент пути, безопасный как имя файла (`None` — пустой, `.` или `..`).
fn safe_segment(raw: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
    if matches!(decoded.as_ref(), "" | "." | "..") {
        return None;
    }
    let cleaned: String = decoded
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '_',
        })
        .take(MAX_SEGMENT_CHARS)
        .collect();
    Some(match cleaned.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None => cleaned,
    })
}

/// Расширение файла мока по Content-Type (обратно к таблице движка).
fn extension_for(content_type: &str) -> Option<&'static str> {
    let mime = content_type.split(';').next()?.trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "application/json" => "json",
        m if m.ends_with("+json") => "json",
        "text/html" => "html",
        "text/javascript" | "application/javascript" => "js",
        "text/css" => "css",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "application/xml" | "text/xml" => "xml",
        "image/svg+xml" => "svg",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "application/pdf" => "pdf",
        "application/wasm" => "wasm",
        _ => return None,
    })
}

/// Ответ правила из захваченного: код (не финальный → 200), Content-Type
/// (некорректный → по расширению), заголовки, которые Map Local может
/// отдать и которые описывают мок, а не тот конкретный ответ.
pub fn mock_response(response: &HttpResponse) -> MapLocalResponse {
    let status = if MAP_LOCAL_STATUS_RANGE.contains(&response.status) {
        response.status
    } else {
        MapLocalResponse::default().status
    };
    let content_type = header(&response.headers, "content-type")
        .map(str::trim)
        .filter(|ct| content_type_error(ct).is_none())
        .map(str::to_string);
    let headers = response
        .headers
        .iter()
        .filter(|h| {
            !h.name.eq_ignore_ascii_case("content-type")
                && !DROPPED_MOCK_HEADERS
                    .iter()
                    .any(|d| d.eq_ignore_ascii_case(&h.name))
                && map_local_header_error(&h.name, &h.value).is_none()
        })
        .cloned()
        .collect();
    MapLocalResponse {
        status,
        content_type,
        headers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, headers: &[(&str, &str)]) -> HttpResponse {
        HttpResponse {
            status,
            reason: None,
            headers: headers
                .iter()
                .map(|(n, v)| HeaderEntry::new(*n, *v))
                .collect(),
            body: None,
            raw_size: 0,
            version: Default::default(),
        }
    }

    #[test]
    fn path_mirrors_host_and_url_path() {
        assert_eq!(
            mock_relative_path("api.test", "/v1/users.json", None),
            PathBuf::from("api.test/v1/users.json")
        );
        assert_eq!(
            mock_relative_path(
                "api.test",
                "/v1/users",
                Some("application/json; charset=utf-8")
            ),
            PathBuf::from("api.test/v1/users.json")
        );
        assert_eq!(
            mock_relative_path("api.test", "/v1/", Some("text/html")),
            PathBuf::from("api.test/v1/index.html")
        );
        assert_eq!(
            mock_relative_path("api.test", "/", None),
            PathBuf::from("api.test/index.bin")
        );
        assert_eq!(
            mock_relative_path("api.test", "/v1/problem", Some("application/problem+json")),
            PathBuf::from("api.test/v1/problem.json")
        );
    }

    #[test]
    fn path_cannot_escape_or_hide() {
        assert_eq!(
            mock_relative_path("api.test", "/a/../../etc/%2E%2E/passwd", None),
            PathBuf::from("api.test/a/etc/passwd.bin")
        );
        assert_eq!(
            mock_relative_path("api.test", "/.env", None),
            PathBuf::from("api.test/_env.bin")
        );
        assert_eq!(
            mock_relative_path("api.test", "/x/.htaccess", Some("text/plain")),
            PathBuf::from("api.test/x/_htaccess.txt")
        );
        assert_eq!(
            mock_relative_path("api.test", "/a%2Fb/c d", None),
            PathBuf::from("api.test/a_b/c_d.bin")
        );
        assert_eq!(
            mock_relative_path("..", "/x", None),
            PathBuf::from("host/x.bin")
        );
        assert_eq!(
            mock_relative_path("api.test:8443", "/x", None),
            PathBuf::from("api.test_8443/x.bin")
        );
    }

    #[test]
    fn file_never_shadows_a_folder_of_the_same_name() {
        let file = mock_relative_path("api.test", "/api/users", None);
        let nested = mock_relative_path("api.test", "/api/users/1", None);
        assert_eq!(file, PathBuf::from("api.test/api/users.bin"));
        assert_eq!(nested, PathBuf::from("api.test/api/users/1.bin"));
        assert_eq!(
            mock_relative_path("api.test", "/api/v1.2", Some("application/json")),
            PathBuf::from("api.test/api/v1.2.json"),
            "dotted names still get the Content-Type extension"
        );
    }

    #[test]
    fn long_segments_are_shortened() {
        let long = "a".repeat(400);
        let path = mock_relative_path("api.test", &format!("/{long}/{long}"), Some("text/plain"));
        assert!(
            path.components()
                .all(|c| c.as_os_str().len() <= MAX_SEGMENT_CHARS + ".txt".len()),
            "{path:?}"
        );
    }

    #[test]
    fn identity_only_encodings_need_no_decoding() {
        assert!(is_identity_encoding(""));
        assert!(is_identity_encoding(" Identity "));
        assert!(is_identity_encoding("identity, identity"));
        assert!(!is_identity_encoding("gzip"));
        assert!(!is_identity_encoding("dcb"));
    }

    #[test]
    fn url_path_drops_scheme_host_and_query() {
        assert_eq!(url_path("https://a.test/v1/x?y=1#f"), "/v1/x");
        assert_eq!(url_path("https://a.test"), "/");
        assert_eq!(url_path("/v1/x?y=1"), "/v1/x");
    }

    #[test]
    fn response_keeps_status_type_and_mock_safe_headers() {
        let captured = response(
            404,
            &[
                ("Content-Type", "application/json"),
                ("Content-Length", "12"),
                ("Content-Encoding", "gzip"),
                ("Transfer-Encoding", "chunked"),
                ("Set-Cookie", "sid=1"),
                ("Date", "Mon, 20 Oct 2026 10:00:00 GMT"),
                ("Alt-Svc", "h3=\":443\""),
                ("Access-Control-Allow-Origin", "*"),
                ("Cache-Control", "no-store"),
                ("Connection", "keep-alive"),
            ],
        );
        assert_eq!(
            mock_response(&captured),
            MapLocalResponse {
                status: 404,
                content_type: Some("application/json".into()),
                headers: vec![
                    HeaderEntry::new("Access-Control-Allow-Origin", "*"),
                    HeaderEntry::new("Cache-Control", "no-store"),
                ],
            }
        );
    }

    #[test]
    fn non_final_status_and_bad_content_type_fall_back_to_defaults() {
        let captured = response(101, &[("Content-Type", "garbage")]);
        assert_eq!(mock_response(&captured), MapLocalResponse::default());
    }
}
