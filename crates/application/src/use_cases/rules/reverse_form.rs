//! Форма Reverse Proxy (Tools → Reverse Proxy): локальный порт, цель по
//! полям (протокол, хост, порт), «Keep original Host». Ошибки — под своими
//! полями; без пути: запросы уходят на цель с исходным path.

use domain::entities::rule::ReverseProxyEntry;

use super::location_form::RemoteTargetForm;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReverseProxyForm {
    pub listen_port: String,
    pub https: bool,
    pub host: String,
    pub port: String,
    pub preserve_host: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReverseProxyErrors {
    pub listen_port: Option<String>,
    pub target: Option<String>,
}

/// Проверенные значения записи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseProxyValues {
    pub listen_port: u16,
    pub target: String,
    pub preserve_host: bool,
}

impl ReverseProxyForm {
    pub fn from_entry(entry: &ReverseProxyEntry) -> Self {
        let target = RemoteTargetForm::from_url(&entry.target);
        Self {
            listen_port: entry.listen_port.to_string(),
            https: target.https,
            host: target.host,
            port: target.port,
            preserve_host: entry.preserve_host,
        }
    }

    pub fn to_values(&self) -> Result<ReverseProxyValues, ReverseProxyErrors> {
        let listen_port = self
            .listen_port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(|| "Local port must be a number 1–65535".to_string());
        let target = RemoteTargetForm {
            https: self.https,
            host: self.host.clone(),
            port: self.port.clone(),
            path: String::new(),
            query: String::new(),
        }
        .to_url();
        match (listen_port, target) {
            (Ok(listen_port), Ok(target)) => Ok(ReverseProxyValues {
                listen_port,
                target,
                preserve_host: self.preserve_host,
            }),
            (port, target) => Err(ReverseProxyErrors {
                listen_port: port.err(),
                target: target.err(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(listen_port: &str, host: &str) -> ReverseProxyForm {
        ReverseProxyForm {
            listen_port: listen_port.into(),
            https: true,
            host: host.into(),
            port: "8443".into(),
            preserve_host: true,
        }
    }

    #[test]
    fn valid_form_gives_an_origin_target() {
        assert_eq!(
            form(" 9090 ", "api.example.com").to_values(),
            Ok(ReverseProxyValues {
                listen_port: 9090,
                target: "https://api.example.com:8443".into(),
                preserve_host: true,
            })
        );
    }

    #[test]
    fn errors_land_under_their_fields() {
        let errors = form("0", "").to_values().expect_err("invalid");
        assert!(errors.listen_port.is_some());
        assert!(errors.target.is_some());
        let only_port = form("70000", "api.example.com")
            .to_values()
            .expect_err("port");
        assert!(only_port.listen_port.is_some() && only_port.target.is_none());
    }

    #[test]
    fn entry_round_trips_through_the_form() {
        let entry = ReverseProxyEntry {
            preserve_host: true,
            ..ReverseProxyEntry::new(9090, "https://api.example.com:8443")
        };
        let values = ReverseProxyForm::from_entry(&entry)
            .to_values()
            .expect("valid");
        assert_eq!(values.listen_port, 9090);
        assert_eq!(values.target, entry.target);
        assert!(values.preserve_host);
    }
}
