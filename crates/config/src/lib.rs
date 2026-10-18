//! Конфигурация crabtrap.
//!
//! GUI-приложение: настройки правятся в UI, поэтому конфиг — TOML-файл
//! в app-config dir (`~/Library/Application Support/crabtrap/config.toml`),
//! а не env. Загружается в композиции (`app/main.rs`)
//! и передаётся туда, где нужен.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// Без Clone: std::io::Error не Clone (derive не компилировался).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("deserialize error: {0}")]
    Deserialize(#[from] toml::de::Error),
    #[error("cannot edit config.toml: {0}")]
    Edit(String),
}

impl From<toml_edit::TomlError> for ConfigError {
    fn from(e: toml_edit::TomlError) -> Self {
        Self::Edit(e.to_string())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub proxy: ProxyConfig,
    pub capture: CaptureConfig,
    pub storage: StorageConfig,
    pub ui: UiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    /// Адрес HTTP(S)-листенера прокси-движка.
    pub listen_addr: String,
    /// Адрес SOCKS5-листенера (Phase 9). None — не слушать.
    pub socks_listen_addr: Option<String>,
    /// Максимум одновременных клиентских соединений.
    pub max_concurrent_conns: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    /// Ёмкость ring buffer'а обменов.
    pub buffer_len: usize,
    /// Тела больше этого размера уходят на диск (spill).
    pub body_spill_threshold_bytes: u64,
    /// Сколько всего тел держим в памяти; сверх бюджета новые тела уходят
    /// на диск, даже маленькие (иначе 10k обменов могут съесть гигабайты).
    pub body_memory_budget_bytes: u64,
    /// Потолок захвата тела: выше — tee перестаёт писать, поток продолжается.
    pub body_capture_cap_bytes: u64,
    /// Захватывать ли тела вообще (только заголовки, если false).
    pub record_bodies: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Каталог сессий (Phase 8).
    pub sessions_dir: PathBuf,
    /// Каталог mirror-to-disk (Phase 8).
    pub mirror_dir: PathBuf,
    /// Кэш тел (spill): ~/Library/Caches/crabtrap/bodies.
    pub bodies_cache_dir: PathBuf,
    /// Каталог MITM-сертификатов (ca.pem / ca.key.pem, Phase 3).
    pub certs_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// "dark" | "light" | "system" (тема пока всегда следует за системой).
    pub theme: String,
    pub window_width: f32,
    pub window_height: f32,
    /// Закреплённые домены боковой панели.
    pub pinned_hosts: Vec<String>,
    pub sidebar_visible: bool,
    /// Где инспектор относительно таблицы.
    pub inspector_layout: InspectorLayout,
    /// Ширина консоли окна Script Editor, px.
    pub script_console_width: f32,
}

/// Раскладка инспектора.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InspectorLayout {
    #[default]
    Bottom,
    Right,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            // Все интерфейсы: телефон в той же сети проксируется через Mac
            // сразу (режим переключается в Settings → Network).
            listen_addr: "0.0.0.0:8080".to_string(),
            socks_listen_addr: None,
            max_concurrent_conns: 1024,
        }
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            buffer_len: 10_000,
            body_spill_threshold_bytes: 1024 * 1024, // 1 MiB
            body_memory_budget_bytes: 256 * 1024 * 1024, // 256 MiB
            body_capture_cap_bytes: 50 * 1024 * 1024, // 50 MiB
            record_bodies: true,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        let app_support = app_support_dir();
        Self {
            sessions_dir: app_support.join("sessions"),
            mirror_dir: app_support.join("mirror"),
            bodies_cache_dir: cache_dir().join("bodies"),
            certs_dir: app_support.join("certs"),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            window_width: 1400.0,
            window_height: 900.0,
            pinned_hosts: Vec::new(),
            sidebar_visible: true,
            inspector_layout: InspectorLayout::Bottom,
            script_console_width: DEFAULT_SCRIPT_CONSOLE_WIDTH,
        }
    }
}

/// Ширина консоли Script Editor по умолчанию, px.
pub const DEFAULT_SCRIPT_CONSOLE_WIDTH: f32 = 320.0;

impl AppConfig {
    /// Каталог конфигурации: `~/Library/Application Support/crabtrap`.
    pub fn app_dir() -> PathBuf {
        app_support_dir()
    }

    fn config_path() -> PathBuf {
        app_support_dir().join("config.toml")
    }

    /// Загрузить конфиг из `~/Library/Application Support/crabtrap`.
    /// Политика — см. [`Self::load_or_init_at`].
    pub fn load_or_init() -> Self {
        Self::load_or_init_at(&Self::config_path())
    }

    /// Загрузить конфиг по пути:
    /// - файла нет → дефолты, записываются на диск;
    /// - файл битый (не TOML/не та схема) → откладывается в
    ///   `config.toml.corrupt-<unix-ts>`, пишутся дефолты (правки
    ///   пользователя не теряются);
    /// - файл не читается (права, IO) → дефолты только в памяти, файл НЕ
    ///   перезаписывается.
    pub fn load_or_init_at(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(raw) => match toml::from_str::<AppConfig>(&raw) {
                Ok(cfg) => return cfg,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "invalid config.toml, moving aside and using defaults");
                    if !move_aside(path) {
                        // Не отложили — не затираем: правки пользователя дороже.
                        return Self::default();
                    }
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read config.toml, using defaults without overwriting");
                return Self::default();
            }
        }
        let cfg = Self::default();
        if let Err(e) = cfg.save_to(path) {
            tracing::warn!(error = %e, "failed to write default config");
        }
        cfg
    }

    /// Поменять `[ui]` в сохранённом конфиге. Файл перечитывается, а
    /// переписываются только ключи `[ui]`: комментарии, форматирование,
    /// неизвестные ключи и остальные секции остаются как были (умолчания не
    /// выписываются). Битый или нечитаемый файл не трогается (ошибка).
    pub fn update_ui(change: impl FnOnce(UiConfig) -> UiConfig) -> Result<UiConfig, ConfigError> {
        Self::update_ui_at(&Self::config_path(), change)
    }

    /// [`Self::update_ui`] по пути.
    pub fn update_ui_at(
        path: &Path,
        change: impl FnOnce(UiConfig) -> UiConfig,
    ) -> Result<UiConfig, ConfigError> {
        update_section(path, "ui", |cfg| change(cfg.ui))
    }

    /// Поменять `[proxy]` в сохранённом конфиге (как [`Self::update_ui`]).
    pub fn update_proxy(
        change: impl FnOnce(ProxyConfig) -> ProxyConfig,
    ) -> Result<ProxyConfig, ConfigError> {
        Self::update_proxy_at(&Self::config_path(), change)
    }

    /// [`Self::update_proxy`] по пути.
    pub fn update_proxy_at(
        path: &Path,
        change: impl FnOnce(ProxyConfig) -> ProxyConfig,
    ) -> Result<ProxyConfig, ConfigError> {
        update_section(path, "proxy", |cfg| change(cfg.proxy))
    }

    /// Атомарная запись в стандартный путь.
    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(&Self::config_path())
    }

    /// Атомарная запись (tmp + fsync + rename).
    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        write_atomic(path, &toml::to_string_pretty(self)?)
    }
}

/// Переписать одну секцию сохранённого конфига: файл перечитывается, меняются
/// только ключи секции `name` — комментарии, форматирование, неизвестные
/// ключи и остальные секции остаются как были. Битый файл не трогается.
fn update_section<T: Serialize>(
    path: &Path,
    name: &str,
    change: impl FnOnce(AppConfig) -> T,
) -> Result<T, ConfigError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    // Схема проверяется до правки: битый файл не переписываем.
    let current: AppConfig = toml::from_str(&raw)?;
    let mut doc: toml_edit::DocumentMut = raw.parse()?;
    let updated = change(current);
    let section_doc: toml_edit::DocumentMut = toml::to_string(&updated)?.parse()?;
    if !doc.contains_table(name) {
        doc.insert(name, toml_edit::table());
    }
    let table = doc[name]
        .as_table_mut()
        .ok_or_else(|| ConfigError::Edit(format!("[{name}] is not a table")))?;
    for (key, value) in section_doc.iter() {
        table.insert(key, value.clone());
    }
    write_atomic(path, &doc.to_string())?;
    Ok(updated)
}

/// Порт листенера по умолчанию (адрес без порта / с битым портом).
const DEFAULT_PORT: &str = "8080";

/// Хост и порт адреса листенера (`[::]:8080` → `::`, `8080`).
fn split_host_port(listen_addr: &str) -> (&str, &str) {
    let (host, port) = listen_addr
        .rsplit_once(':')
        .unwrap_or((listen_addr, DEFAULT_PORT));
    let port = if port.parse::<u16>().is_ok() {
        port
    } else {
        DEFAULT_PORT
    };
    (host.trim_start_matches('[').trim_end_matches(']'), port)
}

/// Видят ли листенер устройства в сети: loopback (`127.0.0.1`, `localhost`,
/// `::1`) доступен только этому Mac.
pub fn lan_access(listen_addr: &str) -> bool {
    let (host, _) = split_host_port(listen_addr);
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => !ip.is_loopback(),
        Err(_) => !host.eq_ignore_ascii_case("localhost"),
    }
}

/// Адрес листенера с тем же портом: все интерфейсы (`0.0.0.0`) или только
/// этот Mac (`127.0.0.1`).
pub fn with_lan_access(listen_addr: &str, allow: bool) -> String {
    let (_, port) = split_host_port(listen_addr);
    let host = if allow { "0.0.0.0" } else { "127.0.0.1" };
    format!("{host}:{port}")
}

/// Запись через временный файл с уникальным именем (pid + счётчик):
/// параллельные записи не пишут в один tmp, rename атомарен.
fn write_atomic(path: &Path, raw: &str) -> Result<(), ConfigError> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("toml.tmp.{}.{seq}", std::process::id()));
    let write = || -> Result<(), ConfigError> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(raw.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    };
    let result = write();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Отложить битый файл рядом: `<name>.corrupt-<unix-ts>`. false — не вышло.
fn move_aside(path: &Path) -> bool {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.toml");
    let aside = path.with_file_name(format!("{name}.corrupt-{ts}"));
    match fs::rename(path, &aside) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(from = %path.display(), error = %e, "cannot move corrupt config aside");
            false
        }
    }
}

fn app_support_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "crabtrap")
        .expect("cannot determine app directories")
        .config_dir()
        .to_path_buf()
}

fn cache_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "crabtrap")
        .expect("cannot determine app directories")
        .cache_dir()
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_in(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn missing_config_is_created_with_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let cfg = AppConfig::load_or_init_at(&path);
        assert_eq!(
            cfg.proxy.listen_addr,
            AppConfig::default().proxy.listen_addr
        );
        assert!(path.exists());
    }

    #[test]
    fn corrupt_config_is_moved_aside_not_lost() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, "this is = = not toml").expect("write");

        let cfg = AppConfig::load_or_init_at(&path);
        assert_eq!(
            cfg.capture.buffer_len,
            AppConfig::default().capture.buffer_len
        );

        let names = names_in(dir.path());
        let aside = names
            .iter()
            .find(|n| n.starts_with("config.toml.corrupt-"))
            .unwrap_or_else(|| panic!("user file kept aside: {names:?}"));
        assert_eq!(
            fs::read_to_string(dir.path().join(aside)).expect("read aside"),
            "this is = = not toml"
        );
        assert!(path.exists(), "fresh defaults written");
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_config_is_not_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, "[proxy]\nlisten_addr = \"127.0.0.1:9999\"\n").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod");

        let cfg = AppConfig::load_or_init_at(&path);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore");

        assert_eq!(
            cfg.proxy.listen_addr,
            AppConfig::default().proxy.listen_addr
        );
        assert!(
            fs::read_to_string(&path).expect("read").contains("9999"),
            "user config must survive an IO error"
        );
        assert_eq!(names_in(dir.path()), vec!["config.toml".to_string()]);
    }

    #[test]
    fn update_ui_touches_only_ui_keys_and_keeps_comments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let original = "# my proxy\n[proxy]\nlisten_addr = \"127.0.0.1:9999\" # custom port\nfuture_key = 1\n\n[ui]\ntheme = \"dark\"\n";
        fs::write(&path, original).expect("write");

        let updated = AppConfig::update_ui_at(&path, |ui| UiConfig {
            pinned_hosts: vec!["api.test".into()],
            inspector_layout: InspectorLayout::Right,
            ..ui
        })
        .expect("update");
        assert_eq!(updated.pinned_hosts, vec!["api.test".to_string()]);

        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.contains("# my proxy"), "comment kept:\n{raw}");
        assert!(raw.contains("# custom port"), "inline comment kept:\n{raw}");
        assert!(raw.contains("future_key = 1"), "unknown key kept:\n{raw}");
        assert!(
            !raw.contains("buffer_len"),
            "defaults not written out:\n{raw}"
        );
        let reread = AppConfig::load_or_init_at(&path);
        assert_eq!(reread.proxy.listen_addr, "127.0.0.1:9999");
        assert_eq!(reread.ui.inspector_layout, InspectorLayout::Right);
        assert_eq!(reread.ui.pinned_hosts, vec!["api.test".to_string()]);
    }

    #[test]
    fn update_ui_creates_missing_file_and_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        AppConfig::update_ui_at(&path, |ui| UiConfig {
            sidebar_visible: false,
            ..ui
        })
        .expect("update");
        assert!(!AppConfig::load_or_init_at(&path).ui.sidebar_visible);
        assert_eq!(
            names_in(dir.path()),
            vec!["config.toml".to_string()],
            "no tmp left"
        );
    }

    #[test]
    fn default_listener_accepts_lan_devices() {
        assert!(lan_access(&AppConfig::default().proxy.listen_addr));
    }

    #[test]
    fn lan_access_follows_listen_host() {
        assert!(!lan_access("127.0.0.1:8080"));
        assert!(!lan_access("localhost:8080"));
        assert!(!lan_access("[::1]:8080"));
        assert!(lan_access("0.0.0.0:8080"));
        assert!(lan_access("[::]:8080"));
        assert!(lan_access("192.168.1.118:8080"));
    }

    #[test]
    fn with_lan_access_switches_host_and_keeps_port() {
        assert_eq!(with_lan_access("127.0.0.1:9090", true), "0.0.0.0:9090");
        assert_eq!(with_lan_access("0.0.0.0:9090", false), "127.0.0.1:9090");
        assert_eq!(with_lan_access("[::]:3128", false), "127.0.0.1:3128");
        assert_eq!(with_lan_access("junk", true), "0.0.0.0:8080");
    }

    #[test]
    fn update_proxy_touches_only_proxy_keys_and_keeps_comments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let original = "# mine\n[proxy]\nlisten_addr = \"127.0.0.1:9999\"\nfuture_key = 1\n\n[ui]\ntheme = \"dark\" # keep\n";
        fs::write(&path, original).expect("write");

        AppConfig::update_proxy_at(&path, |proxy| ProxyConfig {
            listen_addr: "0.0.0.0:9999".into(),
            ..proxy
        })
        .expect("update");

        let raw = fs::read_to_string(&path).expect("read");
        assert!(
            raw.contains("# mine") && raw.contains("# keep"),
            "comments kept:\n{raw}"
        );
        assert!(raw.contains("future_key = 1"), "unknown key kept:\n{raw}");
        assert_eq!(
            AppConfig::load_or_init_at(&path).proxy.listen_addr,
            "0.0.0.0:9999"
        );
    }

    #[test]
    fn update_refuses_to_overwrite_corrupt_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, "not = = toml").expect("write");
        assert!(AppConfig::update_ui_at(&path, |ui| ui).is_err());
        assert_eq!(fs::read_to_string(&path).expect("read"), "not = = toml");
    }

    #[test]
    fn old_config_without_new_ui_fields_still_loads() {
        let parsed: AppConfig =
            toml::from_str("[ui]\ntheme = \"dark\"\nwindow_width = 1200.0\n").expect("parse");
        assert!(parsed.ui.sidebar_visible);
        assert!(parsed.ui.pinned_hosts.is_empty());
        assert_eq!(parsed.ui.inspector_layout, InspectorLayout::Bottom);
    }

    #[test]
    fn default_config_roundtrips_through_toml() {
        let cfg = AppConfig::default();
        let raw = toml::to_string_pretty(&cfg).expect("serialize");
        let parsed: AppConfig = toml::from_str(&raw).expect("deserialize");
        assert_eq!(parsed.proxy.listen_addr, cfg.proxy.listen_addr);
        assert_eq!(parsed.capture.buffer_len, cfg.capture.buffer_len);
    }
}
