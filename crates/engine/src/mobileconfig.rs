//! iOS-профиль настройки устройства (`.mobileconfig`): корневой CA crabtrap
//! и Wi-Fi текущей сети с ручным прокси на этот Mac — одна установка вместо
//! «скачать CA + вписать прокси руками»; удаление профиля снимает и то, и
//! другое. Доверие к CA iOS всё равно включается руками (Certificate Trust
//! Settings): без MDM это не автоматизируется.

use base64::Engine as _;

/// MIME, по которому Safari на iOS предлагает установить профиль.
pub(crate) const MOBILECONFIG_MIME: &str = "application/x-apple-aspen-config";

/// Один идентификатор на все установки: новый профиль заменяет прежний.
const PROFILE_ID: &str = "dev.crabtrap.device-setup";

/// Что положить в профиль.
pub(crate) struct DeviceProfile<'a> {
    /// Корневой CA (DER).
    pub ca_der: &'a [u8],
    /// SHA-256 отпечаток CA (hex, любые разделители): из него стабильные UUID.
    pub ca_fingerprint: &'a str,
    /// Адрес прокси, как его видит устройство (Host страницы `/ssl`).
    pub proxy_host: &'a str,
    pub proxy_port: u16,
    /// Сеть, для которой прописать прокси; None — профиль только с CA.
    pub ssid: Option<&'a str>,
}

/// Payload-UUID, стабильный для одного CA (`salt` различает payload'ы).
fn payload_uuid(fingerprint: &str, salt: u8) -> String {
    let hex: Vec<u8> = fingerprint
        .chars()
        .filter_map(|c| c.to_digit(16))
        .map(|d| d as u8)
        .collect();
    let mut bytes = [0u8; 16];
    for (ix, byte) in bytes.iter_mut().enumerate() {
        let hi = hex.get(ix * 2).copied().unwrap_or(0);
        let lo = hex.get(ix * 2 + 1).copied().unwrap_or(0);
        *byte = hi << 4 | lo;
    }
    bytes[15] ^= salt;
    uuid::Uuid::from_bytes(bytes)
        .hyphenated()
        .to_string()
        .to_uppercase()
}

/// Экранирование текста для XML plist (и HTML-страницы `/ssl`).
pub(crate) fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn ca_payload(profile: &DeviceProfile) -> String {
    let der = base64::engine::general_purpose::STANDARD.encode(profile.ca_der);
    format!(
        r#"<dict>
<key>PayloadCertificateFileName</key><string>crabtrap-ca.cer</string>
<key>PayloadContent</key><data>{der}</data>
<key>PayloadDisplayName</key><string>crabtrap CA</string>
<key>PayloadIdentifier</key><string>{PROFILE_ID}.ca</string>
<key>PayloadType</key><string>com.apple.security.root</string>
<key>PayloadUUID</key><string>{uuid}</string>
<key>PayloadVersion</key><integer>1</integer>
</dict>"#,
        uuid = payload_uuid(profile.ca_fingerprint, 1),
    )
}

fn wifi_payload(profile: &DeviceProfile, ssid: &str) -> String {
    let ssid = xml_escape(ssid);
    format!(
        r#"<dict>
<key>AutoJoin</key><true/>
<key>EncryptionType</key><string>Any</string>
<key>HIDDEN_NETWORK</key><false/>
<key>PayloadDisplayName</key><string>Wi-Fi proxy for {ssid}</string>
<key>PayloadIdentifier</key><string>{PROFILE_ID}.wifi</string>
<key>PayloadType</key><string>com.apple.wifi.managed</string>
<key>PayloadUUID</key><string>{uuid}</string>
<key>PayloadVersion</key><integer>1</integer>
<key>ProxyServer</key><string>{host}</string>
<key>ProxyServerPort</key><integer>{port}</integer>
<key>ProxyType</key><string>Manual</string>
<key>SSID_STR</key><string>{ssid}</string>
</dict>"#,
        uuid = payload_uuid(profile.ca_fingerprint, 2),
        host = xml_escape(profile.proxy_host),
        port = profile.proxy_port,
    )
}

/// Профиль целиком (XML plist).
pub(crate) fn mobileconfig(profile: &DeviceProfile) -> String {
    let wifi = profile
        .ssid
        .map(|ssid| wifi_payload(profile, ssid))
        .unwrap_or_default();
    let description = match profile.ssid {
        Some(ssid) => format!(
            "Installs the crabtrap certificate authority and routes the Wi-Fi network \
             \u{201c}{}\u{201d} through the proxy at {}:{}. Remove the profile to undo both.",
            xml_escape(ssid),
            xml_escape(profile.proxy_host),
            profile.proxy_port
        ),
        None => "Installs the crabtrap certificate authority.".to_string(),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
<key>PayloadContent</key>
<array>
{ca}
{wifi}
</array>
<key>PayloadDescription</key><string>{description}</string>
<key>PayloadDisplayName</key><string>crabtrap</string>
<key>PayloadIdentifier</key><string>{PROFILE_ID}</string>
<key>PayloadOrganization</key><string>crabtrap</string>
<key>PayloadRemovalDisallowed</key><false/>
<key>PayloadType</key><string>Configuration</string>
<key>PayloadUUID</key><string>{uuid}</string>
<key>PayloadVersion</key><integer>1</integer>
</dict>
</plist>
"#,
        ca = ca_payload(profile),
        uuid = payload_uuid(profile.ca_fingerprint, 0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINGERPRINT: &str = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:FF";

    fn profile(ssid: Option<&str>) -> DeviceProfile<'_> {
        DeviceProfile {
            ca_der: &[0x30, 0x82, 0x01],
            ca_fingerprint: FINGERPRINT,
            proxy_host: "192.168.1.118",
            proxy_port: 8080,
            ssid,
        }
    }

    #[test]
    fn profile_carries_ca_and_wifi_proxy() {
        let xml = mobileconfig(&profile(Some("Home")));
        assert!(xml.contains("<string>com.apple.security.root</string>"));
        assert!(
            xml.contains("<data>MIIB</data>"),
            "CA as base64 DER:\n{xml}"
        );
        assert!(xml.contains("<string>com.apple.wifi.managed</string>"));
        assert!(xml.contains("<key>SSID_STR</key><string>Home</string>"));
        assert!(xml.contains("<key>ProxyType</key><string>Manual</string>"));
        assert!(xml.contains("<key>ProxyServer</key><string>192.168.1.118</string>"));
        assert!(xml.contains("<key>ProxyServerPort</key><integer>8080</integer>"));
    }

    #[test]
    fn profile_without_ssid_has_only_the_ca() {
        let xml = mobileconfig(&profile(None));
        assert!(xml.contains("com.apple.security.root"));
        assert!(!xml.contains("com.apple.wifi.managed"));
    }

    #[test]
    fn ssid_is_xml_escaped() {
        let xml = mobileconfig(&profile(Some("Tom & Jerry <5G>")));
        assert!(
            xml.contains("<string>Tom &amp; Jerry &lt;5G&gt;</string>"),
            "{xml}"
        );
    }

    #[test]
    fn payload_uuids_are_stable_and_distinct() {
        let (root, ca, wifi) = (
            payload_uuid(FINGERPRINT, 0),
            payload_uuid(FINGERPRINT, 1),
            payload_uuid(FINGERPRINT, 2),
        );
        assert_eq!(root, "ABCDEF01-2345-6789-ABCD-EF0123456789");
        assert_eq!(root, payload_uuid(FINGERPRINT, 0), "same CA → same profile");
        assert!(root != ca && ca != wifi);
    }
}
