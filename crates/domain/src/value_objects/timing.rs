//! Фазы тайминга обмена (аналог Charles «Timing», маппится в HAR timings).

use serde::{Deserialize, Serialize};

/// Фазы выполнения запроса в миллисекундах.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TimingPhases {
    pub dns_ms: f64,
    pub connect_ms: f64,
    pub tls_ms: f64,
    pub send_ms: f64,
    pub wait_ms: f64,
    pub receive_ms: f64,
    pub total_ms: f64,
}

impl TimingPhases {
    /// Сумма фаз, если `total_ms` не проставлен движком.
    pub fn or_summed_total(&self) -> f64 {
        if self.total_ms > 0.0 {
            self.total_ms
        } else {
            self.dns_ms
                + self.connect_ms
                + self.tls_ms
                + self.send_ms
                + self.wait_ms
                + self.receive_ms
        }
    }
}
