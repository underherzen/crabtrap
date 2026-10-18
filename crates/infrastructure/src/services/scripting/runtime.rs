//! [`ScriptRuntime`] на пуле потоков boa.
//!
//! JS-контексты `!Send`, поэтому каждый живёт в своём OS-потоке; вызовы
//! раздаются через общую ограниченную очередь (переполнена — обмен идёт
//! без скрипта, а не копит память). Ожидающий ждёт не дольше таймаута;
//! задание, чей ожидающий уже ушёл, не выполняется. Поток с зависшим
//! скриптом прервать нельзя (у boa нет прерываний по времени), поэтому на
//! таймаут пул добавляет поток-замену; лишние потоки выходят после своего
//! задания. Паника в скрипте пересоздаёт контекст.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use application::dto::scripting::{ScriptCall, ScriptLogDto, ScriptLogLevel, ScriptRun};
use application::services::ScriptRuntime;
use async_trait::async_trait;
use futures::channel::oneshot;

use super::shared_state::SharedState;
use super::worker::Worker;

/// Потоков в пуле по умолчанию: скрипты короткие, больше 4 не нужно.
const DEFAULT_WORKERS: usize = 4;
/// Сколько обмен ждёт скрипт (включая очередь).
pub const DEFAULT_SCRIPT_TIMEOUT: Duration = Duration::from_secs(3);
/// Заданий в очереди на поток.
const QUEUE_PER_WORKER: usize = 16;
/// Потолок потоков вместе с заменами зависших (во столько раз больше).
const MAX_THREADS_FACTOR: usize = 4;
/// Стек потока: JSON-обход boa рекурсивный, глубину ограничивает прелюдия.
const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

struct Job {
    call: ScriptCall,
    reply: oneshot::Sender<ScriptRun>,
}

struct Pool {
    queue: async_channel::Receiver<Job>,
    shared: Arc<SharedState>,
    /// Целевое число потоков.
    size: usize,
    /// Живых потоков (с заменами).
    live: AtomicUsize,
}

pub struct BoaScriptRuntime {
    jobs: async_channel::Sender<Job>,
    pool: Arc<Pool>,
    timeout: Duration,
}

impl BoaScriptRuntime {
    /// Пул по числу ядер (не больше 4) с таймаутом по умолчанию.
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        Self::with(cores.min(DEFAULT_WORKERS), DEFAULT_SCRIPT_TIMEOUT)
    }

    pub fn with(workers: usize, timeout: Duration) -> Self {
        let size = workers.max(1);
        let (jobs, queue) = async_channel::bounded::<Job>(size * QUEUE_PER_WORKER);
        let pool = Arc::new(Pool {
            queue,
            shared: Arc::new(SharedState::default()),
            size,
            live: AtomicUsize::new(0),
        });
        for _ in 0..size {
            spawn_worker(&pool);
        }
        Self {
            jobs,
            pool,
            timeout,
        }
    }

    /// Ожидающий не дождался: возможно, поток занят зависшим скриптом —
    /// добавить замену (в пределах потолка).
    fn replace_stuck_worker(&self) {
        if self.pool.live.load(Ordering::SeqCst) < self.pool.size * MAX_THREADS_FACTOR {
            spawn_worker(&self.pool);
        }
    }
}

impl Default for BoaScriptRuntime {
    fn default() -> Self {
        Self::new()
    }
}

fn spawn_worker(pool: &Arc<Pool>) {
    let index = pool.live.fetch_add(1, Ordering::SeqCst);
    let thread_pool = Arc::clone(pool);
    let spawned = std::thread::Builder::new()
        .name(format!("crabtrap-script-{index}"))
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || serve(&thread_pool));
    if let Err(error) = spawned {
        pool.live.fetch_sub(1, Ordering::SeqCst);
        tracing::error!(%error, "cannot start script worker thread");
    }
}

/// Цикл потока: свой контекст, задания из очереди до её закрытия.
fn serve(pool: &Pool) {
    let mut worker: Option<Worker> = None;
    while let Ok(job) = pool.queue.recv_blocking() {
        // Ожидающий уже ушёл (таймаут) — результат никому не нужен.
        if job.reply.is_canceled() {
            continue;
        }
        let run = match worker.as_mut() {
            Some(worker) => run_guarded(worker, &job.call, &pool.shared),
            None => match Worker::new() {
                Ok(mut fresh) => {
                    let run = run_guarded(&mut fresh, &job.call, &pool.shared);
                    worker = Some(fresh);
                    run
                }
                Err(error) => Some(failed(error)),
            },
        };
        // Паника оставила контекст в неизвестном состоянии — пересоздать.
        if run.is_none() {
            worker = None;
        }
        let _ = job
            .reply
            .send(run.unwrap_or_else(|| failed("script engine crashed".into())));
        if leave_if_surplus(pool) {
            return;
        }
    }
    pool.live.fetch_sub(1, Ordering::SeqCst);
}

/// Потоков больше целевого (были замены) — этот выходит.
fn leave_if_surplus(pool: &Pool) -> bool {
    pool.live
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |live| {
            (live > pool.size).then(|| live - 1)
        })
        .is_ok()
}

fn run_guarded(worker: &mut Worker, call: &ScriptCall, shared: &SharedState) -> Option<ScriptRun> {
    std::panic::catch_unwind(AssertUnwindSafe(|| worker.run(call, shared))).ok()
}

fn failed(message: String) -> ScriptRun {
    ScriptRun {
        output: Err(message.clone()),
        logs: vec![ScriptLogDto {
            level: ScriptLogLevel::Error,
            message,
        }],
    }
}

#[async_trait]
impl ScriptRuntime for BoaScriptRuntime {
    async fn run(&self, call: ScriptCall) -> ScriptRun {
        let (reply, answer) = oneshot::channel();
        match self.jobs.try_send(Job { call, reply }) {
            Ok(()) => {}
            Err(async_channel::TrySendError::Full(_)) => {
                return failed(
                    "script queue is full (scripts are too slow), exchange passed unchanged".into(),
                );
            }
            Err(async_channel::TrySendError::Closed(_)) => {
                return failed("script workers are not running".into());
            }
        }
        // Вне tokio (пробный запуск без рантайма) — без таймаута.
        if tokio::runtime::Handle::try_current().is_err() {
            return answer
                .await
                .unwrap_or_else(|_| failed("script engine crashed".into()));
        }
        match tokio::time::timeout(self.timeout, answer).await {
            Ok(Ok(run)) => run,
            Ok(Err(_)) => failed("script engine crashed".into()),
            Err(_) => {
                self.replace_stuck_worker();
                failed(format!(
                    "script timed out after {} ms, exchange passed unchanged",
                    self.timeout.as_millis()
                ))
            }
        }
    }

    fn clear_shared_state(&self) {
        self.pool.shared.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use application::dto::scripting::{ScriptOutput, ScriptRequestDto, ScriptSourceDto};
    use domain::value_objects::RuleId;

    fn call(source: &str) -> ScriptCall {
        ScriptCall {
            script: ScriptSourceDto {
                rule_id: RuleId::new(),
                name: "t".into(),
                source: source.into(),
            },
            exchange_id: None,
            request: ScriptRequestDto {
                method: "GET".into(),
                url: "http://a.test/".into(),
                headers: Vec::new(),
                body: None,
            },
            response: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_scripts_on_pool_threads() {
        let runtime = BoaScriptRuntime::with(2, Duration::from_secs(5));
        let run = runtime
            .run(call(
                "function onRequest(c,u,r){ r.method = 'POST'; return r; }",
            ))
            .await;
        let Ok(ScriptOutput::Request(request)) = run.output else {
            panic!("{run:?}")
        };
        assert_eq!(request.method, "POST");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slow_script_times_out_and_pool_recovers() {
        let runtime = BoaScriptRuntime::with(1, Duration::from_millis(50));
        let slow = "function onRequest(c,u,r){ let x = 0; for (let i = 0; i < 9000000; i++) { x += i; } return r; }";
        let run = runtime.run(call(slow)).await;
        assert!(run.output.unwrap_err().contains("timed out"));

        let fast = BoaScriptRuntime::with(1, Duration::from_secs(30));
        assert!(
            fast.run(call("function onRequest(c,u,r){ return r; }"))
                .await
                .output
                .is_ok()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stuck_worker_is_replaced_and_pool_keeps_serving() {
        let runtime = BoaScriptRuntime::with(1, Duration::from_millis(100));
        // Вызов функции на каждой итерации: счётчик цикла сбрасывается на
        // каждый кадр, лимит итераций такой скрипт быстро не оборвёт.
        let stuck = "function spin(){ for (let i = 0; i < 9000000; i++) {} } \
                     function onRequest(c,u,r){ for (let j = 0; j < 1000; j++) spin(); return r; }";
        assert!(
            runtime
                .run(call(stuck))
                .await
                .output
                .unwrap_err()
                .contains("timed out")
        );
        let fast = call("function onRequest(c,u,r){ r.method = 'PUT'; return r; }");
        let tokio_timeout = Duration::from_secs(1);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let run = tokio::time::timeout(tokio_timeout, runtime.run(fast.clone()))
                .await
                .expect("answered");
            if run.output.is_ok() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "pool never recovered: {run:?}"
            );
        }
    }
}
