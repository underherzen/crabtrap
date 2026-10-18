//! Сетевые настройки: DNS-override, port forwarding, reverse proxy (Phase 10).

use async_trait::async_trait;

use crate::entities::network::{DnsOverride, PortForward, ReverseProxyConfig};
use crate::errors::DomainError;

#[async_trait]
pub trait NetworkRepository: Send + Sync {
    async fn list_dns_overrides(&self) -> Result<Vec<DnsOverride>, DomainError>;
    async fn set_dns_overrides(&self, entries: Vec<DnsOverride>) -> Result<(), DomainError>;

    async fn list_port_forwards(&self) -> Result<Vec<PortForward>, DomainError>;
    async fn set_port_forwards(&self, entries: Vec<PortForward>) -> Result<(), DomainError>;

    async fn list_reverse_configs(&self) -> Result<Vec<ReverseProxyConfig>, DomainError>;
    async fn set_reverse_configs(
        &self,
        entries: Vec<ReverseProxyConfig>,
    ) -> Result<(), DomainError>;
}
