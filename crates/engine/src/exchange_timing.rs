//! Фазы тайминга одного обмена: Instant-метки по ходу стрима →
//! [`TimingPhases`] (send/wait/receive + connect/tls исходящего диалапа).

use std::sync::Mutex;
use std::time::Instant;

use domain::value_objects::timing::TimingPhases;

/// Фазы тайминга одного обмена (Instant-метки по ходу стрима).
pub(crate) struct ExchangeTiming {
    start: Instant,
    req_done: Mutex<Option<Instant>>,
    resp_headers: Mutex<Option<Instant>>,
    /// MITM-хендшейк клиент↔crabtrap (Phase 3) — приоритет для tls_ms.
    mitm_tls_ms: Option<f64>,
    upstream_connect_ms: Mutex<Option<f64>>,
    upstream_tls_ms: Mutex<Option<f64>>,
}

impl ExchangeTiming {
    pub(crate) fn started(mitm_tls_ms: Option<f64>) -> Self {
        Self {
            start: Instant::now(),
            req_done: Mutex::new(None),
            resp_headers: Mutex::new(None),
            mitm_tls_ms,
            upstream_connect_ms: Mutex::new(None),
            upstream_tls_ms: Mutex::new(None),
        }
    }

    pub(crate) fn mark_req_done(&self) {
        *self.req_done.lock().expect("timing lock") = Some(Instant::now());
    }

    pub(crate) fn mark_resp_headers(&self) {
        *self.resp_headers.lock().expect("timing lock") = Some(Instant::now());
    }

    /// Диалап исходящего соединения (из Connected-метрик коннектора).
    pub(crate) fn mark_upstream(&self, connect_ms: f64, tls_ms: f64) {
        *self.upstream_connect_ms.lock().expect("timing lock") = Some(connect_ms);
        if tls_ms > 0.0 {
            *self.upstream_tls_ms.lock().expect("timing lock") = Some(tls_ms);
        }
    }

    /// Итоговые фазы. send/wait/receive — по меткам; connect_ms — исходящий
    /// TCP-диалап; tls_ms — MITM-хендшейк клиент↔crabtrap, а для обычных
    /// https-forward — upstream-хендшейк; dns — 0 (HttpConnector не
    /// разделяет DNS-фазу; появится с DNS-работой Phase 10). total — от
    /// старта обмена до конца тела ответа.
    pub(crate) fn finalize(&self) -> TimingPhases {
        let end = Instant::now();
        let req_done = *self.req_done.lock().expect("timing lock");
        let resp_headers = *self.resp_headers.lock().expect("timing lock");
        let since = |t: Instant| (t - self.start).as_secs_f64() * 1000.0;
        let between = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1000.0;
        let send_ms = req_done.map(since).unwrap_or(0.0);
        let wait_ms = match (req_done, resp_headers) {
            (Some(a), Some(b)) => between(a, b),
            (None, Some(b)) => since(b),
            _ => 0.0,
        };
        let receive_ms = resp_headers.map(|h| between(h, end)).unwrap_or(0.0);
        let connect_ms = self
            .upstream_connect_ms
            .lock()
            .expect("timing lock")
            .unwrap_or(0.0);
        let tls_ms = self
            .mitm_tls_ms
            .or(*self.upstream_tls_ms.lock().expect("timing lock"))
            .unwrap_or(0.0);
        TimingPhases {
            dns_ms: 0.0,
            connect_ms,
            tls_ms,
            send_ms,
            wait_ms,
            receive_ms,
            total_ms: since(end),
        }
    }
}
