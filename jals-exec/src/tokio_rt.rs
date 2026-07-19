//! The native runtime adapter: a current-thread tokio runtime plus a persistent worker-thread
//! pool backing [`Exec::fan_out`].
//!
//! [`run`] is the single bootstrap entry point for native binaries (`jals-cli`, `jals-lsp`):
//! one runtime, one `LocalSet`, one [`Exec`] handle threaded down from the top.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::OnceCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, mpsc};

use crate::{ErasedJob, ErasedOutcome, Exec, LocalBoxFuture, RawExec, private};

impl Exec {
    /// An [`Exec`] backed by the ambient tokio current-thread runtime.
    ///
    /// `spawn` requires a `LocalSet` context and `fan_out` result collection requires a tokio
    /// reactor — construct through [`run`] unless embedding in an existing runtime that
    /// guarantees both.
    pub fn tokio() -> Self {
        Self(Rc::new(TokioExec {
            pool: OnceCell::new(),
        }))
    }
}

pub use api::{on_blocking_pool, run};

/// The free-function surface, grouped per the repository's no-free-functions layout; re-exported
/// at the module root.
mod api {
    use super::{Exec, Future};

    /// Run a blocking host closure off the executor.
    ///
    /// On the tokio runtime the closure moves to the blocking pool so the current-thread
    /// executor keeps serving tasks; without a runtime (inline-executor tests, fan-out worker
    /// threads, which are blocking-legal by design) it runs on the calling thread. Native
    /// adapters use this for every blocking syscall batch.
    pub async fn on_blocking_pool<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => match handle.spawn_blocking(f).await {
                Ok(value) => value,
                Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
                Err(error) => panic!("blocking host task failed: {error}"),
            },
            Err(_) => f(),
        }
    }

    /// Builds a current-thread tokio runtime and a `LocalSet`, hands the program an [`Exec`],
    /// and blocks until the returned future completes.
    pub fn run<T, F, Fut>(f: F) -> std::io::Result<T>
    where
        F: FnOnce(Exec) -> Fut,
        Fut: Future<Output = T>,
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let local = tokio::task::LocalSet::new();
        Ok(local.block_on(&runtime, f(Exec::tokio())))
    }
}

struct WorkOrder {
    index: usize,
    job: ErasedJob,
    reply: tokio::sync::mpsc::UnboundedSender<(usize, ErasedOutcome)>,
}

struct TokioExec {
    /// Lazily-started pool; dropping the handle closes the channel and the workers exit.
    pool: OnceCell<mpsc::Sender<WorkOrder>>,
}

impl TokioExec {
    fn pool(&self) -> &mpsc::Sender<WorkOrder> {
        self.pool.get_or_init(|| {
            let (sender, receiver) = mpsc::channel::<WorkOrder>();
            let receiver = Arc::new(Mutex::new(receiver));
            let workers = std::thread::available_parallelism().map_or(4, usize::from);
            for n in 0..workers {
                let receiver = Arc::clone(&receiver);
                std::thread::Builder::new()
                    .name(std::format!("jals-exec-worker-{n}"))
                    .spawn(move || Self::worker_loop(&receiver))
                    .expect("failed to spawn jals-exec fan-out worker");
            }
            sender
        })
    }
}

struct ThreadWaker(std::thread::Thread);

impl std::task::Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

impl TokioExec {
    fn worker_loop(receiver: &Mutex<mpsc::Receiver<WorkOrder>>) {
        loop {
            // Hold the lock only for the receive; the job itself runs unlocked so other workers
            // can pick up orders concurrently.
            let order = {
                let receiver = receiver.lock().expect("fan-out receiver lock poisoned");
                receiver.recv()
            };
            let Ok(order) = order else {
                // Channel closed: the owning `Exec` is gone.
                break;
            };
            let outcome =
                std::panic::catch_unwind(AssertUnwindSafe(|| Self::worker_block_on((order.job)())));
            // A dropped fan-out future closes the reply channel; the result is discarded.
            let _ = order.reply.send((order.index, outcome));
        }
    }

    /// Drives one job future to completion on the worker thread with a parking waker. The
    /// future is created on this thread and never leaves it — this is where `!Send` futures
    /// meet real parallelism.
    fn worker_block_on<T>(mut future: Pin<Box<dyn Future<Output = T> + '_>>) -> T {
        let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::park(),
            }
        }
    }
}

impl private::Sealed for TokioExec {}

impl RawExec for TokioExec {
    fn name(&self) -> &'static str {
        "tokio"
    }

    fn yield_boxed(&self) -> LocalBoxFuture<'static, ()> {
        Box::pin(tokio::task::yield_now())
    }

    fn spawn_boxed(&self, fut: LocalBoxFuture<'static, ()>) {
        drop(tokio::task::spawn_local(fut));
    }

    fn fan_out_boxed(&self, jobs: Vec<ErasedJob>) -> LocalBoxFuture<'static, Vec<ErasedOutcome>> {
        let total = jobs.len();
        let (reply, mut outcomes) = tokio::sync::mpsc::unbounded_channel();
        // Dispatch eagerly: workers may start (and even finish) before the future is polled.
        for (index, job) in jobs.into_iter().enumerate() {
            let order = WorkOrder {
                index,
                job,
                reply: reply.clone(),
            };
            self.pool()
                .send(order)
                .expect("fan-out worker pool is closed");
        }
        drop(reply);
        Box::pin(async move {
            let mut slots: Vec<Option<ErasedOutcome>> = (0..total).map(|_| None).collect();
            let mut received = 0;
            while received < total {
                let (index, outcome) = outcomes
                    .recv()
                    .await
                    .expect("fan-out worker exited without replying");
                slots[index] = Some(outcome);
                received += 1;
            }
            slots
                .into_iter()
                .map(|slot| slot.expect("fan-out reply indexes are unique"))
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yield_now;

    #[test]
    fn fan_out_runs_on_workers_and_preserves_order() {
        let outputs = run(|exec| async move {
            exec.fan_out(0..32u32, |n| async move {
                yield_now().await;
                let on_worker = std::thread::current()
                    .name()
                    .is_some_and(|name| name.starts_with("jals-exec-worker"));
                (n, on_worker)
            })
            .await
        })
        .expect("runtime bootstraps");
        let values: Vec<u32> = outputs.iter().map(|(n, _)| *n).collect();
        assert_eq!(values, (0..32).collect::<Vec<_>>());
        assert!(outputs.iter().all(|(_, on_worker)| *on_worker));
    }

    #[test]
    fn fan_out_resumes_the_first_job_panic_after_all_jobs_settle() {
        let caught = std::panic::catch_unwind(|| {
            run(|exec| async move {
                exec.fan_out(0..4u32, |n| async move {
                    assert_ne!(n, 2, "job 2 fails deterministically");
                    n
                })
                .await
            })
        });
        assert!(caught.is_err());
    }

    #[test]
    fn spawned_tasks_complete_and_yield_their_output() {
        let value = run(|exec| async move {
            let task = exec.spawn(async {
                yield_now().await;
                21u32
            });
            task.await * 2
        })
        .expect("runtime bootstraps");
        assert_eq!(value, 42);
    }

    #[test]
    fn empty_fan_out_returns_immediately() {
        let outputs = run(|exec| async move { exec.fan_out(0..0u32, |n| async move { n }).await })
            .expect("runtime bootstraps");
        assert!(outputs.is_empty());
    }
}
