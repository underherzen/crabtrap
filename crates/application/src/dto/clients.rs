//! Клиент обмена: какое приложение открыло соединение с прокси (Track G8).

use serde::{Deserialize, Serialize};

/// Приложение-клиент на этом Mac.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientAppDto {
    pub pid: u32,
    /// Отображаемое имя: имя `.app`-бандла или процесса.
    pub name: String,
    /// Путь к исполняемому файлу процесса (если macOS его отдала).
    pub executable: Option<String>,
    /// Внешний `.app`-бандл (для иконки и группировки помощников браузера).
    pub bundle_path: Option<String>,
}

/// Найденный клиент для порта соединения.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientLookupDto {
    /// Порт клиента (`peer` соединения на стороне прокси).
    pub client_port: u16,
    pub app: ClientAppDto,
}
