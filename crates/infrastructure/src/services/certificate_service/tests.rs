//! Тесты RcgenCertificateService: persist/reload CA, corrupt-политика,
//! SAN-политика (apex/поддомен/IDN/IP), LRU листьев, команды доверия.

use super::*;

fn temp_service() -> (tempfile::TempDir, RcgenCertificateService) {
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = RcgenCertificateService::new(dir.path().to_path_buf());
    (dir, svc)
}

#[tokio::test]
async fn ca_generate_persist_reload_same_fingerprint() {
    let (dir, svc) = temp_service();
    let first = svc.ensure_ca().await.expect("ensure #1");
    assert!(first.exists);
    let fp = first.fingerprint.expect("fingerprint present");
    assert_eq!(fp.len(), 64, "sha256 hex");
    assert!(dir.path().join("ca.pem").exists());
    assert!(dir.path().join("ca.key.pem").exists());
    assert!(dir.path().join("ca-meta.json").exists());

    // «Перезапуск»: новый сервис на том же каталоге — fingerprint тот же.
    let svc2 = RcgenCertificateService::new(dir.path().to_path_buf());
    let second = svc2.ensure_ca().await.expect("ensure #2");
    assert_eq!(second.fingerprint.as_deref(), Some(fp.as_str()));
    // Ключи не регенерируются: mtime-«created_at» сохраняется с точностью до записи.
    assert_eq!(second.created_at, first.created_at);
}

#[tokio::test]
async fn corrupt_ca_is_renamed_and_regenerated() {
    let (dir, svc) = temp_service();
    let first = svc.ensure_ca().await.expect("ensure");
    // Порча сертификата (ключ остаётся валидным — ловим именно parse).
    std::fs::write(dir.path().join("ca.pem"), b"not a pem at all").expect("corrupt");

    let svc2 = RcgenCertificateService::new(dir.path().to_path_buf());
    let second = svc2.ensure_ca().await.expect("regenerated");
    assert_ne!(
        second.fingerprint, first.fingerprint,
        "CA must be regenerated after corruption"
    );
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftovers.iter().any(|n| n.starts_with("ca.pem.corrupt-")),
        "corrupt file renamed aside: {leftovers:?}"
    );
}

#[tokio::test]
async fn mismatched_ca_pair_is_regenerated() {
    // Валидные, но НЕсогласованные ca.pem и ca.key.pem (ключ от другого CA):
    // sanity-подписание ловит рассинхрон → corrupt-политика → регенерация.
    let (other_dir, other_svc) = temp_service();
    other_svc.ensure_ca().await.expect("other CA");
    let (own_dir, own_svc) = temp_service();
    own_svc.ensure_ca().await.expect("own CA");
    std::fs::copy(
        other_dir.path().join("ca.key.pem"),
        own_dir.path().join("ca.key.pem"),
    )
    .expect("swap key");

    let svc = RcgenCertificateService::new(own_dir.path().to_path_buf());
    let status = svc.ensure_ca().await.expect("regenerated after mismatch");
    assert!(status.exists);
    // Лист подписывается НОВЫМ (согласованным) CA.
    svc.issue_leaf("example.com", &[])
        .await
        .expect("leaf after regen");
}

#[test]
fn san_policy_exact_host_only() {
    // Лист выпускается на каждый хост (кэш — по точному имени), wildcard
    // ничего не даёт, а для `bbc.co.uk` дал бы `*.co.uk` (публичный суффикс).
    assert_eq!(
        dns_names(&sans_for_host("example.com")),
        vec!["example.com"]
    );
    assert_eq!(
        dns_names(&sans_for_host("api.example.com")),
        vec!["api.example.com"]
    );
    assert_eq!(dns_names(&sans_for_host("bbc.co.uk")), vec!["bbc.co.uk"]);
}

#[test]
fn san_policy_ip_address_no_wildcard() {
    let sans = sans_for_host("127.0.0.1");
    assert!(matches!(sans.as_slice(), [SanType::IpAddress(_)]));
}

#[test]
fn normalize_host_punycodes_idn() {
    assert_eq!(normalize_host("münchen.de"), "xn--mnchen-3ya.de");
    assert_eq!(
        normalize_host("пример.испытание"),
        "xn--e1afmkfd.xn--80akhbyknj4f"
    );
    assert_eq!(normalize_host("API.Example.COM."), "api.example.com");
    assert_eq!(normalize_host("127.0.0.1"), "127.0.0.1");
}

#[test]
fn idn_leaf_gets_punycode_san() {
    let sans = sans_for_host(&normalize_host("münchen.de"));
    assert_eq!(dns_names(&sans), vec!["xn--mnchen-3ya.de"]);
}

#[tokio::test]
async fn leaf_cache_hits_within_same_ca() {
    let (_dir, svc) = temp_service();
    svc.ensure_ca().await.expect("CA");
    let a = svc
        .issue_leaf("cached.example.com", &[])
        .await
        .expect("leaf 1");
    let b = svc
        .issue_leaf("cached.example.com", &[])
        .await
        .expect("leaf 2");
    assert_eq!(a, b, "LRU must serve identical PEM DTO");
    assert_eq!(svc.issued_leaf_count(), 1, "only one real signing");

    // Другой хост — реальная подпись.
    svc.issue_leaf("other.example.org", &[])
        .await
        .expect("leaf 3");
    assert_eq!(svc.issued_leaf_count(), 2);
}

#[tokio::test]
async fn regenerate_invalidates_leaf_cache() {
    let (_dir, svc) = temp_service();
    svc.ensure_ca().await.expect("CA");
    let before = svc.issue_leaf("x.example.com", &[]).await.expect("leaf");
    let new_status = svc.regenerate_ca().await.expect("regen");
    let after = svc.issue_leaf("x.example.com", &[]).await.expect("leaf 2");
    assert_ne!(
        before.cert_pem, after.cert_pem,
        "new CA must re-sign leaves"
    );
    assert_eq!(svc.issued_leaf_count(), 2);
    assert!(new_status.fingerprint.is_some());
}

#[tokio::test]
async fn leaf_is_signed_by_our_ca() {
    let (_dir, svc) = temp_service();
    svc.ensure_ca().await.expect("CA");
    let leaf = svc
        .issue_leaf("signed.example.com", &[])
        .await
        .expect("leaf");
    // Парсим оба PEM и сверяем issuer DN листа с subject DN CA.
    let leaf_der = pem_certificate_der(&leaf.cert_pem).expect("leaf der");
    let ca_der = pem_certificate_der(&std::fs::read_to_string(svc.ca_cert_path()).expect("ca.pem"))
        .expect("ca der");
    let leaf_cert = x509_parser::parse_x509_certificate(&leaf_der).expect("leaf parse");
    let ca_cert = x509_parser::parse_x509_certificate(&ca_der).expect("ca parse");
    assert_eq!(
        leaf_cert
            .1
            .issuer()
            .to_string()
            .replace("CN=", "")
            .replace(", ", ""),
        ca_cert
            .1
            .subject()
            .to_string()
            .replace("CN=", "")
            .replace(", ", ""),
        "leaf issuer must be the CA subject"
    );
    // SAN на месте.
    let sans = leaf_cert
        .1
        .subject_alternative_name()
        .expect("san ext")
        .expect("san present")
        .value
        .general_names
        .iter()
        .filter_map(|g| match g {
            x509_parser::extensions::GeneralName::DNSName(s) => Some(s.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    // Только точное имя (см. sans_for_host).
    assert_eq!(sans, vec!["signed.example.com"]);
}

#[test]
fn install_trust_command_construction() {
    let argv = build_install_trust_command(Path::new("/tmp/ca.pem"));
    assert_eq!(
        argv,
        vec![
            "/usr/bin/security",
            "add-trusted-cert",
            "-d",
            "-r",
            "trustRoot",
            "-p",
            "ssl",
            "-k",
            "/Library/Keychains/System.keychain",
            "/tmp/ca.pem",
        ]
    );
}

#[test]
fn osascript_line_escapes_and_quotes() {
    let argv = build_install_trust_command(Path::new("/tmp/my certs/ca.pem"));
    let line = osascript_line(&argv);
    assert!(line.starts_with("do shell script \""));
    assert!(line.ends_with("\" with administrator privileges"));
    // Путь с пробелом квотится одинарными кавычками внутри shell-строки.
    assert!(line.contains("'/tmp/my certs/ca.pem'"), "{line}");
    // Путь с двойной кавычкой экранируется для AppleScript-строки.
    let argv2 = vec!["echo".to_string(), "a\"b".to_string()];
    let line2 = osascript_line(&argv2);
    assert!(line2.contains("\\\""), "{line2}");
}

/// DNS-SAN из списка (для компактных ассертов).
fn dns_names(sans: &[SanType]) -> Vec<&str> {
    sans.iter()
        .map(|s| match s {
            SanType::DnsName(name) => name.as_str(),
            other => panic!("expected dns name, got {other:?}"),
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_ensure_ca_yields_single_ca() {
    // Первый запуск: экран сертификатов, движок и main зовут ensure_ca
    // одновременно — должен получиться ОДИН CA (память == диск).
    let dir = tempfile::tempdir().expect("tempdir");
    let svc = std::sync::Arc::new(RcgenCertificateService::new(dir.path().to_path_buf()));
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let svc = svc.clone();
            tokio::spawn(async move { svc.ensure_ca().await.expect("ensure").fingerprint })
        })
        .collect();
    let mut fps = Vec::new();
    for t in tasks {
        fps.push(t.await.expect("join").expect("fingerprint"));
    }
    assert!(fps.windows(2).all(|w| w[0] == w[1]), "single CA: {fps:?}");
    let on_disk = RcgenCertificateService::new(dir.path().to_path_buf())
        .ensure_ca()
        .await
        .expect("reload")
        .fingerprint
        .expect("fp");
    assert_eq!(on_disk, fps[0], "memory and disk agree");
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_ca_is_error_not_regenerated() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, svc) = temp_service();
    let first = svc.ensure_ca().await.expect("ensure");
    let cert = dir.path().join("ca.pem");
    std::fs::set_permissions(&cert, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    let svc2 = RcgenCertificateService::new(dir.path().to_path_buf());
    let result = svc2.ensure_ca().await;
    std::fs::set_permissions(&cert, std::fs::Permissions::from_mode(0o600)).expect("restore");

    assert!(result.is_err(), "IO error must surface, not regenerate");
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names.iter().any(|n| n.contains(".corrupt-")),
        "readable-but-denied CA must not be moved aside: {names:?}"
    );
    let again = svc2.ensure_ca().await.expect("readable again");
    assert_eq!(again.fingerprint, first.fingerprint, "same CA kept");
}

#[cfg(unix)]
#[tokio::test]
async fn ca_files_have_restrictive_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, svc) = temp_service();
    svc.ensure_ca().await.expect("ensure");
    let mode = |p: &str| {
        std::fs::metadata(dir.path().join(p))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("ca.key.pem"), 0o600);
    assert_eq!(mode("ca.pem"), 0o600);
}

#[tokio::test]
async fn failed_regenerate_keeps_old_ca_in_memory_and_on_disk() {
    let (dir, svc) = temp_service();
    let old = svc.ensure_ca().await.expect("ensure").fingerprint;
    // Каталог на месте tmp-файла ключа: запись нового ключа упадёт.
    std::fs::create_dir(dir.path().join("ca.key.tmp")).expect("block tmp");

    assert!(svc.regenerate_ca().await.is_err(), "regenerate must fail");
    std::fs::remove_dir(dir.path().join("ca.key.tmp")).expect("unblock");

    assert_eq!(svc.ensure_ca().await.expect("memory").fingerprint, old);
    let reloaded = RcgenCertificateService::new(dir.path().to_path_buf())
        .ensure_ca()
        .await
        .expect("disk")
        .fingerprint;
    assert_eq!(reloaded, old, "old CA restored on disk, trust not broken");
}

/// Имя CA различает выпуски (на телефоне видно, какой из «crabtrap CA»
/// актуален): дата и время создания в CN, и ca-meta.json хранит его же.
#[tokio::test]
async fn new_ca_name_carries_creation_time() {
    let (dir, svc) = temp_service();
    svc.ensure_ca().await.expect("ensure");
    let pem = std::fs::read_to_string(dir.path().join("ca.pem")).expect("ca.pem");
    let name = ca_subject(&pem);
    let today = Utc::now().format("%Y-%m-%d").to_string();
    assert!(name.starts_with("crabtrap CA "), "{name}");
    assert!(name.contains(&today), "{name}");
    let meta = std::fs::read_to_string(dir.path().join("ca-meta.json")).expect("meta");
    assert!(meta.contains(&name), "{meta}");
}

#[test]
fn ca_name_formats_creation_minute() {
    let created = chrono::DateTime::parse_from_rfc3339("2026-10-18T08:40:59Z")
        .expect("time")
        .with_timezone(&Utc);
    assert_eq!(ca_common_name(created), "crabtrap CA 2026-10-18 08:40");
}
