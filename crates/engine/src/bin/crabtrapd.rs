//! crabtrapd — headless-бинарь движка (главное средство ручной проверки).
//!
//! Композиция: локальные мини-заглушки печатют события в stdout
//! (человекочитаемо или JSONL с --json) и всегда пропускают трафик на
//! уровне пайплайна (Passthrough/Proceed); SSL-проксирование — списком
//! `--mitm` (см. ниже). Сертификаты — реальный RcgenCertificateService
//! из infrastructure (единственная точка, где engine-бинарь касается
//! infra; фича `mitm-bin`).
//!
//! Аргументы: `--listen ADDR` (env CRABTRAP_LISTEN, дефолт 127.0.0.1:8080),
//! `--mitm GLOB` (повторяемый; env CRABTRAP_MITM — список через запятую),
//! `--json`. Останов: Ctrl-C (SIGINT) → EngineCommand::Stop → graceful.

use std::sync::Arc;

use application::dto::capture::ProxyEvent;
use application::services::{
    ConnectDecision, EventPublisher, InterceptContext, InterceptOutcome, InterceptionPipeline,
    RouteDecision,
};
use async_trait::async_trait;
use domain::value_objects::Host;

use engine::{EngineConfig, EngineDeps};

/// Печататель событий в stdout: движок зовёт publish sync и
/// неблокирующе, здесь просто println.
struct PrintingPublisher {
    json: bool,
}

impl EventPublisher for PrintingPublisher {
    fn publish(&self, event: ProxyEvent) {
        if self.json {
            match serde_json::to_string(&event) {
                Ok(line) => println!("{line}"),
                Err(e) => eprintln!("event serialize error: {e}"),
            }
            return;
        }
        match event {
            ProxyEvent::EngineStatus {
                listening,
                active_conns,
                error,
            } => match listening {
                Some(addr) => println!("[engine] listening {addr} · conns: {active_conns}"),
                None => {
                    if let Some(err) = error {
                        println!("[engine] stopped · error: {err}");
                    } else {
                        println!("[engine] stopped");
                    }
                }
            },
            ProxyEvent::ConnectionOpened { id, peer, .. } => {
                println!("[conn ] opened {peer} ({})", short(id));
            }
            ProxyEvent::ConnectionClosed { id } => println!("[conn ] closed ({})", short(id)),
            ProxyEvent::ExchangeStarted { exchange, tls, .. } => println!(
                "[xchg ] start {} {}://{}{} req~{}B{} ({})",
                exchange.method,
                exchange.scheme,
                exchange.host,
                exchange.path,
                exchange.req_size,
                tls.map(|t| format!(" tls={} mitm", t.version.as_deref().unwrap_or("?")))
                    .unwrap_or_default(),
                short(exchange.id)
            ),
            ProxyEvent::WebSocketMessage { id, message } => println!(
                "[ws   ] {:?} {:?} {}B ({})",
                message.direction,
                message.opcode,
                message.size,
                short(id)
            ),
            ProxyEvent::ScriptLog { level, message, .. } => {
                println!("[script] {level:?} {message}")
            }
            ProxyEvent::RequestEdited {
                id, method, uri, ..
            } => {
                println!("[xchg ] edited {method} {uri} ({})", short(id))
            }
            ProxyEvent::RequestBodyDone { id, body, raw_size } => println!(
                "[xchg ] req-body done {raw_size}B stored={} ({})",
                body.map(|b| b.size).unwrap_or(0),
                short(id)
            ),
            ProxyEvent::ResponseHeaders {
                id,
                status,
                version,
                ..
            } => {
                println!("[xchg ] resp {status} {version} ({})", short(id))
            }
            ProxyEvent::BodyProgress {
                id,
                direction,
                bytes_so_far,
            } => println!(
                "[xchg ] progress {direction:?} {bytes_so_far}B ({})",
                short(id)
            ),
            ProxyEvent::ExchangeCompleted {
                id,
                timing,
                body,
                raw_size,
            } => println!(
                "[xchg ] done {raw_size}B stored={}B total={:.1}ms send={:.1} wait={:.1} recv={:.1} ({})",
                body.as_ref().map(|b| b.size).unwrap_or(0),
                timing.total_ms,
                timing.send_ms,
                timing.wait_ms,
                timing.receive_ms,
                short(id)
            ),
            ProxyEvent::ExchangeFailed { id, error } => {
                println!("[xchg ] FAILED {error} ({})", short(id))
            }
            ProxyEvent::ExchangeTagged { id, tag } => {
                println!("[xchg ] tag {tag} ({})", short(id))
            }
            ProxyEvent::InterceptWaiting { preview, .. } => {
                println!(
                    "[intr ] waiting {} on exchange {}",
                    short(preview.intercept_id),
                    short(preview.exchange_id)
                )
            }
            ProxyEvent::InterceptResolved { id } => println!("[intr ] resolved {}", short(id)),
            ProxyEvent::ReverseProxyStatus {
                listen_port,
                listening,
                error,
            } => match (listening, error) {
                (Some(addr), _) => println!("[rev  ] :{listen_port} listening on {addr}"),
                (None, Some(error)) => println!("[rev  ] :{listen_port} error: {error}"),
                (None, None) => println!("[rev  ] :{listen_port} stopped"),
            },
        }
    }
}

/// Пайплайн-заглушка: всё пропускаем — headless-демон правила (Block, Map,
/// Throttle, Breakpoints, Rewrite, Scripts) не применяет; они есть только в app.
struct PassthroughPipeline;

#[async_trait]
impl InterceptionPipeline for PassthroughPipeline {
    async fn on_connect(&self, _host: &Host, _port: u16) -> ConnectDecision {
        ConnectDecision::Passthrough
    }

    async fn on_request(&self, context: InterceptContext) -> InterceptOutcome {
        InterceptOutcome::Proceed {
            message: context.message,
            route: RouteDecision::Origin,
        }
    }

    async fn on_response(&self, context: InterceptContext) -> InterceptOutcome {
        InterceptOutcome::Proceed {
            message: context.message,
            route: RouteDecision::Origin,
        }
    }
}

fn default_listen() -> String {
    std::env::var("CRABTRAP_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".to_string())
}

/// SSL-Proxying globs из env (через запятую).
fn mitm_from_env() -> Vec<String> {
    std::env::var("CRABTRAP_MITM")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Аргументы запуска: адрес, MITM-глобы, JSON-вывод, захват тел.
struct Args {
    listen: String,
    mitm: Vec<String>,
    json: bool,
    /// Захват тел включён (тумблер `--no-bodies`).
    record_bodies: bool,
    /// Потолок tee-буфера одного тела, байт (по умолчанию 50 MiB).
    cap_bytes: u64,
}

fn parse_args() -> Args {
    let mut listen = default_listen();
    let mut mitm = mitm_from_env();
    let mut json = false;
    let mut record_bodies = true;
    let mut cap_bytes = 50 * 1024 * 1024_u64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                if let Some(value) = args.next() {
                    listen = value;
                } else {
                    eprintln!("--listen requires an address (e.g. 127.0.0.1:8080)");
                    std::process::exit(2);
                }
            }
            "--mitm" => {
                if let Some(value) = args.next() {
                    mitm.push(value);
                } else {
                    eprintln!("--mitm requires a host glob (e.g. '*' or '*.example.com')");
                    std::process::exit(2);
                }
            }
            "--json" => json = true,
            "--no-bodies" => record_bodies = false,
            "--cap-bytes" => match args.next().and_then(|v| v.parse::<u64>().ok()) {
                Some(cap) => cap_bytes = cap,
                None => {
                    eprintln!("--cap-bytes requires a positive number of bytes (e.g. 1048576)");
                    std::process::exit(2);
                }
            },
            other => {
                eprintln!("unknown argument: {other}");
                eprintln!(
                    "usage: crabtrapd [--listen 127.0.0.1:8080] [--mitm '*'] [--json] \
                     [--no-bodies] [--cap-bytes 52428800]"
                );
                std::process::exit(2);
            }
        }
    }
    Args {
        listen,
        mitm,
        json,
        record_bodies,
        cap_bytes,
    }
}

/// Короткий идентификатор для логов (первые 8 символов uuid).
fn short(id: impl std::fmt::Display) -> String {
    id.to_string().chars().take(8).collect()
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = parse_args();
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    tracing::info!(%run_id, listen = %args.listen, json = args.json, "crabtrapd starting");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    runtime.block_on(async move {
        let (cmd_tx, cmd_rx) =
            async_channel::bounded::<application::dto::proxy_config::EngineCommand>(16);
        let certificate_service = certificate_service();
        // Тела: memory ≤1 MiB → spill на диск, кап — --cap-bytes.
        let bodies: Arc<dyn domain::repositories::BodyRepository> =
            Arc::new(infrastructure::repositories::MemorySpillBodyStore::new(
                1024 * 1024,
                args.cap_bytes,
                std::env::temp_dir().join("crabtrapd-bodies"),
            ));
        let deps = EngineDeps {
            events: Arc::new(PrintingPublisher { json: args.json }),
            pipeline: Arc::new(PassthroughPipeline),
            certificate_service: certificate_service.clone(),
            bodies,
            direct_requests: None,
        };
        let cfg = EngineConfig::new(&args.listen, 1024)
            .with_body_capture(args.record_bodies, args.cap_bytes);
        if !args.record_bodies {
            println!("body capture disabled (--no-bodies)");
        } else {
            println!("body capture enabled, cap {}B per body", args.cap_bytes);
        }

        // CA: создать/переиспользовать ДО приёма трафика (fingerprint в лог —
        // стабильность CA видна между перезапусками).
        match deps.certificate_service.ensure_ca().await {
            Ok(status) => println!(
                "[ssl  ] CA fingerprint={} path={}",
                status.fingerprint.as_deref().unwrap_or("?"),
                status.cert_path.display()
            ),
            Err(e) => eprintln!("CA init failed: {e}"),
        }

        // SSL-Proxying: MITM только для перечисленных glob'ов.
        if !args.mitm.is_empty() {
            let entries: Vec<domain::entities::rule::SslProxyEntry> = args
                .mitm
                .iter()
                .map(|glob| domain::entities::rule::SslProxyEntry::include(glob.clone()))
                .collect();
            println!("ssl-proxying enabled for {}", args.mitm.join(", "));
            let _ = cmd_tx
                .send(application::dto::proxy_config::EngineCommand::UpdateSslProxying { entries })
                .await;
        }

        let engine_task = tokio::spawn(engine::run(cfg, deps, cmd_rx));

        // Ctrl-C → Stop (graceful).
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to listen for ctrl-c");
        }
        tracing::info!("ctrl-c received, sending Stop");
        let _ = cmd_tx
            .send(application::dto::proxy_config::EngineCommand::Stop)
            .await;

        match engine_task.await {
            Ok(Ok(())) => tracing::info!("crabtrapd exited cleanly"),
            Ok(Err(e)) => {
                eprintln!("engine error: {e}");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("engine task panicked: {e}");
                std::process::exit(1);
            }
        }
    });
}

/// Реальный сервис сертификатов (infrastructure, фича `mitm-bin`); без
/// фичи — конфигурация без MITM невозможна и бинарь не собирается.
#[cfg(feature = "mitm-bin")]
fn certificate_service() -> Arc<dyn application::services::CertificateService> {
    use infrastructure::services::RcgenCertificateService;
    match RcgenCertificateService::default_location() {
        Ok(svc) => Arc::new(svc),
        Err(e) => {
            eprintln!("cannot locate certificate dir: {e}");
            std::process::exit(1);
        }
    }
}
