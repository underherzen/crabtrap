//! Mirror: успешный (200) GET хоста из списка Mirror → тело ответа
//! (распакованное) в файл `<папка>/<host>/<path>`; ошибка или POST на тот же
//! путь хорошую копию не затирают. `/` и путь на `/` —
//! `index.html`; query отбрасывается; сегменты пути раскодируются и
//! обезвреживаются — файл всегда внутри выбранной папки.

use std::path::PathBuf;
use std::sync::Arc;

use domain::entities::exchange::ExchangeStatus;
use domain::errors::DomainError;
use domain::repositories::{BodyRepository, ExchangeRepository, RuleRepository};
use domain::value_objects::ExchangeId;

use crate::services::{BodyDecoderService, MirrorFileWriter};

/// Имя файла для каталога (`/`, `/docs/`).
const DIRECTORY_INDEX: &str = "index.html";
/// Потолок имени файла macOS (байт).
const MAX_NAME_BYTES: usize = 255;

/// Сохраняет ответы хостов из списка Mirror в файлы.
pub struct MirrorResponseUseCase {
    exchanges: Arc<dyn ExchangeRepository>,
    bodies: Arc<dyn BodyRepository>,
    decoder: Arc<dyn BodyDecoderService>,
    rules: Arc<dyn RuleRepository>,
    writer: Arc<dyn MirrorFileWriter>,
}

impl MirrorResponseUseCase {
    pub fn new(
        exchanges: Arc<dyn ExchangeRepository>,
        bodies: Arc<dyn BodyRepository>,
        decoder: Arc<dyn BodyDecoderService>,
        rules: Arc<dyn RuleRepository>,
        writer: Arc<dyn MirrorFileWriter>,
    ) -> Self {
        Self {
            exchanges,
            bodies,
            decoder,
            rules,
            writer,
        }
    }

    /// Сохранить ответ обмена, если его хост в списке Mirror. `None` — не
    /// зеркалируется (хост не в списке, нет ответа или тело не записано).
    pub async fn mirror(&self, id: &ExchangeId) -> Result<Option<PathBuf>, DomainError> {
        let Some(exchange) = self.exchanges.get(id).await? else {
            return Ok(None);
        };
        let successful_get = exchange.status == ExchangeStatus::Completed
            && exchange.request.method.as_str() == "GET"
            && exchange
                .response
                .as_ref()
                // 206 (Range) — кусок файла: полную копию не затирает.
                .is_some_and(|r| r.status == 200);
        if !successful_get {
            return Ok(None);
        }
        let rules = self.rules.get().await?;
        let Some(folder) = rules.mirror_folder_for(exchange.host.as_str()) else {
            return Ok(None);
        };
        let Some(body_ref) = exchange.response.as_ref().and_then(|r| r.body.as_ref()) else {
            return Ok(None);
        };
        let Some(raw) = self.bodies.get(body_ref).await? else {
            return Ok(None);
        };
        let data = match body_ref.encoding.as_deref() {
            Some(encoding) => self.decoder.decode(&raw, encoding).await?,
            None => raw,
        };
        let Some(relative) = mirror_path(&exchange.request.uri) else {
            tracing::debug!(%id, uri = %exchange.request.uri, "mirror: no host in the URI, skipped");
            return Ok(None);
        };
        self.writer.write(folder, &relative, &data).map(Some)
    }
}

/// Путь файла внутри папки: `<host>[_port]/<сегменты>`; `None` — URL без
/// хоста.
pub fn mirror_path(uri: &str) -> Option<PathBuf> {
    let url = url::Url::parse(uri).ok()?;
    let host = url.host_str()?.trim_matches(['[', ']']);
    let top = match url.port() {
        Some(port) => format!("{host}_{port}"),
        None => host.to_string(),
    };
    let mut path = PathBuf::from(safe_name(&top));
    let segments: Vec<String> = url
        .path()
        .split('/')
        .map(|s| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        })
        .collect();
    let (dirs, file) = segments.split_at(segments.len().saturating_sub(1));
    for dir in dirs.iter().filter(|d| !d.is_empty()) {
        path.push(safe_name(dir));
    }
    match file.first().map(String::as_str) {
        None | Some("") => path.push(DIRECTORY_INDEX),
        Some(name) => path.push(safe_name(name)),
    }
    Some(path)
}

/// Имя файла или папки без опасного: `.`/`..` и разделители заменяются,
/// управляющие символы убираются, длина — до [`MAX_NAME_BYTES`].
fn safe_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '/' | '\\' | ':' => '_',
            c => c,
        })
        .collect();
    let named = match cleaned.as_str() {
        "" | "." | ".." => "_".repeat(cleaned.len().max(1)),
        _ => cleaned,
    };
    truncate_bytes(named, MAX_NAME_BYTES)
}

fn truncate_bytes(mut name: String, max: usize) -> String {
    if name.len() > max {
        let cut = (0..=max)
            .rev()
            .find(|&i| name.is_char_boundary(i))
            .unwrap_or(0);
        name.truncate(cut);
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(uri: &str) -> String {
        mirror_path(uri).expect("path").display().to_string()
    }

    #[test]
    fn urls_map_to_a_host_tree() {
        assert_eq!(path("https://api.test/v1/users?id=7"), "api.test/v1/users");
        assert_eq!(path("https://api.test/"), "api.test/index.html");
        assert_eq!(path("https://api.test"), "api.test/index.html");
        assert_eq!(path("https://api.test/docs/"), "api.test/docs/index.html");
        assert_eq!(path("http://api.test:8080/a//b"), "api.test_8080/a/b");
        assert_eq!(path("http://[::1]:3000/x"), "__1_3000/x");
    }

    #[test]
    fn paths_never_leave_the_folder() {
        for uri in [
            "https://api.test/../../etc/passwd",
            "https://api.test/%2e%2e/%2e%2e/secret",
            "https://api.test/a/%2F..%2Fb",
            "https://api.test/a\\..\\b",
        ] {
            let relative = mirror_path(uri).expect("path");
            assert!(
                relative
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                "{uri} → {}",
                relative.display()
            );
        }
        // URL сам схлопывает `..` (и `%2e%2e`); оставшееся — заменяется.
        assert_eq!(path("https://api.test/%2e%2e/x"), "api.test/x");
        assert_eq!(path("https://api.test/a/%2F..%2Fb"), "api.test/a/_.._b");
    }

    #[test]
    fn names_are_cleaned_and_capped() {
        assert_eq!(safe_name("a\u{0}b\nc"), "abc");
        assert_eq!(safe_name("."), "_");
        assert_eq!(safe_name(""), "_");
        assert_eq!(
            safe_name(&"é".repeat(200)).len(),
            254,
            "cut on a char boundary"
        );
    }
}
