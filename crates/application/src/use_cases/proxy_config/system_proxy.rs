//! Системный прокси macOS (кнопка «macOS Proxy»): включить/выключить
//! проксирование ОС через crabtrap (`networksetup` в infrastructure).

use std::sync::Arc;

use crate::dto::proxy_config::SystemProxyStatusDto;
use crate::services::SystemProxyService;
use domain::errors::DomainError;
use domain::value_objects::SocketAddrVo;

/// Включить системный HTTP/HTTPS-прокси на `listen`.
pub struct SetSystemProxyUseCase {
    service: Arc<dyn SystemProxyService>,
}

impl SetSystemProxyUseCase {
    pub fn new(service: Arc<dyn SystemProxyService>) -> Self {
        Self { service }
    }

    /// `bypass` пустой → infrastructure подставит дефолт
    /// (127.0.0.1, localhost, *.local, 169.254.0.0/16).
    pub async fn execute(
        &self,
        listen: SocketAddrVo,
        bypass: Vec<String>,
    ) -> Result<(), DomainError> {
        if listen.port == 0 {
            return Err(DomainError::ValidationError(
                "system proxy: port must not be 0".into(),
            ));
        }
        self.service.enable(listen, bypass).await
    }
}

/// Выключить системный прокси (все сервисы).
pub struct UnsetSystemProxyUseCase {
    service: Arc<dyn SystemProxyService>,
}

impl UnsetSystemProxyUseCase {
    pub fn new(service: Arc<dyn SystemProxyService>) -> Self {
        Self { service }
    }

    pub async fn execute(&self) -> Result<(), DomainError> {
        self.service.disable().await
    }
}

/// Текущий статус системного прокси.
pub struct GetSystemProxyStatusUseCase {
    service: Arc<dyn SystemProxyService>,
}

impl GetSystemProxyStatusUseCase {
    pub fn new(service: Arc<dyn SystemProxyService>) -> Self {
        Self { service }
    }

    pub async fn execute(&self) -> Result<SystemProxyStatusDto, DomainError> {
        self.service.status().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct MockService {
        calls: Mutex<Vec<&'static str>>,
        fail: bool,
    }

    #[async_trait]
    impl SystemProxyService for MockService {
        async fn enable(
            &self,
            _listen: SocketAddrVo,
            _bypass: Vec<String>,
        ) -> Result<(), DomainError> {
            self.calls.lock().unwrap().push("enable");
            if self.fail {
                Err(DomainError::InfrastructureError("boom".into()))
            } else {
                Ok(())
            }
        }

        async fn disable(&self) -> Result<(), DomainError> {
            self.calls.lock().unwrap().push("disable");
            Ok(())
        }

        async fn status(&self) -> Result<SystemProxyStatusDto, DomainError> {
            self.calls.lock().unwrap().push("status");
            Ok(SystemProxyStatusDto {
                enabled: true,
                server: Some("127.0.0.1:8080".into()),
                secure_server: Some("127.0.0.1:8080".into()),
                configured_servers: vec!["127.0.0.1:8080".into()],
                bypass: Vec::new(),
                details: None,
            })
        }
    }

    fn addr(port: u16) -> SocketAddrVo {
        SocketAddrVo::new(domain::value_objects::Host::new("127.0.0.1").unwrap(), port)
    }

    #[test]
    fn set_validates_port_and_calls_enable() {
        let mock = Arc::new(MockService {
            calls: Mutex::new(Vec::new()),
            fail: false,
        });
        let uc = SetSystemProxyUseCase::new(mock.clone());
        assert!(matches!(
            futures::executor::block_on(uc.execute(addr(0), Vec::new())),
            Err(DomainError::ValidationError(_))
        ));
        assert!(futures::executor::block_on(uc.execute(addr(8080), Vec::new())).is_ok());
        assert_eq!(*mock.calls.lock().unwrap(), vec!["enable"]);
    }

    #[test]
    fn unset_calls_disable_and_status_maps_errors() {
        let mock = Arc::new(MockService {
            calls: Mutex::new(Vec::new()),
            fail: false,
        });
        let unset = UnsetSystemProxyUseCase::new(mock.clone());
        futures::executor::block_on(unset.execute()).unwrap();
        let status = GetSystemProxyStatusUseCase::new(mock.clone());
        let dto = futures::executor::block_on(status.execute()).unwrap();
        assert!(dto.enabled);
        assert_eq!(dto.server.as_deref(), Some("127.0.0.1:8080"));
        assert_eq!(*mock.calls.lock().unwrap(), vec!["disable", "status"]);
    }
}
