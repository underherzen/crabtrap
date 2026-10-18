//! Реализация [`SystemProxyService`] через CLI `networksetup` (macOS).
//!
//! Кнопка «macOS Proxy» (как Proxy → macOS Proxy в Charles): прописывает
//! HTTP/HTTPS-прокси crabtrap во все включённые сетевые сервисы и снимает
//! его обратно. `networksetup` работает без sudo для пользователя-админа;
//! ошибки CLI не глотаем — наружу DomainError::InfrastructureError со stderr.

use async_trait::async_trait;
use domain::errors::DomainError;
use domain::value_objects::SocketAddrVo;
use tokio::process::Command;

use application::dto::proxy_config::SystemProxyStatusDto;
use application::services::SystemProxyService;

/// Bypass-домены по умолчанию (если UI не передал свои).
pub const DEFAULT_BYPASS: &[&str] = &["127.0.0.1", "localhost", "*.local", "169.254.0.0/16"];

pub struct NetworksetupSystemProxyService {
    /// Переопределение бинарника (тесты: путь к echo-скрипту).
    #[allow(dead_code)]
    binary: String,
}

impl NetworksetupSystemProxyService {
    pub fn new() -> Self {
        Self {
            binary: "networksetup".to_string(),
        }
    }
}

impl Default for NetworksetupSystemProxyService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SystemProxyService for NetworksetupSystemProxyService {
    async fn enable(&self, listen: SocketAddrVo, bypass: Vec<String>) -> Result<(), DomainError> {
        let host = listen.host.as_str().to_string();
        let port = listen.port.to_string();
        let bypass = if bypass.is_empty() {
            DEFAULT_BYPASS.iter().map(|s| s.to_string()).collect()
        } else {
            bypass
        };
        // Сбой одного сервиса (VPN, отключённый интерфейс) не должен
        // оставлять остальные без прокси: идём по всем, ошибки копим.
        let mut failures = Vec::new();
        let services = self.list_services().await?;
        for service in &services {
            let mut result = Ok(String::new());
            for args in [
                str_vec(&["-setwebproxy", service, &host, &port]),
                str_vec(&["-setsecurewebproxy", service, &host, &port]),
                bypass_args(service, &bypass),
            ] {
                result = run_networksetup(&self.binary, &args).await;
                if result.is_err() {
                    break;
                }
            }
            match result {
                Ok(_) => {
                    tracing::info!(service = %service, host = %host, port = %port, "system proxy enabled")
                }
                Err(e) => {
                    tracing::warn!(service = %service, error = %e, "cannot enable system proxy for service");
                    failures.push(format!("{service}: {e}"));
                }
            }
        }
        if !services.is_empty() && failures.len() == services.len() {
            return Err(DomainError::InfrastructureError(failures.join("; ")));
        }
        Ok(())
    }

    async fn disable(&self) -> Result<(), DomainError> {
        for service in self.list_services().await? {
            for args in [
                str_vec(&["-setwebproxystate", &service, "off"]),
                str_vec(&["-setsecurewebproxystate", &service, "off"]),
            ] {
                run_networksetup(&self.binary, &args).await?;
            }
            tracing::info!(service = %service, "system proxy disabled");
        }
        Ok(())
    }

    /// Статус = ЭФФЕКТИВНЫЕ прокси (`scutil --proxy`, то, что применяют
    /// приложения) + что прописано в каждом сервисе. Раньше «on» значило
    /// «включён хоть в одном сервисе» — UI показывал on, когда активный
    /// сервис (VPN, другой интерфейс) шёл мимо прокси.
    async fn status(&self) -> Result<SystemProxyStatusDto, DomainError> {
        let effective = parse_scutil_proxy(&run_command("scutil", &str_vec(&["--proxy"])).await?);
        let mut configured_servers = Vec::new();
        let mut details = Vec::new();
        for service in self.list_services().await? {
            let mut on = Vec::new();
            for (flag, label) in [("-getwebproxy", "http"), ("-getsecurewebproxy", "https")] {
                let out = run_networksetup(&self.binary, &str_vec(&[flag, &service])).await?;
                if let Some(addr) = parse_proxy_state(&out).address() {
                    on.push(format!("{label}={addr}"));
                    if !configured_servers.contains(&addr) {
                        configured_servers.push(addr);
                    }
                }
            }
            details.push(if on.is_empty() {
                format!("{service}: off")
            } else {
                format!("{service}: {}", on.join(" "))
            });
        }
        Ok(SystemProxyStatusDto {
            enabled: effective.http.is_some() && effective.https.is_some(),
            server: effective.http,
            secure_server: effective.https,
            configured_servers,
            bypass: Vec::new(),
            details: Some(details.join(", ")),
        })
    }
}

impl NetworksetupSystemProxyService {
    /// Включённые (не `*`) сетевые сервисы: Wi-Fi, Ethernet, ...
    async fn list_services(&self) -> Result<Vec<String>, DomainError> {
        let out = run_networksetup(&self.binary, &str_vec(&["-listallnetworkservices"])).await?;
        Ok(parse_service_names(&out))
    }
}

/// `&str`-слайс → `Vec<String>` (аргументы networksetup).
fn str_vec(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// Аргументы `-setproxybypassdomains <svc> <dom1> <dom2> ...`.
fn bypass_args(service: &str, bypass: &[String]) -> Vec<String> {
    let mut args = vec!["-setproxybypassdomains".to_string(), service.to_string()];
    args.extend(bypass.iter().cloned());
    args
}

async fn run_networksetup(binary: &str, args: &[String]) -> Result<String, DomainError> {
    run_command(binary, args).await
}

async fn run_command(binary: &str, args: &[String]) -> Result<String, DomainError> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .await
        .map_err(|e| DomainError::InfrastructureError(format!("run {binary}: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(DomainError::InfrastructureError(format!(
            "{binary} {} failed: {stderr}",
            args.join(" ")
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Разбор `-listallnetworkservices`: первая строка — заголовок, сервисы
/// с префиксом `*` отключены — пропускаем.
pub fn parse_service_names(raw: &str) -> Vec<String> {
    raw.lines()
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !l.starts_with('*'))
        .map(str::to_string)
        .collect()
}

/// Разбор вывода `-getwebproxy` / `-getsecurewebproxy`.
pub struct ProxyState {
    pub enabled: bool,
    pub server: Option<String>,
    pub port: Option<u16>,
}

impl ProxyState {
    /// `host:port`, если прокси включён и адрес задан.
    pub fn address(&self) -> Option<String> {
        match (self.enabled, &self.server, self.port) {
            (true, Some(server), Some(port)) => Some(format!("{server}:{port}")),
            _ => None,
        }
    }
}

/// Эффективные прокси из `scutil --proxy` (State:/Network/Global/Proxies).
#[derive(Debug, Default, PartialEq)]
pub struct EffectiveProxies {
    pub http: Option<String>,
    pub https: Option<String>,
}

/// Разбор `scutil --proxy`: `HTTPEnable : 1`, `HTTPProxy : host`,
/// `HTTPPort : 8080` (и HTTPS*). Включённый без адреса — None.
pub fn parse_scutil_proxy(raw: &str) -> EffectiveProxies {
    let value = |key: &str| {
        raw.lines().find_map(|line| {
            let (k, v) = line.split_once(" : ")?;
            (k.trim() == key).then(|| v.trim().to_string())
        })
    };
    let proxy = |prefix: &str| {
        let enabled = value(&format!("{prefix}Enable")).as_deref() == Some("1");
        let host = value(&format!("{prefix}Proxy"))?;
        let port = value(&format!("{prefix}Port"))?;
        enabled.then(|| format!("{host}:{port}"))
    };
    EffectiveProxies {
        http: proxy("HTTP"),
        https: proxy("HTTPS"),
    }
}

pub fn parse_proxy_state(raw: &str) -> ProxyState {
    let mut enabled = false;
    let mut server = None;
    let mut port = None;
    for line in raw.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("Enabled:") {
            enabled = v.trim().eq_ignore_ascii_case("yes");
        } else if let Some(v) = line.strip_prefix("Server:") {
            let v = v.trim();
            if !v.is_empty() {
                server = Some(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("Port:") {
            port = v.trim().parse::<u16>().ok();
        }
    }
    ProxyState {
        enabled,
        server,
        port,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_names_skipping_header_and_disabled() {
        let raw = "An asterisk (*) denotes that a network service is disabled.\nWi-Fi\nThunderbolt Ethernet\n*Bluetooth PAN\n";
        assert_eq!(
            parse_service_names(raw),
            vec!["Wi-Fi".to_string(), "Thunderbolt Ethernet".to_string()]
        );
    }

    #[test]
    fn parses_proxy_state_on() {
        let raw = "Enabled: Yes\nServer: 127.0.0.1\nPort: 8080\nAuthenticated Proxy Enabled: 0\n";
        let st = parse_proxy_state(raw);
        assert!(st.enabled);
        assert_eq!(st.server.as_deref(), Some("127.0.0.1"));
        assert_eq!(st.port, Some(8080));
    }

    #[test]
    fn parses_proxy_state_off() {
        let raw = "Enabled: No\nServer: \nPort: 0\n";
        let st = parse_proxy_state(raw);
        assert!(!st.enabled);
        assert_eq!(st.server, None);
    }

    const SCUTIL_ON: &str = "<dictionary> {
  ExceptionsList : <array> {
    0 : 127.0.0.1
  }
  FTPPassive : 1
  HTTPEnable : 1
  HTTPPort : 8080
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 1
  HTTPSPort : 8080
  HTTPSProxy : 127.0.0.1
}
";

    #[test]
    fn parses_effective_proxies_from_scutil() {
        let eff = parse_scutil_proxy(SCUTIL_ON);
        assert_eq!(eff.http.as_deref(), Some("127.0.0.1:8080"));
        assert_eq!(eff.https.as_deref(), Some("127.0.0.1:8080"));
    }

    #[test]
    fn scutil_disabled_or_partial_proxies_are_none() {
        let off = "<dictionary> {\n  HTTPEnable : 0\n  HTTPSEnable : 0\n}\n";
        let eff = parse_scutil_proxy(off);
        assert_eq!((eff.http, eff.https), (None, None));

        // Включён, но без адреса — не считается.
        let partial = "<dictionary> {\n  HTTPEnable : 1\n  HTTPSEnable : 1\n  HTTPSProxy : 10.0.0.1\n  HTTPSPort : 3128\n}\n";
        let eff = parse_scutil_proxy(partial);
        assert_eq!(eff.http, None);
        assert_eq!(eff.https.as_deref(), Some("10.0.0.1:3128"));
    }

    #[test]
    fn bypass_args_shape() {
        let args = bypass_args("Wi-Fi", &["localhost".into(), "*.local".into()]);
        assert_eq!(
            args,
            vec![
                "-setproxybypassdomains".to_string(),
                "Wi-Fi".to_string(),
                "localhost".to_string(),
                "*.local".to_string(),
            ]
        );
    }
}
