//! Порт «чьё это соединение»: локальный порт клиента → процесс (macOS —
//! lsof + libproc, реализация в infrastructure).

use domain::errors::DomainError;

use crate::dto::clients::{ClientAppDto, ClientLookupDto};

#[async_trait::async_trait]
pub trait ClientProcessService: Send + Sync {
    /// Процессы, держащие соединения `127.0.0.1:<client_port>` →
    /// `<proxy_port>`. Порты без найденного процесса в ответ не попадают.
    async fn resolve(
        &self,
        proxy_port: u16,
        client_ports: Vec<u16>,
    ) -> Result<Vec<ClientLookupDto>, DomainError>;

    /// Иконка приложения (PNG) для [`ClientAppDto::bundle_path`].
    async fn icon_png(&self, app: ClientAppDto) -> Result<Option<Vec<u8>>, DomainError>;
}
