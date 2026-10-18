//! rules.json в app-support dir: load-or-init, атомарная запись
//! (tmp+rename), карантин битого файла (`rules.json.corrupt-<ts>`) со
//! стартом с пустого набора (см. «Персистентность» в PLAN.md).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use chrono::Utc;

use domain::entities::rule::RuleSet;
use domain::errors::DomainError;
use domain::repositories::RuleRepository;

pub struct FileRuleRepository {
    /// Полный путь к rules.json (инъектируется — тестируется на tempdir).
    path: PathBuf,
    state: Mutex<Option<RuleSet>>,
    version: AtomicU64,
}

impl FileRuleRepository {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: Mutex::new(None),
            version: AtomicU64::new(0),
        }
    }

    /// `rules.json` в app-support dir (из каталогов конфига).
    pub fn from_config(_config: &config::AppConfig) -> Self {
        Self::new(config::AppConfig::app_dir().join("rules.json"))
    }

    /// Загрузить с диска (один раз), при отсутствии — инициализировать
    /// пустой набор и записать файл.
    fn ensure_loaded(&self) -> Result<RuleSet, DomainError> {
        let mut guard = self.state.lock().expect("lock");
        if let Some(rules) = guard.as_ref() {
            return Ok(rules.clone());
        }

        let rules = match fs::read_to_string(&self.path) {
            Ok(raw) => match serde_json::from_str::<RuleSet>(&raw) {
                Ok(rules) => rules,
                Err(e) => {
                    // Битый файл — в карантин, старт с пустого набора.
                    quarantine(&self.path);
                    tracing::warn!(
                        path = %self.path.display(),
                        error = %e,
                        "rules.json is corrupt, quarantined and reset to empty ruleset"
                    );
                    RuleSet::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RuleSet::default(),
            Err(e) => {
                return Err(DomainError::RepositoryError(format!(
                    "cannot read {}: {e}",
                    self.path.display()
                )));
            }
        };

        // Умолчания SSL Proxying (исключения доменов с пиннингом Apple) —
        // один раз; файл переписывается, только если что-то добавилось.
        let defaults_before = rules.ssl_defaults_version;
        let rules = rules.with_ssl_defaults();
        if !self.path.exists() || rules.ssl_defaults_version != defaults_before {
            persist(&self.path, &rules)?;
        }
        self.version.store(rules.version, Ordering::SeqCst);
        *guard = Some(rules.clone());
        Ok(rules)
    }
}

#[async_trait]
impl RuleRepository for FileRuleRepository {
    async fn get(&self) -> Result<RuleSet, DomainError> {
        self.ensure_loaded()
    }

    async fn update(&self, mut rules: RuleSet) -> Result<u64, DomainError> {
        self.ensure_loaded()?;
        let version = self.version.fetch_add(1, Ordering::SeqCst) + 1;
        rules.version = version;
        persist(&self.path, &rules)?;
        *self.state.lock().expect("lock") = Some(rules);
        Ok(version)
    }

    async fn version(&self) -> Result<u64, DomainError> {
        self.ensure_loaded()?;
        Ok(self.version.load(Ordering::SeqCst))
    }
}

/// Атомарная запись: tmp + rename.
fn persist(path: &Path, rules: &RuleSet) -> Result<(), DomainError> {
    let raw = serde_json::to_string_pretty(rules)
        .map_err(|e| DomainError::RepositoryError(format!("serialize rules: {e}")))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| DomainError::InfrastructureError(format!("rules dir: {e}")))?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, raw)
        .map_err(|e| DomainError::InfrastructureError(format!("write rules tmp: {e}")))?;
    fs::rename(&tmp, path)
        .map_err(|e| DomainError::InfrastructureError(format!("rename rules: {e}")))?;
    Ok(())
}

/// Переименовать битый файл в `rules.json.corrupt-<unix_ts>`.
fn quarantine(path: &Path) {
    let ts = Utc::now().timestamp();
    let corrupt = path.with_file_name(format!(
        "{}.corrupt-{ts}",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    if let Err(e) = fs::rename(path, &corrupt) {
        tracing::warn!(
            from = %path.display(),
            to = %corrupt.display(),
            error = %e,
            "cannot quarantine corrupt rules.json"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::entities::rule::{BlockRule, SslProxyEntry};
    use domain::entities::ssl_proxy::{APPLE_PINNED_HOSTS, SSL_DEFAULTS_VERSION};

    /// Пустой набор, в котором только умолчания SSL Proxying (их id —
    /// из `rules`: случайные).
    fn only_ssl_defaults(rules: &RuleSet) -> RuleSet {
        let patterns: Vec<_> = rules
            .ssl_proxy
            .iter()
            .map(|e| e.host_pattern.as_str())
            .collect();
        assert_eq!(patterns, APPLE_PINNED_HOSTS.to_vec());
        assert!(rules.ssl_proxy.iter().all(|e| e.exclude && e.enabled));
        RuleSet {
            ssl_proxy: rules.ssl_proxy.clone(),
            ssl_defaults_version: SSL_DEFAULTS_VERSION,
            ..RuleSet::default()
        }
    }

    #[tokio::test]
    async fn legacy_rules_get_apple_exclusions_once() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rules.json");
        let star = SslProxyEntry::include("*");
        let legacy = RuleSet {
            ssl_proxy: vec![star.clone()],
            ..RuleSet::default()
        };
        fs::write(&path, serde_json::to_string(&legacy).unwrap()).unwrap();

        let rules = FileRuleRepository::new(path.clone()).get().await.unwrap();
        assert_eq!(rules.ssl_proxy[0], star, "user entries first, untouched");
        assert_eq!(rules.ssl_proxy.len(), 1 + APPLE_PINNED_HOSTS.len());

        // Записано на диск: следующий запуск ничего не добавляет.
        let reread: RuleSet = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reread.ssl_defaults_version, SSL_DEFAULTS_VERSION);
        assert_eq!(FileRuleRepository::new(path).get().await.unwrap(), rules);
    }
    use domain::value_objects::{RuleId, TrafficMatch};

    #[tokio::test]
    async fn initializes_default_ruleset_on_first_use() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rules.json");
        let repo = FileRuleRepository::new(path.clone());

        let rules = repo.get().await.unwrap();
        assert_eq!(rules, only_ssl_defaults(&rules));
        assert_eq!(rules.version, 0);
        // load-or-init: файл создан сразу.
        assert!(path.exists());
    }

    #[tokio::test]
    async fn update_bumps_version_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rules.json");
        let repo = FileRuleRepository::new(path.clone());

        let rules = RuleSet {
            ssl_proxy: vec![SslProxyEntry::include("*.example.com")],
            ssl_defaults_version: SSL_DEFAULTS_VERSION,
            ..Default::default()
        };
        let v1 = repo.update(rules).await.unwrap();
        assert_eq!(v1, 1);
        assert_eq!(repo.version().await.unwrap(), 1);

        // Новый экземпляр (перезапуск приложения) читает то же самое.
        let repo2 = FileRuleRepository::new(path);
        let loaded = repo2.get().await.unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.ssl_proxy.len(), 1);
        assert_eq!(loaded.ssl_proxy[0].host_pattern, "*.example.com");

        let v2 = repo2
            .update(RuleSet {
                blocks: vec![BlockRule {
                    id: RuleId::new(),
                    r#match: TrafficMatch::default(),
                    action: domain::entities::rule::BlockAction::Status(403),
                    enabled: true,
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(v2, 2);
    }

    #[tokio::test]
    async fn corrupt_file_is_quarantined_and_reset() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rules.json");
        fs::write(&path, "{ это не json").unwrap();

        let repo = FileRuleRepository::new(path.clone());
        let rules = repo.get().await.unwrap();
        assert_eq!(rules, only_ssl_defaults(&rules));

        // Битый файл переименован в *.corrupt-<ts>, на его месте валидный.
        let mut entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(entries.len(), 2, "corrupt + rewritten: {entries:?}");
        // "rules.json" — префикс "rules.json.corrupt-…", поэтому при сортировке
        // валидный файл идёт первым.
        assert_eq!(entries[0], "rules.json");
        assert!(entries[1].starts_with("rules.json.corrupt-"));
    }

    #[tokio::test]
    async fn no_tmp_files_left_after_save() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("rules.json");
        let repo = FileRuleRepository::new(path.clone());
        repo.update(RuleSet::default()).await.unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
