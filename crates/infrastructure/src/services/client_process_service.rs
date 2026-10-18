//! Клиент обмена на macOS: кто держит соединение `127.0.0.1:<port>` →
//! прокси.
//!
//! Один вызов `lsof -nP -iTCP:<proxy> -sTCP:ESTABLISHED -Fpcn` отдаёт все
//! соединения с прокси разом (UI копит порты новых соединений и спрашивает
//! пачкой). Путь к исполняемому файлу — `proc_pidpath` (libproc), имя и
//! иконка — по внешнему `.app`-бандлу: помощники браузеров
//! («Google Chrome Helper.app» внутри «Google Chrome.app») группируются под
//! своим приложением. Иконка — NSWorkspace, отрисованная в 64×64 PNG.

use std::time::Duration;

use application::dto::clients::{ClientAppDto, ClientLookupDto};
use application::services::ClientProcessService;
use domain::errors::DomainError;

/// Где лежит lsof на macOS.
const LSOF: &str = "/usr/sbin/lsof";
/// lsof на загруженной машине может думать долго — не держим UI вечно.
const LSOF_TIMEOUT: Duration = Duration::from_secs(3);
/// Сторона иконки в пикселях (до 32 pt на Retina).
const ICON_PX: f64 = 64.;

/// Сокет из вывода `lsof -F pcn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LsofSocket {
    pub pid: u32,
    pub command: String,
    pub local_port: u16,
    pub remote_port: u16,
}

fn port_of(endpoint: &str) -> Option<u16> {
    endpoint.rsplit_once(':')?.1.parse().ok()
}

/// Разбор `lsof -F pcn`: строки `p<pid>`, `c<command>`, `n<a>-><b>`.
pub fn parse_lsof(output: &str) -> Vec<LsofSocket> {
    let (mut pid, mut command) = (None::<u32>, String::new());
    let mut sockets = Vec::new();
    for line in output.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => {
                pid = value.parse().ok();
                command.clear();
            }
            "c" => command = value.to_string(),
            "n" => {
                let ports = value
                    .split_once("->")
                    .and_then(|(local, remote)| Some((port_of(local)?, port_of(remote)?)));
                if let (Some(pid), Some((local_port, remote_port))) = (pid, ports) {
                    sockets.push(LsofSocket {
                        pid,
                        command: command.clone(),
                        local_port,
                        remote_port,
                    });
                }
            }
            _ => {}
        }
    }
    sockets
}

/// Внешний `.app`-бандл пути: `/Applications/X.app/Contents/…/Y.app/…` →
/// `/Applications/X.app`.
pub fn outermost_app_bundle(path: &str) -> Option<&str> {
    let end = path
        .find(".app/")
        .map(|ix| ix + ".app".len())
        .or_else(|| path.ends_with(".app").then_some(path.len()))?;
    Some(&path[..end])
}

/// Имя приложения: бандл без `.app` или имя процесса.
pub fn display_name(bundle: Option<&str>, command: &str) -> String {
    bundle
        .and_then(|b| b.rsplit('/').next())
        .and_then(|name| name.strip_suffix(".app"))
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| command.to_string())
}

/// Сокеты клиентов: удалённый порт — прокси, локальный — из запроса, не мы.
pub fn client_sockets(
    sockets: Vec<LsofSocket>,
    proxy_port: u16,
    client_ports: &[u16],
    own_pid: u32,
) -> Vec<LsofSocket> {
    sockets
        .into_iter()
        .filter(|s| {
            s.remote_port == proxy_port && client_ports.contains(&s.local_port) && s.pid != own_pid
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn executable_path(pid: u32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: буфер на PROC_PIDPATHINFO_MAXSIZE байт, длину передаём явно;
    // proc_pidpath пишет не больше и возвращает число записанных байт.
    let len = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buf.as_mut_ptr().cast(),
            libc::PROC_PIDPATHINFO_MAXSIZE as u32,
        )
    };
    (len > 0).then(|| String::from_utf8_lossy(&buf[..len as usize]).into_owned())
}

#[cfg(not(target_os = "macos"))]
fn executable_path(_pid: u32) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn icon_png_sync(path: &str) -> Option<Vec<u8>> {
    use objc2::AllocAnyThread as _;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
    use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

    objc2::rc::autoreleasepool(|_| {
        let image = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path));
        let mut rect = NSRect::new(NSPoint::new(0., 0.), NSSize::new(ICON_PX, ICON_PX));
        // SAFETY: указатель на локальный NSRect; контекст и подсказки не нужны.
        let cg = unsafe { image.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
        let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
        // SAFETY: пустой словарь свойств — допустимое значение для PNG.
        let png = unsafe {
            rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
        }?;
        Some(png.to_vec())
    })
}

#[cfg(not(target_os = "macos"))]
fn icon_png_sync(_path: &str) -> Option<Vec<u8>> {
    None
}

/// Реализация порта через lsof + libproc + NSWorkspace.
#[derive(Debug, Default)]
pub struct MacClientProcessService;

impl MacClientProcessService {
    pub fn new() -> Self {
        Self
    }

    async fn lsof(proxy_port: u16) -> Result<String, DomainError> {
        let run = tokio::process::Command::new(LSOF)
            .args([
                "-nP",
                &format!("-iTCP:{proxy_port}"),
                "-sTCP:ESTABLISHED",
                "-Fpcn",
            ])
            .output();
        let output = tokio::time::timeout(LSOF_TIMEOUT, run)
            .await
            .map_err(|_| DomainError::Timeout("lsof timed out".into()))?
            .map_err(|e| DomainError::InfrastructureError(format!("cannot run lsof: {e}")))?;
        // Код 1 без вывода — «ничего не нашлось», не ошибка.
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[async_trait::async_trait]
impl ClientProcessService for MacClientProcessService {
    async fn resolve(
        &self,
        proxy_port: u16,
        client_ports: Vec<u16>,
    ) -> Result<Vec<ClientLookupDto>, DomainError> {
        if client_ports.is_empty() {
            return Ok(Vec::new());
        }
        let output = Self::lsof(proxy_port).await?;
        let sockets = client_sockets(
            parse_lsof(&output),
            proxy_port,
            &client_ports,
            std::process::id(),
        );
        Ok(sockets
            .into_iter()
            .map(|s| {
                let executable = executable_path(s.pid);
                let bundle = executable
                    .as_deref()
                    .and_then(outermost_app_bundle)
                    .map(str::to_string);
                ClientLookupDto {
                    client_port: s.local_port,
                    app: ClientAppDto {
                        pid: s.pid,
                        name: display_name(bundle.as_deref(), &s.command),
                        executable,
                        bundle_path: bundle,
                    },
                }
            })
            .collect())
    }

    async fn icon_png(&self, app: ClientAppDto) -> Result<Option<Vec<u8>>, DomainError> {
        let Some(path) = app.bundle_path.or(app.executable) else {
            return Ok(None);
        };
        tokio::task::spawn_blocking(move || icon_png_sync(&path))
            .await
            .map_err(|e| DomainError::InfrastructureError(format!("icon task failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSOF_OUTPUT: &str = "p501\ncGoogle Chrome Helper\nf23\nn127.0.0.1:52344->127.0.0.1:8080\n\
f24\nn127.0.0.1:52345->127.0.0.1:8080\np777\ncrabtrap\nf10\nn127.0.0.1:8080->127.0.0.1:52344\n\
p900\ncurl\nf5\nn[::1]:60000->[::1]:8080\n";

    #[test]
    fn parses_processes_and_sockets() {
        let sockets = parse_lsof(LSOF_OUTPUT);
        assert_eq!(sockets.len(), 4);
        assert_eq!(
            sockets[0],
            LsofSocket {
                pid: 501,
                command: "Google Chrome Helper".into(),
                local_port: 52344,
                remote_port: 8080,
            }
        );
        assert_eq!(sockets[3].local_port, 60000, "IPv6 endpoints parse too");
    }

    #[test]
    fn keeps_only_requested_client_sides_of_other_processes() {
        let sockets = client_sockets(parse_lsof(LSOF_OUTPUT), 8080, &[52344, 60000], 777);
        let pairs: Vec<_> = sockets.iter().map(|s| (s.pid, s.local_port)).collect();
        assert_eq!(pairs, [(501, 52344), (900, 60000)]);
    }

    #[test]
    fn garbage_lines_are_ignored() {
        assert!(parse_lsof("\nx\nnno-arrow\np\nn1:2->3:4\n").is_empty());
    }

    #[test]
    fn helpers_group_under_their_outer_app() {
        let helper = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper";
        assert_eq!(
            outermost_app_bundle(helper),
            Some("/Applications/Google Chrome.app")
        );
        assert_eq!(outermost_app_bundle("/usr/bin/curl"), None);
        assert_eq!(
            outermost_app_bundle("/Applications/Safari.app"),
            Some("/Applications/Safari.app")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn renders_a_system_app_icon_as_png() {
        let png = icon_png_sync("/System/Applications/Calculator.app")
            .or_else(|| icon_png_sync("/Applications/Safari.app"))
            .expect("icon of a system app");
        assert!(png.starts_with(b"\x89PNG"), "PNG signature");
        assert!(
            png.len() < 64 * 1024,
            "small icon, not the 1024px representation"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn executable_path_of_this_process() {
        let path = executable_path(std::process::id()).expect("own path");
        assert!(path.contains("infrastructure"), "{path}");
    }

    #[test]
    fn display_name_prefers_bundle() {
        assert_eq!(
            display_name(
                Some("/Applications/Google Chrome.app"),
                "Google Chrome Helper"
            ),
            "Google Chrome"
        );
        assert_eq!(display_name(None, "curl"), "curl");
    }
}
