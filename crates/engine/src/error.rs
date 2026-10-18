//! Ошибки движка прокси (adapter-слой).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("failed to bind listener on {addr}: {source}")]
    Bind {
        addr: String,
        #[source]
        source: std::io::Error,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid upstream address {addr}: {reason}")]
    InvalidAddr { addr: String, reason: String },

    #[error("invalid upstream uri: {0}")]
    InvalidUri(String),

    #[error("tls error: {0}")]
    Tls(String),

    #[error("no native root certificates available")]
    NoRootCertificates,

    #[error("hyper error: {0}")]
    Hyper(#[from] hyper::Error),
}

/// Ошибка со всей цепочкой `source()` — hyper-util верхним уровнем пишет
/// лишь «client error (Connect)», причина (refused/DNS/TLS) — глубже.
pub(crate) fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_chain_appends_sources_without_repeats() {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");
        let err = EngineError::Io(io);
        assert_eq!(error_chain(&err), "io error: connection refused");

        let bind = EngineError::Bind {
            addr: "x".into(),
            source: std::io::Error::other("busy"),
        };
        assert_eq!(error_chain(&bind), "failed to bind listener on x: busy");
    }
}
