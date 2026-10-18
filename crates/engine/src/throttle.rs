//! Throttle (Phase 6): ограничение пропускной способности тела и задержка.
//!
//! [`ThrottledBody`] оборачивает ТЕЛО (не TLS-стрим — так ограничение одно и
//! то же для http и расшифрованного https): данные отдаются порциями по
//! [`TICK`] с паузой `len / rate` после каждой. Большой кадр (64 KiB) не
//! «спит, а потом вываливается разом», а течёт равномерно.

use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use bytes::{Buf, Bytes};
use hyper::body::{Body, Frame, SizeHint};

/// Квант равномерности: порция = rate × TICK.
const TICK: Duration = Duration::from_millis(50);

/// Тело с ограничением скорости `rate_bps` байт/сек.
pub(crate) struct ThrottledBody<B> {
    inner: Pin<Box<B>>,
    rate_bps: u64,
    /// Остаток текущего кадра, ещё не отданный.
    pending: Bytes,
    /// Пауза после последней порции.
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl<B> ThrottledBody<B> {
    pub(crate) fn new(inner: B, rate_bps: u64) -> Self {
        Self {
            inner: Box::pin(inner),
            rate_bps: rate_bps.max(1),
            pending: Bytes::new(),
            sleep: None,
        }
    }

    fn chunk_len(&self) -> usize {
        let per_tick = self.rate_bps as u128 * TICK.as_millis() / 1000;
        usize::try_from(per_tick).unwrap_or(usize::MAX).max(1)
    }

    /// Отдать порцию из `pending` и запланировать паузу под её размер.
    fn emit_chunk(&mut self) -> Bytes {
        let take = self.chunk_len().min(self.pending.len());
        let chunk = self.pending.split_to(take);
        let pause = Duration::from_secs_f64(chunk.len() as f64 / self.rate_bps as f64);
        self.sleep = Some(Box::pin(tokio::time::sleep(pause)));
        chunk
    }
}

impl<B> Body for ThrottledBody<B>
where
    B: Body,
    B::Data: Buf,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if let Some(sleep) = this.sleep.as_mut() {
            ready!(sleep.as_mut().poll(cx));
            this.sleep = None;
        }
        if !this.pending.is_empty() {
            return Poll::Ready(Some(Ok(Frame::data(this.emit_chunk()))));
        }
        match ready!(this.inner.as_mut().poll_frame(cx)) {
            Some(Ok(frame)) => match frame.into_data() {
                Ok(mut data) => {
                    this.pending = data.copy_to_bytes(data.remaining());
                    if this.pending.is_empty() {
                        return Poll::Ready(Some(Ok(Frame::data(Bytes::new()))));
                    }
                    Poll::Ready(Some(Ok(Frame::data(this.emit_chunk()))))
                }
                // Трейлеры и прочие не-data кадры — без ограничения.
                Err(frame) => Poll::Ready(Some(Ok(
                    frame.map_data(|mut d| d.copy_to_bytes(d.remaining()))
                ))),
            },
            Some(Err(e)) => Poll::Ready(Some(Err(e))),
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.pending.is_empty() && self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        // Длина тела не меняется — Content-Length остаётся корректным.
        let inner = self.inner.size_hint();
        let pending = self.pending.len() as u64;
        let mut hint = SizeHint::new();
        hint.set_lower(inner.lower() + pending);
        if let Some(upper) = inner.upper() {
            hint.set_upper(upper + pending);
        }
        hint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::{BodyExt as _, Full};

    #[tokio::test]
    async fn throttled_body_keeps_content_and_respects_rate() {
        let data = Bytes::from(vec![7u8; 2_000]);
        let started = std::time::Instant::now();
        let collected = ThrottledBody::new(Full::new(data.clone()), 10_000)
            .collect()
            .await
            .expect("collect")
            .to_bytes();
        let elapsed = started.elapsed();
        assert_eq!(collected, data, "bytes unchanged");
        // 2000 B при 10 000 B/s ≈ 200 мс (последняя пауза не ждётся).
        assert!(
            elapsed >= Duration::from_millis(140),
            "too fast: {elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(2), "too slow: {elapsed:?}");
    }

    #[tokio::test]
    async fn large_frame_is_split_into_ticks() {
        let mut body = ThrottledBody::new(Full::new(Bytes::from(vec![1u8; 10_000])), 20_000);
        let first = body.frame().await.expect("frame").expect("ok");
        // rate 20 000 B/s × 50 мс = 1000 B на порцию.
        assert_eq!(first.into_data().expect("data").len(), 1_000);
    }

    #[test]
    fn size_hint_is_preserved() {
        let body = ThrottledBody::new(Full::new(Bytes::from_static(b"hello")), 1);
        assert_eq!(body.size_hint().exact(), Some(5));
    }
}
