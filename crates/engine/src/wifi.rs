//! Имя Wi-Fi-сети этого Mac (для профиля устройства). `networksetup`, затем
//! `ipconfig getsummary`; новые macOS без доступа к геолокации прячут SSID
//! (`<redacted>` / «not associated») — тогда None, и имя вводят на странице.

use std::time::Duration;

/// Сколько ждём системную утилиту.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
/// Интерфейс Wi-Fi, если `networksetup` его не назвал.
const DEFAULT_WIFI_DEVICE: &str = "en0";

/// SSID текущей сети или None (не Wi-Fi, нет сети, SSID скрыт системой).
pub(crate) async fn current_ssid() -> Option<String> {
    let device = run("networksetup", &["-listallhardwareports"])
        .await
        .and_then(|out| wifi_device(&out))
        .unwrap_or_else(|| DEFAULT_WIFI_DEVICE.to_string());
    if let Some(ssid) = run("networksetup", &["-getairportnetwork", &device])
        .await
        .and_then(|out| parse_airport_network(&out))
    {
        return Some(ssid);
    }
    run("ipconfig", &["getsummary", &device])
        .await
        .and_then(|out| parse_summary_ssid(&out))
}

async fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new(program).args(args).output();
    match tokio::time::timeout(COMMAND_TIMEOUT, output).await {
        Ok(Ok(out)) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        }
        Ok(Ok(_)) => None,
        Ok(Err(e)) => {
            tracing::debug!(program, error = %e, "cannot run");
            None
        }
        Err(_) => {
            tracing::debug!(program, "timed out");
            None
        }
    }
}

/// Устройство Wi-Fi из `networksetup -listallhardwareports`.
fn wifi_device(ports: &str) -> Option<String> {
    let mut lines = ports.lines().map(str::trim);
    lines.find(|line| *line == "Hardware Port: Wi-Fi")?;
    lines
        .next()
        .and_then(|line| line.strip_prefix("Device: "))
        .map(|device| device.trim().to_string())
        .filter(|device| !device.is_empty())
}

/// `Current Wi-Fi Network: Home` → `Home`.
fn parse_airport_network(out: &str) -> Option<String> {
    out.lines()
        .find_map(|line| line.trim().strip_prefix("Current Wi-Fi Network:"))
        .map(|ssid| ssid.trim().to_string())
        .filter(|ssid| !ssid.is_empty())
}

/// Строка `  SSID : Home` из `ipconfig getsummary` (BSSID не путаем;
/// скрытый системой `<redacted>` — нет имени).
fn parse_summary_ssid(out: &str) -> Option<String> {
    out.lines()
        .find_map(|line| line.trim().strip_prefix("SSID :"))
        .map(|ssid| ssid.trim().to_string())
        .filter(|ssid| !ssid.is_empty() && ssid != "<redacted>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_wifi_device() {
        let ports = "\nHardware Port: Ethernet\nDevice: en1\n\nHardware Port: Wi-Fi\nDevice: en0\nEthernet Address: aa\n";
        assert_eq!(wifi_device(ports).as_deref(), Some("en0"));
        assert_eq!(wifi_device("Hardware Port: Ethernet\nDevice: en1\n"), None);
    }

    #[test]
    fn parses_networksetup_ssid() {
        assert_eq!(
            parse_airport_network("Current Wi-Fi Network: Keenetic-8606\n").as_deref(),
            Some("Keenetic-8606")
        );
        assert_eq!(
            parse_airport_network("You are not associated with an AirPort network.\n"),
            None
        );
    }

    #[test]
    fn parses_ipconfig_ssid_but_not_bssid_or_redacted() {
        let summary = "  BSSID : aa:bb\n  SSID : Home 5G\n";
        assert_eq!(parse_summary_ssid(summary).as_deref(), Some("Home 5G"));
        assert_eq!(parse_summary_ssid("  BSSID : aa:bb\n"), None);
        assert_eq!(parse_summary_ssid("  SSID : <redacted>\n"), None);
    }
}
