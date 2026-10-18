//! Мониторируемые тела HTTP-сообщений.
//!
//! Тело проксируется ПОТОКОМ (никакой агрегации в память): hyper опрашивает
//! [`MonitoredBody`] кадр-за-кадром, обёртка считает байты, шлёт
//! `BodyProgress`, а на терминальном состоянии (конец потока / ошибка /
//! drop без опроса) ровно один раз вызывает callback — он публикует
//! `RequestBodyDone` / `ExchangeCompleted` / `ExchangeFailed`.
//!
//! Drop-детект: если hyper бросает тело, не дочитав до конца (клиент
//! оборвал соединение), терминальный кадр не придёт — событие публикует
//! `Drop`-гард с признаком `Dropped`.
//!
//! # Tee захвата тела (Phase 4)
//!
//! `StreamTracker` дополнительно копирует КАДОЧНЫЕ ДЕКОДИРОВАННЫЕ байты тела
//! в ограниченный буфер ([`StreamTracker::set_tee`]) и на терминальном
//! состоянии (ровно один раз, тем же гардом, что и события) сбрасывает
//! накопленное в `BodyRepository`, получая `BodyRef` для события.
//!
//! Важно про семантику байт (решение D7): кадры здесь — тело ПОСЛЕ http1-кодека
//! hyper, т.е. chunked-фрейминг уже снят — это декодированное тело.
//! Wire-сырой вид (чанки как чанки) приедет в Phase 5 через
//! ре-сериализацию/нижний слой, здесь он принципиально не достижим.
//!
//! Поток НЕ прерывается при переполнении cap: tee просто перестаёт
//! дописывать (сохранённый префикс обрезается по cap), а счётчик `bytes`
//! и события прогресса продолжают считать полный поток.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use domain::entities::body::{BodyDescriptor, BodyRef};
use domain::repositories::BodyRepository;
use domain::value_objects::BodyId;
use http_body_util::BodyExt;
use hyper::body::Body as HttpBody;
use hyper::body::Frame;

/// Порог публикаций BodyProgress: событие шлём на каждые 256 KiB потока.
const PROGRESS_STEP_BYTES: u64 = 256 * 1024;

/// Терминальное состояние потока тела.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamEnd {
    /// Поток дочитан до конца.
    Done,
    /// Ошибка во время чтения.
    Failed,
    /// Тело брошено без дочитывания (клиент/сервер оборвал соединение).
    Dropped,
}

/// Ровно однократное завершение потока тела.
/// Callback терминального состояния потока: (конец, всего байт, BodyRef
/// сохранённого тела — None, если захват выключен/пуст/не удался).
type FinishFn = Box<dyn FnOnce(StreamEnd, u64, Option<BodyRef>) + Send>;
/// Callback прогресса потока (байт на текущий момент).
type ProgressFn = Box<dyn Fn(u64) + Send>;

/// Настройка tee-захвата тела (Phase 4).
pub(crate) struct TeeConfig {
    pub id: BodyId,
    /// Потолок буфера: после его заполнения tee перестаёт дописывать
    /// (поток продолжается), сохраняется префикс длиной ≤ cap.
    pub cap_bytes: u64,
    /// Content-Type/Content-Encoding из заголовков сообщения.
    pub descriptor: BodyDescriptor,
    pub repo: Arc<dyn BodyRepository>,
}

/// Состояние tee-буфера одного тела.
struct TeeState {
    /// None — захват тел отключён (record_bodies = false или трекер без tee).
    config: Option<TeeConfig>,
    buffer: Vec<u8>,
    /// Буфер переполнился: тело сохранится обрезанным префиксом.
    truncated: bool,
}

pub(crate) struct StreamTracker {
    bytes: AtomicU64,
    finished: Mutex<bool>,
    /// Вызывается один раз с (конец, байты, body_ref).
    finish: Mutex<Option<FinishFn>>,
    /// Периодический колбэк прогресса (каждые PROGRESS_STEP_BYTES).
    progress: Mutex<Option<ProgressFn>>,
    tee: Mutex<TeeState>,
}

impl StreamTracker {
    pub(crate) fn new(
        finish: impl FnOnce(StreamEnd, u64, Option<BodyRef>) + Send + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            bytes: AtomicU64::new(0),
            finished: Mutex::new(false),
            finish: Mutex::new(Some(Box::new(finish))),
            progress: Mutex::new(None),
            tee: Mutex::new(TeeState {
                config: None,
                buffer: Vec::new(),
                truncated: false,
            }),
        })
    }

    /// Включить публикацию прогресса (байт на текущий момент).
    pub(crate) fn set_progress(self: &Arc<Self>, f: impl Fn(u64) + Send + 'static) {
        *self.progress.lock().expect("tracker lock") = Some(Box::new(f));
    }

    /// Включить tee-захват тела в репозиторий (вызывается до первого кадра).
    pub(crate) fn set_tee(self: &Arc<Self>, config: TeeConfig) {
        let mut tee = self.tee.lock().expect("tracker lock");
        tee.config = Some(config);
    }

    fn count(&self, data: &[u8]) {
        let len = data.len() as u64;
        let prev = self.bytes.fetch_add(len, Ordering::Relaxed);
        let now = prev + len;
        // Публикуем прогресс при пересечении границы шага (для маленьких тел
        // прогресса не будет — только финальные события).
        if now / PROGRESS_STEP_BYTES > prev / PROGRESS_STEP_BYTES
            && let Some(p) = self.progress.lock().expect("tracker lock").as_ref()
        {
            p(now);
        }
        // Tee: копия кадра в ограниченный буфер (захват тел Phase 4).
        let mut tee = self.tee.lock().expect("tracker lock");
        if tee.config.is_none() || tee.truncated {
            return;
        }
        let cap = tee
            .config
            .as_ref()
            .map(|c| c.cap_bytes.min(usize::MAX as u64) as usize)
            .unwrap_or(0);
        if tee.buffer.len() >= cap {
            tee.truncated = true;
            return;
        }
        let room = cap - tee.buffer.len();
        if data.len() > room {
            tee.buffer.extend_from_slice(&data[..room]);
            tee.truncated = true;
        } else {
            tee.buffer.extend_from_slice(data);
        }
    }

    fn total(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Идемпотентное завершение потока.
    fn finish(&self, end: StreamEnd) {
        let mut finished = self.finished.lock().expect("tracker lock");
        if *finished {
            return;
        }
        *finished = true;
        drop(finished);
        // Tee-сброс выполняется тем же «ровно один раз»-гардом, что и
        // терминальное событие: конфиг забирается (Option::take), повторный
        // вход ничего не сохранит. Тело пишем только на чистом Done —
        // обрыв/ошибка оставляет буфер несохранённым (событие ошибки и так
        // терминально).
        let body_ref = if matches!(end, StreamEnd::Done) {
            self.flush_tee()
        } else {
            None
        };
        if let Some(cb) = self.finish.lock().expect("tracker lock").take() {
            cb(end, self.total(), body_ref);
        }
    }

    /// Сохранить накопленный tee-буфер в репозиторий. Вызывается ровно один
    /// раз из [`Self::finish`].
    fn flush_tee(&self) -> Option<BodyRef> {
        let mut tee = self.tee.lock().expect("tracker lock");
        let config = tee.config.take()?;
        if tee.buffer.is_empty() {
            // Пустое тело (204/GET без тела) — сохранять нечего.
            return None;
        }
        if tee.truncated {
            tracing::debug!(
                body = %config.id,
                stored = tee.buffer.len(),
                cap = config.cap_bytes,
                "body capture truncated at cap (stream continues)"
            );
        }
        let data = std::mem::take(&mut tee.buffer);
        // В дескрипторе — фактически сохранённый размер (префикс при обрезке).
        let descriptor = BodyDescriptor {
            size: data.len() as u64,
            ..config.descriptor
        };
        // Порог — синхронный вызов async-порта из poll/Drop. block_on здесь
        // НЕЛЬЗЯ: flush случается и внутри executor'а (poll тела под hyper/
        // tokio и даже под futures::executor в тестах) — вложенный block_on
        // паникует. Вместо этого ровно один poll с no-op waker'ом:
        // производственный MemorySpillBodyStore::put синхронен (std::fs +
        // mutex, без реальных await-точек) и завершается первым poll'ом в
        // любом контексте. Pending (по-настоящему асинхронный store) здесь
        // означает потерю тела — задокументированное ограничение; тогда
        // понадобится явный spawn-конвейер.
        let mut fut = std::pin::pin!(config.repo.put(config.id, data, descriptor));
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(Ok(body_ref)) => Some(body_ref),
            Poll::Ready(Err(e)) => {
                tracing::warn!(body = %config.id, error = %e, "tee flush failed; body lost");
                None
            }
            Poll::Pending => {
                tracing::warn!(
                    body = %config.id,
                    "body store returned Pending on first poll (async impl?); body lost"
                );
                None
            }
        }
    }
}

impl Drop for StreamTracker {
    fn drop(&mut self) {
        // Гард на случай, когда hyper бросает тело без терминального кадра.
        self.finish(StreamEnd::Dropped);
    }
}

/// Потоковое тело с подсчётом байтов и терминальным callback.
///
/// Ошибка унифицирована в `std::io::Error` (граница hyper допускает любой
/// `Into<Box<dyn Error + Send + Sync>>`); `Incoming` конвертируется через
/// [`http_body_util::BodyExt::map_err`] на сборке.
pub struct MonitoredBody {
    inner: Pin<Box<dyn HttpBody<Data = Bytes, Error = std::io::Error> + Send>>,
    tracker: Arc<StreamTracker>,
}

impl MonitoredBody {
    /// Тело из `Incoming` (или иного `B`) + трекер.
    pub(crate) fn from_body<B>(body: B, tracker: Arc<StreamTracker>) -> Self
    where
        B: HttpBody + Send + 'static,
        B::Data: Into<Bytes>,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let mapped = body
            .map_frame(|frame| frame.map_data(Into::into))
            .map_err(std::io::Error::other);
        Self {
            inner: Box::pin(mapped),
            tracker,
        }
    }
}

impl HttpBody for MonitoredBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_frame(cx) {
            Poll::Ready(None) => {
                this.tracker.finish(StreamEnd::Done);
                Poll::Ready(None)
            }
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.tracker.count(data);
                }
                // hyper может не опрашивать тело повторно, если после кадра
                // оно уже «кончилось» (size_hint/Content-Length): завершаем
                // поток по факу конца, не дожидаясь финального poll.
                if this.inner.is_end_stream() {
                    this.tracker.finish(StreamEnd::Done);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(e))) => {
                this.tracker.finish(StreamEnd::Failed);
                Poll::Ready(Some(Err(e)))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        let end = self.inner.is_end_stream();
        if end {
            // Пустое тело (204/HEAD/GET без тела): hyper узнаёт конец через
            // is_end_stream, ни одного poll_frame не будет.
            self.tracker.finish(StreamEnd::Done);
        }
        end
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use domain::errors::DomainError;
    use http_body_util::Full;
    use std::sync::atomic::AtomicUsize;

    fn drive<B: HttpBody<Data = Bytes, Error = std::io::Error> + Unpin>(
        mut body: B,
    ) -> Result<(u64, Vec<u8>), std::io::Error> {
        // Простейший опрос тела до конца (без реального executor'а не
        // понадобится: Full всегда готов).
        let mut bytes = Vec::new();
        loop {
            match futures::executor::block_on(http_body_util::BodyExt::frame(&mut body)) {
                Some(Ok(frame)) => {
                    if let Some(data) = frame.data_ref() {
                        bytes.extend_from_slice(data);
                    }
                }
                Some(Err(e)) => return Err(e),
                None => return Ok((bytes.len() as u64, bytes)),
            }
        }
    }

    /// Тестовое хранилище тел: помнит put'ы, отдаёт по ссылке.
    struct MemBodyRepo {
        puts: Mutex<Vec<(BodyId, Vec<u8>, BodyDescriptor)>>,
    }

    impl MemBodyRepo {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                puts: Mutex::new(Vec::new()),
            })
        }

        fn puts(&self) -> Vec<(BodyId, Vec<u8>, BodyDescriptor)> {
            self.puts.lock().expect("lock").clone()
        }
    }

    #[async_trait]
    impl BodyRepository for MemBodyRepo {
        async fn put(
            &self,
            id: BodyId,
            data: Vec<u8>,
            descriptor: BodyDescriptor,
        ) -> Result<BodyRef, DomainError> {
            self.puts
                .lock()
                .expect("lock")
                .push((id, data.clone(), descriptor.clone()));
            Ok(BodyRef {
                id,
                size: data.len() as u64,
                content_type: descriptor.content_type,
                encoding: descriptor.encoding,
                storage: Default::default(),
            })
        }

        async fn get(&self, body_ref: &BodyRef) -> Result<Option<Vec<u8>>, DomainError> {
            Ok(self
                .puts
                .lock()
                .expect("lock")
                .iter()
                .find(|(id, _, _)| *id == body_ref.id)
                .map(|(_, data, _)| data.clone()))
        }

        async fn get_range(
            &self,
            _body_ref: &BodyRef,
            _offset: u64,
            _len: u64,
        ) -> Result<Option<Vec<u8>>, DomainError> {
            Ok(None)
        }

        async fn delete(&self, _id: BodyId) -> Result<(), DomainError> {
            Ok(())
        }

        async fn total_size(&self) -> Result<u64, DomainError> {
            Ok(self
                .puts
                .lock()
                .expect("lock")
                .iter()
                .map(|(_, d, _)| d.len() as u64)
                .sum())
        }

        async fn clear(&self) -> Result<(), DomainError> {
            self.puts.lock().expect("lock").clear();
            Ok(())
        }
    }

    fn descriptor(ct: Option<&str>) -> BodyDescriptor {
        BodyDescriptor {
            size: 0,
            content_type: ct.map(String::from),
            encoding: Some("gzip".into()),
        }
    }

    #[test]
    fn full_body_completes_with_byte_count() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let tracker = StreamTracker::new(move |end, bytes, body| {
            assert_eq!(end, StreamEnd::Done);
            assert_eq!(bytes, 5);
            assert!(body.is_none(), "no tee configured -> no body");
            calls2.fetch_add(1, Ordering::SeqCst);
        });
        let body = MonitoredBody::from_body(Full::new(Bytes::from_static(b"hello")), tracker);
        let (n, data) = drive(body).expect("drain");
        assert_eq!(n, 5);
        assert_eq!(&data, b"hello");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn empty_body_completes_via_is_end_stream() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let tracker = StreamTracker::new(move |end, bytes, body| {
            assert_eq!(end, StreamEnd::Done);
            assert_eq!(bytes, 0);
            assert!(body.is_none());
            calls2.fetch_add(1, Ordering::SeqCst);
        });
        let body = MonitoredBody::from_body(Full::<Bytes>::default(), tracker);
        assert!(drive(body).expect("drain").0 == 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn drop_without_drain_fires_dropped() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let repo = MemBodyRepo::new();
        let tracker = StreamTracker::new(move |end, _bytes, body| {
            assert_eq!(end, StreamEnd::Dropped);
            assert!(body.is_none(), "dropped stream must not store body");
            calls2.fetch_add(1, Ordering::SeqCst);
        });
        tracker.set_tee(TeeConfig {
            id: BodyId::new(),
            cap_bytes: 1024,
            descriptor: descriptor(None),
            repo,
        });
        let _body = MonitoredBody::from_body(Full::new(Bytes::from_static(b"abc")), tracker);
        drop(_body);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn tee_stores_full_body_once_with_descriptor() {
        let repo = MemBodyRepo::new();
        let seen_ref = Arc::new(Mutex::new(None::<BodyRef>));
        let seen_ref_cell = seen_ref.clone();
        let tracker = StreamTracker::new(move |end, bytes, body| {
            assert_eq!(end, StreamEnd::Done);
            assert_eq!(bytes, 11);
            *seen_ref_cell.lock().unwrap() = body;
        });
        tracker.set_tee(TeeConfig {
            id: BodyId::new(),
            cap_bytes: 1024,
            descriptor: descriptor(Some("application/json")),
            repo: repo.clone(),
        });
        let body = MonitoredBody::from_body(Full::new(Bytes::from_static(b"hello world")), tracker);
        let (_, data) = drive(body).expect("drain");
        assert_eq!(&data, b"hello world");

        let stored = repo.puts();
        assert_eq!(stored.len(), 1, "exactly one put");
        assert_eq!(stored[0].1, b"hello world");
        assert_eq!(
            stored[0].2.content_type.as_deref(),
            Some("application/json")
        );
        assert_eq!(stored[0].2.encoding.as_deref(), Some("gzip"));
        let body_ref = seen_ref
            .lock()
            .unwrap()
            .clone()
            .expect("BodyRef in callback");
        assert_eq!(body_ref.size, 11);
        assert_eq!(body_ref.id, stored[0].0);
        assert_eq!(body_ref.content_type.as_deref(), Some("application/json"));
    }

    #[test]
    fn tee_cap_truncates_but_stream_count_continues() {
        let repo = MemBodyRepo::new();
        let total_seen = Arc::new(AtomicU64::new(0));
        let total_cell = total_seen.clone();
        let tracker = StreamTracker::new(move |_end, bytes, body| {
            total_cell.store(bytes, Ordering::SeqCst);
            assert!(body.is_some(), "truncated prefix is still stored");
        });
        tracker.set_tee(TeeConfig {
            id: BodyId::new(),
            cap_bytes: 4,
            descriptor: descriptor(None),
            repo: repo.clone(),
        });
        let payload = Bytes::from_static(b"abcdefgh");
        let body = MonitoredBody::from_body(Full::new(payload), tracker);
        let (n, data) = drive(body).expect("drain");
        assert_eq!(n, 8, "stream itself streams everything");
        assert_eq!(&data, b"abcdefgh");
        assert_eq!(total_seen.load(Ordering::SeqCst), 8);

        let stored = repo.puts();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].1, b"abcd", "stored prefix is capped");
    }

    #[test]
    fn tee_flush_is_exactly_once() {
        // Done через poll + поздний Drop-гард трекера — put ровно один.
        let repo = MemBodyRepo::new();
        let tracker = StreamTracker::new(|_, _, _| {});
        tracker.set_tee(TeeConfig {
            id: BodyId::new(),
            cap_bytes: 1024,
            descriptor: descriptor(None),
            repo: repo.clone(),
        });
        let body = MonitoredBody::from_body(Full::new(Bytes::from_static(b"xy")), tracker.clone());
        let _ = drive(body).expect("drain"); // Done-путь сохранил; body упал внутри drive
        assert_eq!(repo.puts().len(), 1);
        drop(tracker); // Drop-гард: второй finish — no-op
        assert_eq!(repo.puts().len(), 1, "second terminal must not re-store");
    }

    #[test]
    fn tee_without_config_never_touches_repo() {
        let repo = MemBodyRepo::new();
        let tracker = StreamTracker::new(|_, _, _| {});
        let body = MonitoredBody::from_body(Full::new(Bytes::from_static(b"zz")), tracker);
        let _ = drive(body).expect("drain");
        assert!(repo.puts().is_empty(), "disabled capture stores nothing");
    }
}
