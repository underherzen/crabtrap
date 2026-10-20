//! ПОРТЫ — trait'ы внешнего мира. Реализации: `engine` (прокси),
//! `infrastructure` (каналы, файлы, osascript). Application знает только
//! эти интерфейсы.

mod body_decoder;
mod certificate_service;
mod client_process_service;
mod direct_http_service;
mod event_publisher;
mod export_file_writer;
mod import_file_reader;
mod interception_pipeline;
mod mirror_file_writer;
mod mock_file_store;
mod proxy_engine_service;
mod release_feed;
mod reveal_service;
mod script_runtime;
mod system_proxy_service;

pub use body_decoder::BodyDecoderService;
pub use certificate_service::CertificateService;
pub use client_process_service::ClientProcessService;
pub use direct_http_service::DirectHttpService;
pub use event_publisher::EventPublisher;
pub use export_file_writer::{ExportFile, ExportFileWriter};
pub use import_file_reader::ImportFileReader;
pub use interception_pipeline::InterceptionPipeline;
pub use interception_pipeline::{
    AbortMode, ConnectDecision, InterceptContext, InterceptOutcome, InterceptableMessage,
    RouteDecision,
};
pub use mirror_file_writer::MirrorFileWriter;
pub use mock_file_store::MockFileStore;
pub use proxy_engine_service::ProxyEngineService;
pub use release_feed::ReleaseFeed;
pub use reveal_service::RevealService;
pub use script_runtime::ScriptRuntime;
pub use system_proxy_service::SystemProxyService;
