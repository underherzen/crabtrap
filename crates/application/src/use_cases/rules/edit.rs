//! Редактирование RuleSet для UI (Tools → Block List / Map Local /
//! Map Remote / Throttle). Чистые функции: принимают снапшот, возвращают
//! новый — без мутаций, сохранение отдельно (UpdateRuleSetUseCase).
//!
//! «Location» — как в Charles: одна строка вместо формы из полей:
//! `api.example.com`, `*.ads.com`, `https://api.example.com/v1/*`.
//! Схема и порт отбрасываются (TrafficMatch их не знает), path `/*` или `*`
//! — то же, что без path.

use std::path::PathBuf;

use domain::DomainError;
use domain::entities::rule::{
    AllowEntry, BlockAction, BlockRule, BreakPhase, BreakpointRule, DEFAULT_MAP_LOCAL_STATUS,
    DnsSpoofEntry, MapLocalRule, MapRemoteRule, RemotePathMode, ReverseProxyEntry, RewriteLocation,
    RewriteOp, RewriteRule, RuleSet, SslProxyEntry, ThrottleProfile,
};
use domain::entities::script_rule::ScriptRule;
use domain::value_objects::http_vo::HeaderEntry;
use domain::value_objects::{RuleId, TrafficMatch};

/// Разобрать location в условие.
pub fn parse_location(input: &str) -> Result<TrafficMatch, DomainError> {
    let trimmed = input.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let (authority, path) = match without_scheme.find('/') {
        Some(ix) => (&without_scheme[..ix], &without_scheme[ix..]),
        None => (without_scheme, ""),
    };
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
        .map(|(host, _)| host)
        .unwrap_or(authority);
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(DomainError::ValidationError(format!(
            "location '{trimmed}': host is required (e.g. api.example.com/v1/*)"
        )));
    }
    let path_glob = match path {
        "" | "/" | "/*" | "*" => None,
        other => Some(other.to_string()),
    };
    Ok(TrafficMatch {
        host_glob: Some(host.to_ascii_lowercase()),
        path_glob,
        ..Default::default()
    })
}

/// Location для отображения в списке правил.
pub fn format_location(m: &TrafficMatch) -> String {
    let location = match &m.url_regex {
        Some(regex) => format!("URL ~ {regex}"),
        None => {
            let host = m.host_glob.as_deref().unwrap_or("*");
            let path = m.path_glob.as_deref().unwrap_or("/*");
            let query = match m.ignore_query && !path.ends_with('*') {
                true => " (any query)",
                false => "",
            };
            format!("{host}{path}{query}")
        }
    };
    let method = m.method.as_ref().map(|method| format!("{method} "));
    let graphql = m
        .graphql_operation
        .as_ref()
        .map(|op| format!(" · GraphQL {op}"));
    format!(
        "{}{location}{}",
        method.unwrap_or_default(),
        graphql.unwrap_or_default()
    )
}

/// Вставить новое правило (`id = None`) в конец или заменить существующее
/// на месте. `make` получает старое правило (при правке), чтобы сохранить
/// его `enabled`.
pub(crate) fn upsert<T>(
    list: Vec<T>,
    id: Option<RuleId>,
    id_of: impl Fn(&T) -> RuleId,
    make: impl FnOnce(Option<&T>) -> T,
) -> Vec<T> {
    let position = id.and_then(|id| list.iter().position(|r| id_of(r) == id));
    match position {
        Some(pos) => {
            let mut replacement = Some(make(Some(&list[pos])));
            list.into_iter()
                .enumerate()
                .map(|(ix, rule)| match replacement.take_if(|_| ix == pos) {
                    Some(updated) => updated,
                    None => rule,
                })
                .collect()
        }
        None => list.into_iter().chain([make(None)]).collect(),
    }
}

pub fn upsert_block(
    rules: RuleSet,
    id: Option<RuleId>,
    location: TrafficMatch,
    action: BlockAction,
) -> RuleSet {
    RuleSet {
        blocks: upsert(
            rules.blocks,
            id,
            |r| r.id,
            |old| BlockRule {
                id: old.map(|o| o.id).unwrap_or_else(RuleId::new),
                r#match: location,
                action,
                enabled: old.is_none_or(|o| o.enabled),
            },
        ),
        ..rules
    }
}

/// Ответ Map Local помимо тела из файла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapLocalResponse {
    pub status: u16,
    /// `None` — Content-Type по расширению файла.
    pub content_type: Option<String>,
    /// Дополнительные заголовки ответа, по порядку.
    pub headers: Vec<HeaderEntry>,
}

impl Default for MapLocalResponse {
    fn default() -> Self {
        Self {
            status: DEFAULT_MAP_LOCAL_STATUS,
            content_type: None,
            headers: Vec::new(),
        }
    }
}

pub fn upsert_map_local(
    rules: RuleSet,
    id: Option<RuleId>,
    location: TrafficMatch,
    local_path: PathBuf,
    response: MapLocalResponse,
) -> RuleSet {
    RuleSet {
        map_local: upsert(
            rules.map_local,
            id,
            |r| r.id,
            |old| MapLocalRule {
                id: old.map(|o| o.id).unwrap_or_else(RuleId::new),
                r#match: location,
                local_path,
                status: response.status,
                content_type: response.content_type,
                headers: response.headers,
                enabled: old.is_none_or(|o| o.enabled),
            },
        ),
        ..rules
    }
}

pub fn upsert_map_remote(
    rules: RuleSet,
    id: Option<RuleId>,
    location: TrafficMatch,
    remote: String,
    preserve_host: bool,
    path_mode: RemotePathMode,
) -> RuleSet {
    RuleSet {
        map_remote: upsert(
            rules.map_remote,
            id,
            |r| r.id,
            |old| MapRemoteRule {
                id: old.map(|o| o.id).unwrap_or_else(RuleId::new),
                r#match: location,
                remote,
                preserve_host,
                path_mode,
                enabled: old.is_none_or(|o| o.enabled),
            },
        ),
        ..rules
    }
}

/// Брейкпоинт: новый (`id = None`) или правка (с сохранением `enabled`).
pub fn upsert_breakpoint(
    rules: RuleSet,
    id: Option<RuleId>,
    location: TrafficMatch,
    phase: BreakPhase,
) -> RuleSet {
    RuleSet {
        breakpoints: upsert(
            rules.breakpoints,
            id,
            |r| r.id,
            |old| BreakpointRule {
                id: old.map(|o| o.id).unwrap_or_else(RuleId::new),
                r#match: location,
                phase,
                enabled: old.is_none_or(|o| o.enabled),
            },
        ),
        ..rules
    }
}

/// Rewrite-правило из диалога: новое (`id = None`) или правка (с
/// сохранением `enabled`). `location = None` — все запросы/ответы.
pub fn upsert_rewrite(
    rules: RuleSet,
    id: Option<RuleId>,
    name: String,
    location: RewriteLocation,
    r#match: Option<TrafficMatch>,
    ops: Vec<RewriteOp>,
) -> RuleSet {
    RuleSet {
        rewrite: upsert(
            rules.rewrite,
            id,
            |r| r.id,
            |old| RewriteRule {
                id: old.map(|o| o.id).unwrap_or_else(RuleId::new),
                name: name.trim().to_string(),
                enabled: old.is_none_or(|o| o.enabled),
                location,
                r#match,
                ops,
            },
        ),
        ..rules
    }
}

/// Хост SSL Proxying: новый или правка существующего; `exclude` — «не
/// расшифровывать» (перекрывает include-записи).
pub fn upsert_ssl_host(
    rules: RuleSet,
    id: Option<RuleId>,
    location: &str,
    exclude: bool,
) -> Result<RuleSet, DomainError> {
    let host = parse_location(location)?
        .host_glob
        .unwrap_or_else(|| "*".to_string());
    Ok(RuleSet {
        ssl_proxy: upsert(
            rules.ssl_proxy,
            id,
            |r| r.id,
            |old| SslProxyEntry {
                id: old.map(|o| o.id).unwrap_or_else(RuleId::new),
                host_pattern: host,
                enabled: old.is_none_or(|o| o.enabled),
                exclude,
            },
        ),
        ..rules
    })
}

/// Хост Allow List: новый или правка существующего.
pub fn upsert_allow_host(
    rules: RuleSet,
    id: Option<RuleId>,
    location: &str,
) -> Result<RuleSet, DomainError> {
    let host = location_host(location)?;
    Ok(RuleSet {
        allow_list: upsert(
            rules.allow_list,
            id,
            |r| r.id,
            |old| AllowEntry {
                id: old.map(|o| o.id).unwrap_or_else(RuleId::new),
                host_pattern: host,
                enabled: old.is_none_or(|o| o.enabled),
            },
        ),
        ..rules
    })
}

/// «Allow only <host>» (правый клик в трафике): хост — в Allow List (есть —
/// включается, без дублей), список — включён.
pub fn with_host_allowed(rules: RuleSet, location: &str) -> Result<RuleSet, DomainError> {
    let host = location_host(location)?;
    let exists = rules.allow_list.iter().any(|e| e.host_pattern == host);
    let allow_list = rules
        .allow_list
        .into_iter()
        .map(|e| AllowEntry {
            enabled: e.enabled || e.host_pattern == host,
            ..e
        })
        .chain((!exists).then(|| AllowEntry::new(host.clone())))
        .collect();
    Ok(RuleSet {
        allow_enabled: true,
        allow_list,
        ..rules
    })
}

/// Запись DNS Spoofing: новая или правка существующей. `ip` — IPv4 или
/// IPv6 (`127.0.0.1`, `::1`, `[::1]`).
pub fn upsert_dns_spoof(
    rules: RuleSet,
    id: Option<RuleId>,
    location: &str,
    ip: &str,
) -> Result<RuleSet, DomainError> {
    let host = location_host(location)?;
    let ip = parse_ip(ip)?;
    Ok(RuleSet {
        dns_spoof: upsert(
            rules.dns_spoof,
            id,
            |r| r.id,
            |old| DnsSpoofEntry {
                id: old.map(|o| o.id).unwrap_or_else(RuleId::new),
                host_pattern: host,
                ip,
                enabled: old.is_none_or(|o| o.enabled),
            },
        ),
        ..rules
    })
}

/// Запись Reverse Proxy: новая или правка существующей. `target` — origin
/// без пути (проверяет `UpdateRuleSetUseCase`).
pub fn upsert_reverse_proxy(
    rules: RuleSet,
    id: Option<RuleId>,
    listen_port: u16,
    target: String,
    preserve_host: bool,
) -> RuleSet {
    RuleSet {
        reverse_proxies: upsert(
            rules.reverse_proxies,
            id,
            |r| r.id,
            |old| ReverseProxyEntry {
                id: old.map(|o| o.id).unwrap_or_else(RuleId::new),
                listen_port,
                target,
                preserve_host,
                enabled: old.is_none_or(|o| o.enabled),
            },
        ),
        ..rules
    }
}

/// IP-адрес из поля формы (скобки IPv6 допускаются).
pub fn parse_ip(input: &str) -> Result<std::net::IpAddr, DomainError> {
    let trimmed = input.trim();
    let bare = trimmed
        .strip_prefix('[')
        .and_then(|ip| ip.strip_suffix(']'))
        .unwrap_or(trimmed);
    bare.parse().map_err(|_| {
        DomainError::ValidationError(format!(
            "'{trimmed}' is not an IP address (e.g. 127.0.0.1 or ::1)"
        ))
    })
}

/// Хост из локации (`https://API.com:8443/v1` → `api.com`; пусто — `*`).
fn location_host(location: &str) -> Result<String, DomainError> {
    Ok(parse_location(location)?
        .host_glob
        .unwrap_or_else(|| "*".to_string()))
}

/// Копия правила (новый id) сразу после оригинала.
pub fn with_rule_duplicated(rules: RuleSet, id: RuleId) -> RuleSet {
    fn dup<T: Clone>(
        list: Vec<T>,
        id: RuleId,
        id_of: impl Fn(&T) -> RuleId,
        renew: impl Fn(T) -> T,
    ) -> Vec<T> {
        list.into_iter()
            .flat_map(|r| {
                let copy = (id_of(&r) == id).then(|| renew(r.clone()));
                std::iter::once(r).chain(copy)
            })
            .collect()
    }
    RuleSet {
        ssl_proxy: dup(
            rules.ssl_proxy,
            id,
            |r| r.id,
            |r| SslProxyEntry {
                id: RuleId::new(),
                ..r
            },
        ),
        blocks: dup(
            rules.blocks,
            id,
            |r| r.id,
            |r| BlockRule {
                id: RuleId::new(),
                ..r
            },
        ),
        map_local: dup(
            rules.map_local,
            id,
            |r| r.id,
            |r| MapLocalRule {
                id: RuleId::new(),
                ..r
            },
        ),
        map_remote: dup(
            rules.map_remote,
            id,
            |r| r.id,
            |r| MapRemoteRule {
                id: RuleId::new(),
                ..r
            },
        ),
        breakpoints: dup(
            rules.breakpoints,
            id,
            |r| r.id,
            |r| BreakpointRule {
                id: RuleId::new(),
                ..r
            },
        ),
        rewrite: dup(
            rules.rewrite,
            id,
            |r| r.id,
            |r| RewriteRule {
                id: RuleId::new(),
                name: format!("{} copy", r.name),
                ..r
            },
        ),
        scripts: dup(
            rules.scripts,
            id,
            |r| r.id,
            |r| ScriptRule {
                id: RuleId::new(),
                name: format!("{} copy", r.name),
                ..r
            },
        ),
        allow_list: dup(
            rules.allow_list,
            id,
            |r| r.id,
            |r| AllowEntry {
                id: RuleId::new(),
                ..r
            },
        ),
        dns_spoof: dup(
            rules.dns_spoof,
            id,
            |r| r.id,
            |r| DnsSpoofEntry {
                id: RuleId::new(),
                ..r
            },
        ),
        // Копия занимала бы тот же порт — создаётся выключенной.
        reverse_proxies: dup(
            rules.reverse_proxies,
            id,
            |r| r.id,
            |r| ReverseProxyEntry {
                id: RuleId::new(),
                enabled: false,
                ..r
            },
        ),
        ..rules
    }
}

/// Инструмент Tools (галочка «Enable …»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    SslProxying,
    AllowList,
    DnsSpoofing,
    ReverseProxy,
    BlockList,
    MapLocal,
    MapRemote,
    Throttle,
    Rewrite,
    Breakpoints,
    Scripting,
}

pub fn tool_enabled(rules: &RuleSet, tool: Tool) -> bool {
    match tool {
        Tool::SslProxying => rules.ssl_proxy_enabled,
        Tool::AllowList => rules.allow_enabled,
        Tool::DnsSpoofing => rules.dns_spoof_enabled,
        Tool::ReverseProxy => rules.reverse_proxy_enabled,
        Tool::BlockList => rules.blocks_enabled,
        Tool::MapLocal => rules.map_local_enabled,
        Tool::MapRemote => rules.map_remote_enabled,
        Tool::Throttle => rules.throttle_enabled,
        Tool::Rewrite => rules.rewrite_enabled,
        Tool::Breakpoints => rules.breakpoints_enabled,
        Tool::Scripting => rules.scripts_enabled,
    }
}

pub fn with_tool_enabled(rules: RuleSet, tool: Tool, enabled: bool) -> RuleSet {
    match tool {
        Tool::SslProxying => rules.with_ssl_proxying(enabled),
        Tool::AllowList => RuleSet {
            allow_enabled: enabled,
            ..rules
        },
        Tool::DnsSpoofing => RuleSet {
            dns_spoof_enabled: enabled,
            ..rules
        },
        Tool::ReverseProxy => RuleSet {
            reverse_proxy_enabled: enabled,
            ..rules
        },
        Tool::BlockList => RuleSet {
            blocks_enabled: enabled,
            ..rules
        },
        Tool::MapLocal => RuleSet {
            map_local_enabled: enabled,
            ..rules
        },
        Tool::MapRemote => RuleSet {
            map_remote_enabled: enabled,
            ..rules
        },
        Tool::Throttle => RuleSet {
            throttle_enabled: enabled,
            ..rules
        },
        Tool::Rewrite => RuleSet {
            rewrite_enabled: enabled,
            ..rules
        },
        Tool::Breakpoints => RuleSet {
            breakpoints_enabled: enabled,
            ..rules
        },
        Tool::Scripting => RuleSet {
            scripts_enabled: enabled,
            ..rules
        },
    }
}

/// Добавить хост в SSL Proxying (`*.example.com`, `api.example.com`, `*`).
/// Схема, порт и path отбрасываются — расшифровка решается на уровне хоста.
pub fn with_ssl_location(rules: RuleSet, location: &str) -> Result<RuleSet, DomainError> {
    let host = parse_location(location)?
        .host_glob
        .unwrap_or_else(|| "*".to_string());
    Ok(RuleSet {
        ssl_proxy: rules
            .ssl_proxy
            .into_iter()
            .chain([SslProxyEntry::include(host)])
            .collect(),
        ..rules
    })
}

/// «Enable SSL Proxying for <host>» (правый клик в трафике): включить
/// расшифровку ЭТОГО хоста. Есть запись — включается она (без дублей; запись-
/// исключение становится расшифровкой); нет — добавляется. Если SSL Proxying был выключен глобально, остальные записи
/// (например, `*`) остаются выключенными — иначе включение глобального флага
/// внезапно расшифровало бы всё.
pub fn with_ssl_host_enabled(rules: RuleSet, location: &str) -> Result<RuleSet, DomainError> {
    let host = parse_location(location)?
        .host_glob
        .unwrap_or_else(|| "*".to_string());
    let was_enabled = rules.ssl_proxy_enabled;
    let exists = rules.ssl_proxy.iter().any(|e| e.host_pattern == host);
    let entries: Vec<SslProxyEntry> = rules
        .ssl_proxy
        .into_iter()
        .map(|e| {
            if e.host_pattern == host {
                return SslProxyEntry {
                    enabled: true,
                    exclude: false,
                    ..e
                };
            }
            SslProxyEntry {
                enabled: was_enabled && e.enabled,
                ..e
            }
        })
        .chain((!exists).then(|| SslProxyEntry::include(host.clone())))
        .collect();
    Ok(RuleSet {
        ssl_proxy_enabled: true,
        ssl_proxy: entries,
        ..rules
    })
}

/// Удалить правило (любой категории) по id.
pub fn without_rule(rules: RuleSet, id: RuleId) -> RuleSet {
    RuleSet {
        ssl_proxy: rules.ssl_proxy.into_iter().filter(|r| r.id != id).collect(),
        blocks: rules.blocks.into_iter().filter(|r| r.id != id).collect(),
        map_local: rules.map_local.into_iter().filter(|r| r.id != id).collect(),
        map_remote: rules
            .map_remote
            .into_iter()
            .filter(|r| r.id != id)
            .collect(),
        rewrite: rules.rewrite.into_iter().filter(|r| r.id != id).collect(),
        breakpoints: rules
            .breakpoints
            .into_iter()
            .filter(|r| r.id != id)
            .collect(),
        scripts: rules.scripts.into_iter().filter(|r| r.id != id).collect(),
        allow_list: rules
            .allow_list
            .into_iter()
            .filter(|r| r.id != id)
            .collect(),
        dns_spoof: rules.dns_spoof.into_iter().filter(|r| r.id != id).collect(),
        reverse_proxies: rules
            .reverse_proxies
            .into_iter()
            .filter(|r| r.id != id)
            .collect(),
        ..rules
    }
}

/// Переключить `enabled` правила (любой категории) по id.
pub fn with_rule_toggled(rules: RuleSet, id: RuleId) -> RuleSet {
    RuleSet {
        ssl_proxy: rules
            .ssl_proxy
            .into_iter()
            .map(|r| {
                if r.id == id {
                    SslProxyEntry {
                        enabled: !r.enabled,
                        ..r
                    }
                } else {
                    r
                }
            })
            .collect(),
        blocks: rules
            .blocks
            .into_iter()
            .map(|r| {
                if r.id == id {
                    BlockRule {
                        enabled: !r.enabled,
                        ..r
                    }
                } else {
                    r
                }
            })
            .collect(),
        map_local: rules
            .map_local
            .into_iter()
            .map(|r| {
                if r.id == id {
                    MapLocalRule {
                        enabled: !r.enabled,
                        ..r
                    }
                } else {
                    r
                }
            })
            .collect(),
        map_remote: rules
            .map_remote
            .into_iter()
            .map(|r| {
                if r.id == id {
                    MapRemoteRule {
                        enabled: !r.enabled,
                        ..r
                    }
                } else {
                    r
                }
            })
            .collect(),
        rewrite: rules
            .rewrite
            .into_iter()
            .map(|r| {
                if r.id == id {
                    RewriteRule {
                        enabled: !r.enabled,
                        ..r
                    }
                } else {
                    r
                }
            })
            .collect(),
        breakpoints: rules
            .breakpoints
            .into_iter()
            .map(|r| {
                if r.id == id {
                    BreakpointRule {
                        enabled: !r.enabled,
                        ..r
                    }
                } else {
                    r
                }
            })
            .collect(),
        scripts: rules
            .scripts
            .into_iter()
            .map(|r| {
                if r.id == id {
                    ScriptRule {
                        enabled: !r.enabled,
                        ..r
                    }
                } else {
                    r
                }
            })
            .collect(),
        allow_list: rules
            .allow_list
            .into_iter()
            .map(|r| AllowEntry {
                enabled: r.enabled != (r.id == id),
                ..r
            })
            .collect(),
        dns_spoof: rules
            .dns_spoof
            .into_iter()
            .map(|r| DnsSpoofEntry {
                enabled: r.enabled != (r.id == id),
                ..r
            })
            .collect(),
        reverse_proxies: rules
            .reverse_proxies
            .into_iter()
            .map(|r| ReverseProxyEntry {
                enabled: r.enabled != (r.id == id),
                ..r
            })
            .collect(),
        ..rules
    }
}

/// Готовый профиль throttle (Charles: Throttle Settings → presets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottlePreset {
    pub name: &'static str,
    pub values: ThrottleValues,
}

/// Значения профиля throttle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleValues {
    /// Байт/сек вниз.
    pub down_bps: u64,
    /// Байт/сек вверх.
    pub up_bps: u64,
    pub latency_ms: u64,
    /// Доля обрываемых запросов, 0–100 %.
    pub loss_percent: u8,
}

const fn preset(
    name: &'static str,
    down_bps: u64,
    up_bps: u64,
    latency_ms: u64,
    loss_percent: u8,
) -> ThrottlePreset {
    ThrottlePreset {
        name,
        values: ThrottleValues {
            down_bps,
            up_bps,
            latency_ms,
            loss_percent,
        },
    }
}

pub const THROTTLE_PRESETS: &[ThrottlePreset] = &[
    preset("56 kbps Modem", 7_000, 6_000, 200, 0),
    preset("256 kbps ISDN/DSL", 32_000, 32_000, 50, 0),
    preset("3G", 96_000, 48_000, 150, 0),
    preset("Lossy 3G", 96_000, 48_000, 300, 5),
    preset("4G", 1_250_000, 625_000, 50, 0),
    preset("100 Mbps Fibre", 12_500_000, 12_500_000, 5, 0),
    preset("Very Bad Network", 125_000, 62_500, 500, 10),
    preset("100% Loss", 12_500_000, 12_500_000, 0, 100),
];

/// Включить throttle по пресету (на весь трафик или по location).
pub fn with_throttle_preset(
    rules: RuleSet,
    preset: &ThrottlePreset,
    location: Option<TrafficMatch>,
) -> RuleSet {
    with_throttle_values(rules, preset.name, preset.values, location)
}

/// Имя профиля, заданного вручную (не пресет).
pub const CUSTOM_THROTTLE: &str = "Custom";

/// Свои значения throttle; scope и id профиля сохраняются.
pub fn with_throttle_custom(rules: RuleSet, values: ThrottleValues) -> RuleSet {
    let scope = rules.throttle.as_ref().and_then(|t| t.r#match.clone());
    with_throttle_values(rules, CUSTOM_THROTTLE, values, scope)
}

fn with_throttle_values(
    rules: RuleSet,
    name: &str,
    values: ThrottleValues,
    scope: Option<TrafficMatch>,
) -> RuleSet {
    RuleSet {
        throttle: Some(ThrottleProfile {
            id: rules.throttle.as_ref().map(|t| t.id).unwrap_or_default(),
            name: name.to_string(),
            down_bps: Some(values.down_bps),
            up_bps: Some(values.up_bps),
            latency_ms: values.latency_ms,
            loss_percent: values.loss_percent,
            r#match: scope,
        }),
        ..rules
    }
}

/// Ограничить throttle location'ом (`None` — весь трафик). Без профиля —
/// ошибка: сначала выбирают пресет или свои значения.
pub fn with_throttle_scope(
    rules: RuleSet,
    scope: Option<TrafficMatch>,
) -> Result<RuleSet, DomainError> {
    let Some(profile) = rules.throttle.clone() else {
        return Err(DomainError::ValidationError(
            "choose a throttle preset or custom values first".into(),
        ));
    };
    Ok(RuleSet {
        throttle: Some(ThrottleProfile {
            r#match: scope,
            ..profile
        }),
        ..rules
    })
}

/// Выключить throttle.
pub fn without_throttle(rules: RuleSet) -> RuleSet {
    RuleSet {
        throttle: None,
        ..rules
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn allow_host_is_added_once_and_turns_the_list_on() {
        let rules = with_host_allowed(RuleSet::default(), "https://API.com:8443/v1").expect("ok");
        assert!(rules.allow_enabled);
        assert_eq!(rules.allow_list.len(), 1);
        assert_eq!(rules.allow_list[0].host_pattern, "api.com");

        let disabled = with_rule_toggled(rules.clone(), rules.allow_list[0].id);
        let again = with_host_allowed(disabled, "api.com").expect("ok");
        assert_eq!(again.allow_list.len(), 1, "no duplicates");
        assert!(again.allow_list[0].enabled, "existing entry is re-enabled");
    }

    #[test]
    fn dns_spoof_entries_take_part_in_generic_rule_edits() {
        let rules = upsert_dns_spoof(RuleSet::default(), None, "https://API.test/v1", "127.0.0.1")
            .expect("ok");
        assert_eq!(rules.dns_spoof[0].host_pattern, "api.test");
        let id = rules.dns_spoof[0].id;
        let edited = upsert_dns_spoof(rules, Some(id), "*.api.test", "[::1]").expect("ok");
        assert_eq!(edited.dns_spoof.len(), 1);
        assert_eq!(edited.dns_spoof[0].host_pattern, "*.api.test");
        assert_eq!(edited.dns_spoof[0].ip, parse_ip("::1").expect("ip"));

        assert!(!with_rule_toggled(edited.clone(), id).dns_spoof[0].enabled);
        assert_eq!(with_rule_duplicated(edited.clone(), id).dns_spoof.len(), 2);
        assert!(without_rule(edited.clone(), id).dns_spoof.is_empty());
        let off = with_tool_enabled(edited, Tool::DnsSpoofing, false);
        assert!(!tool_enabled(&off, Tool::DnsSpoofing));
        assert!(off.effective_dns_overrides().is_empty());
    }

    #[test]
    fn reverse_proxy_entries_take_part_in_generic_rule_edits() {
        let rules = upsert_reverse_proxy(
            RuleSet::default(),
            None,
            9090,
            "https://api.example.com".into(),
            false,
        );
        let id = rules.reverse_proxies[0].id;
        let edited =
            upsert_reverse_proxy(rules, Some(id), 9091, "http://localhost:3000".into(), true);
        assert_eq!(edited.reverse_proxies.len(), 1);
        assert_eq!(edited.reverse_proxies[0].listen_port, 9091);
        assert!(edited.reverse_proxies[0].preserve_host);

        let copied = with_rule_duplicated(edited.clone(), id);
        assert_eq!(copied.reverse_proxies.len(), 2);
        assert!(
            !copied.reverse_proxies[1].enabled,
            "a copy would take the same port"
        );
        assert!(!with_rule_toggled(edited.clone(), id).reverse_proxies[0].enabled);
        assert!(without_rule(edited.clone(), id).reverse_proxies.is_empty());
        let off = with_tool_enabled(edited, Tool::ReverseProxy, false);
        assert!(!tool_enabled(&off, Tool::ReverseProxy));
        assert!(off.effective_reverse_proxies().is_empty());
    }

    #[test]
    fn dns_spoof_rejects_bad_ip() {
        let error = upsert_dns_spoof(RuleSet::default(), None, "api.test", "localhost")
            .expect_err("not an IP");
        assert!(error.to_string().contains("not an IP address"), "{error}");
    }

    #[test]
    fn allow_entries_take_part_in_generic_rule_edits() {
        let rules = upsert_allow_host(RuleSet::default(), None, "*.example.com").expect("ok");
        let id = rules.allow_list[0].id;
        let edited = upsert_allow_host(rules, Some(id), "cdn.example.com").expect("ok");
        assert_eq!(edited.allow_list.len(), 1);
        assert_eq!(edited.allow_list[0].host_pattern, "cdn.example.com");

        let toggled = with_rule_toggled(edited.clone(), id);
        assert!(!toggled.allow_list[0].enabled);
        let copied = with_rule_duplicated(edited.clone(), id);
        assert_eq!(copied.allow_list.len(), 2);
        assert_ne!(copied.allow_list[1].id, id);
        assert!(without_rule(edited, id).allow_list.is_empty());

        let on = with_tool_enabled(RuleSet::default(), Tool::AllowList, true);
        assert!(tool_enabled(&on, Tool::AllowList));
        assert!(!tool_enabled(&RuleSet::default(), Tool::AllowList));
    }

    #[test]
    fn parses_host_only_and_wildcards() {
        let m = parse_location("API.Example.com").expect("ok");
        assert_eq!(m.host_glob.as_deref(), Some("api.example.com"));
        assert_eq!(m.path_glob, None);
        assert_eq!(parse_location("*.ads.com/*").expect("ok").path_glob, None);
    }

    #[test]
    fn parses_full_url_dropping_scheme_and_port() {
        let m = parse_location("https://api.example.com:8443/v1/users*").expect("ok");
        assert_eq!(m.host_glob.as_deref(), Some("api.example.com"));
        assert_eq!(m.path_glob.as_deref(), Some("/v1/users*"));
    }

    #[test]
    fn rejects_empty_host() {
        assert!(parse_location("").is_err());
        assert!(parse_location("https:///path").is_err());
        assert!(parse_location("bad host/x").is_err());
    }

    #[test]
    fn formats_regex_query_and_graphql_conditions() {
        let regex = TrafficMatch {
            url_regex: Some(r"https://api\.com/.*".into()),
            method: Some("POST".into()),
            ..TrafficMatch::default()
        };
        assert_eq!(format_location(&regex), r"POST URL ~ https://api\.com/.*");
        let graphql = TrafficMatch {
            host_glob: Some("api.com".into()),
            path_glob: Some("/graphql".into()),
            ignore_query: true,
            graphql_operation: Some("GetUser".into()),
            ..TrafficMatch::default()
        };
        assert_eq!(
            format_location(&graphql),
            "api.com/graphql (any query) · GraphQL GetUser"
        );
    }

    #[test]
    fn formats_location_back() {
        let m = parse_location("api.com/v1/*").expect("ok");
        assert_eq!(format_location(&m), "api.com/v1/*");
        assert_eq!(
            format_location(&parse_location("ads.com").expect("ok")),
            "ads.com/*"
        );
    }

    #[test]
    fn add_toggle_remove_without_touching_other_rules() {
        let loc = || parse_location("a.com").expect("ok");
        let rules = upsert_block(RuleSet::default(), None, loc(), BlockAction::Status(403));
        let rules = upsert_map_local(
            rules,
            None,
            loc(),
            PathBuf::from("/tmp/a.json"),
            MapLocalResponse::default(),
        );
        let rules = upsert_map_remote(
            rules,
            None,
            loc(),
            "http://localhost:3000".into(),
            false,
            RemotePathMode::Replace,
        );
        assert_eq!(
            (
                rules.blocks.len(),
                rules.map_local.len(),
                rules.map_remote.len()
            ),
            (1, 1, 1)
        );

        let local_id = rules.map_local[0].id;
        let toggled = with_rule_toggled(rules.clone(), local_id);
        assert!(!toggled.map_local[0].enabled);
        assert!(toggled.blocks[0].enabled && toggled.map_remote[0].enabled);

        let removed = without_rule(toggled, local_id);
        assert!(removed.map_local.is_empty());
        assert_eq!((removed.blocks.len(), removed.map_remote.len()), (1, 1));
        assert_eq!(rules.map_local.len(), 1, "input snapshot untouched");
    }

    #[test]
    fn enabling_ssl_for_host_reuses_entry_and_does_not_decrypt_everything() {
        let rules = with_ssl_location(RuleSet::default(), "*").expect("ok");
        let rules = RuleSet {
            ssl_proxy_enabled: false,
            ..rules
        };
        let on = with_ssl_host_enabled(rules, "api.com").expect("ok");
        assert!(on.ssl_proxy_enabled);
        let star = on
            .ssl_proxy
            .iter()
            .find(|e| e.host_pattern == "*")
            .expect("star kept");
        assert!(
            !star.enabled,
            "global SSL was off: '*' must not start decrypting all"
        );
        assert!(
            on.ssl_proxy
                .iter()
                .any(|e| e.host_pattern == "api.com" && e.enabled)
        );

        let disabled = with_rule_toggled(on.clone(), on.ssl_proxy[1].id);
        let again = with_ssl_host_enabled(disabled, "api.com").expect("ok");
        assert_eq!(
            again.ssl_proxy.len(),
            2,
            "existing entry re-enabled, not duplicated"
        );
        assert!(again.ssl_proxy[1].enabled);
    }

    #[test]
    fn ssl_locations_add_toggle_remove() {
        let rules =
            with_ssl_location(RuleSet::default(), "https://*.Example.com/any/path").expect("ok");
        assert_eq!(rules.ssl_proxy[0].host_pattern, "*.example.com");
        let id = rules.ssl_proxy[0].id;
        assert!(!with_rule_toggled(rules.clone(), id).ssl_proxy[0].enabled);
        assert!(without_rule(rules, id).ssl_proxy.is_empty());
        assert!(with_ssl_location(RuleSet::default(), "  ").is_err());
    }

    #[test]
    fn upsert_appends_new_and_replaces_existing_in_place() {
        let loc = |h: &str| parse_location(h).expect("ok");
        let rules = upsert_block(
            RuleSet::default(),
            None,
            loc("a.com"),
            BlockAction::Status(403),
        );
        let rules = upsert_block(rules, None, loc("b.com"), BlockAction::Status(403));
        let first = rules.blocks[0].id;
        let toggled = with_rule_toggled(rules, first);

        let edited = upsert_block(toggled, Some(first), loc("c.com"), BlockAction::Close);
        assert_eq!(edited.blocks.len(), 2, "replaced, not appended");
        assert_eq!(edited.blocks[0].id, first, "id kept");
        assert_eq!(edited.blocks[0].r#match.host_glob.as_deref(), Some("c.com"));
        assert_eq!(edited.blocks[0].action, BlockAction::Close);
        assert!(!edited.blocks[0].enabled, "enabled flag kept on edit");

        let local = upsert_map_local(
            RuleSet::default(),
            None,
            loc("a.com"),
            "/tmp/a".into(),
            MapLocalResponse::default(),
        );
        let id = local.map_local[0].id;
        let not_found = MapLocalResponse {
            status: 404,
            content_type: Some("text/plain".into()),
            headers: vec![HeaderEntry::new("Cache-Control", "no-store")],
        };
        let local = upsert_map_local(
            local,
            Some(id),
            loc("a.com/v2/*"),
            "/tmp/b".into(),
            not_found,
        );
        assert_eq!(local.map_local.len(), 1);
        assert_eq!(local.map_local[0].local_path, PathBuf::from("/tmp/b"));
        assert_eq!(
            (
                local.map_local[0].status,
                local.map_local[0].content_type.as_deref()
            ),
            (404, Some("text/plain"))
        );
        assert_eq!(
            local.map_local[0].headers,
            vec![HeaderEntry::new("Cache-Control", "no-store")]
        );

        let remote = upsert_map_remote(
            RuleSet::default(),
            None,
            loc("a.com"),
            "http://x".into(),
            false,
            RemotePathMode::Replace,
        );
        let id = remote.map_remote[0].id;
        let remote = upsert_map_remote(
            remote,
            Some(id),
            loc("a.com"),
            "http://y".into(),
            true,
            RemotePathMode::Prefix,
        );
        assert_eq!(remote.map_remote[0].path_mode, RemotePathMode::Prefix);
        assert_eq!(
            (
                remote.map_remote[0].remote.as_str(),
                remote.map_remote[0].preserve_host
            ),
            ("http://y", true)
        );

        let ssl = with_ssl_location(RuleSet::default(), "a.com").expect("ok");
        let id = ssl.ssl_proxy[0].id;
        let ssl = upsert_ssl_host(ssl, Some(id), "*.b.com", true).expect("ok");
        assert_eq!(ssl.ssl_proxy.len(), 1);
        assert_eq!(ssl.ssl_proxy[0].host_pattern, "*.b.com");
        assert!(ssl.ssl_proxy[0].exclude, "edited into an exclusion");
    }

    #[test]
    fn enabling_an_excluded_host_turns_it_into_decryption() {
        let rules = RuleSet {
            ssl_proxy: vec![SslProxyEntry::exclude("api.com")],
            ..RuleSet::default()
        };
        let on = with_ssl_host_enabled(rules, "api.com").expect("ok");
        assert_eq!(on.ssl_proxy.len(), 1, "no duplicate");
        assert!(on.ssl_proxy[0].enabled && !on.ssl_proxy[0].exclude);
    }

    #[test]
    fn duplicate_inserts_copy_after_original_with_new_id() {
        let loc = |h: &str| parse_location(h).expect("ok");
        let rules = upsert_block(
            RuleSet::default(),
            None,
            loc("a.com"),
            BlockAction::Status(403),
        );
        let rules = upsert_block(rules, None, loc("z.com"), BlockAction::Status(403));
        let id = rules.blocks[0].id;
        let dup = with_rule_duplicated(rules, id);
        assert_eq!(dup.blocks.len(), 3);
        assert_eq!(dup.blocks[1].r#match, dup.blocks[0].r#match);
        assert_ne!(dup.blocks[1].id, id);
        assert_eq!(dup.blocks[2].r#match.host_glob.as_deref(), Some("z.com"));
    }

    #[test]
    fn tool_switches_map_to_flags() {
        let off = with_tool_enabled(RuleSet::default(), Tool::BlockList, false);
        assert!(!off.blocks_enabled && off.map_local_enabled);
        assert!(!with_tool_enabled(RuleSet::default(), Tool::MapLocal, false).map_local_enabled);
        assert!(!with_tool_enabled(RuleSet::default(), Tool::MapRemote, false).map_remote_enabled);
        assert!(!with_tool_enabled(RuleSet::default(), Tool::Throttle, false).throttle_enabled);
        assert!(!with_tool_enabled(RuleSet::default(), Tool::SslProxying, false).ssl_proxy_enabled);
        assert!(tool_enabled(&off, Tool::MapLocal) && !tool_enabled(&off, Tool::BlockList));
    }

    #[test]
    fn custom_throttle_keeps_scope_and_id() {
        let on = with_throttle_preset(
            RuleSet::default(),
            &THROTTLE_PRESETS[0],
            Some(parse_location("a.com").expect("ok")),
        );
        let id = on.throttle.as_ref().map(|t| t.id);
        let custom = with_throttle_custom(
            on,
            ThrottleValues {
                down_bps: 10_000,
                up_bps: 2_000,
                latency_ms: 75,
                loss_percent: 3,
            },
        );
        let t = custom.throttle.expect("throttle");
        assert_eq!(Some(t.id), id);
        assert_eq!(t.name, CUSTOM_THROTTLE);
        assert_eq!(
            (t.down_bps, t.up_bps, t.latency_ms, t.loss_percent),
            (Some(10_000), Some(2_000), 75, 3)
        );
        assert_eq!(
            t.r#match.and_then(|m| m.host_glob).as_deref(),
            Some("a.com")
        );
    }

    #[test]
    fn loss_presets_carry_their_drop_rate() {
        let loss = |name: &str| {
            THROTTLE_PRESETS
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.values.loss_percent)
        };
        assert_eq!(loss("100% Loss"), Some(100));
        assert_eq!(loss("Very Bad Network"), Some(10));
        assert_eq!(loss("3G"), Some(0));
        let rules = with_throttle_preset(RuleSet::default(), &THROTTLE_PRESETS[3], None);
        assert_eq!(rules.throttle.map(|t| t.loss_percent), Some(5));
    }

    #[test]
    fn throttle_scope_requires_profile_and_keeps_values() {
        assert!(with_throttle_scope(RuleSet::default(), None).is_err());
        let on = with_throttle_preset(RuleSet::default(), &THROTTLE_PRESETS[2], None);
        let scoped =
            with_throttle_scope(on, Some(parse_location("a.com").expect("ok"))).expect("ok");
        let t = scoped.throttle.expect("throttle");
        assert_eq!(t.name, "3G");
        assert_eq!(
            t.r#match.and_then(|m| m.host_glob).as_deref(),
            Some("a.com")
        );
    }

    #[test]
    fn throttle_preset_on_and_off_keeps_profile_id() {
        let preset = &THROTTLE_PRESETS[2];
        let on = with_throttle_preset(RuleSet::default(), preset, None);
        let profile = on.throttle.clone().expect("throttle");
        assert_eq!(profile.name, "3G");
        assert_eq!(profile.down_bps, Some(96_000));

        let again = with_throttle_preset(on, &THROTTLE_PRESETS[3], None);
        assert_eq!(again.throttle.as_ref().map(|t| t.id), Some(profile.id));
        assert!(without_throttle(again).throttle.is_none());
    }
}
