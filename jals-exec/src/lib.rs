#![cfg_attr(not(any(feature = "std", test)), no_std)]
//! The unified execution context for the workspace.
//!
//! Every future in the workspace is deliberately `!Send`: runtimes are current-thread on every
//! platform, and multi-core parallelism exists only as [`Exec::fan_out`] — `Send` inputs are
//! distributed to workers that each build and drive a `!Send` future locally, so no future ever
//! crosses a thread.
//!
//! Cooperative yielding needs no context at all: [`yield_now`] and the amortized [`Yielder`] are
//! free functions usable from any crate, on any executor. Only code that spawns tasks or fans
//! work out holds an [`Exec`] handle.
//!
//! Runtime selection happens at the top of the program: hosts build an [`Exec`] from the adapter
//! they own (`tokio_rt::run` natively, `Exec::wasm()` in the browser, [`Exec::inline`] for tests
//! and pure in-memory use) and thread the handle down; portable code never names a runtime.

mod inline;
mod task;
#[cfg(any(feature = "tokio", test))]
pub mod tokio_rt;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod wasm;
mod yields;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::future::Future;
use core::pin::Pin;

pub use task::Task;
pub use yields::{Yielder, block_on_inline, join_ordered, yield_now};

/// A boxed, thread-local future. The workspace-wide future shape: nothing here is `Send`.
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// A type-erased fan-out job. The closure is `Send` and crosses to a worker thread; the future
/// it returns is `!Send` and is created and driven entirely on that worker.
type ErasedJob = Box<dyn FnOnce() -> LocalBoxFuture<'static, Box<dyn Any + Send>> + Send>;

/// `Ok(result)` or `Err(panic payload)`; payloads are resumed on the caller once all jobs settle.
type ErasedOutcome = Result<Box<dyn Any + Send>, Box<dyn Any + Send>>;

mod private {
    pub trait Sealed {}
}

/// Object-safe execution core. Closed contract: only the inline, tokio, and wasm runtimes in
/// this crate implement it — consumers program against [`Exec`].
#[doc(hidden)]
pub trait RawExec: private::Sealed {
    /// Runtime name for `Debug` output.
    fn name(&self) -> &'static str;
    fn yield_boxed(&self) -> LocalBoxFuture<'static, ()>;
    fn spawn_boxed(&self, fut: LocalBoxFuture<'static, ()>);
    /// Ordered fan-out over erased jobs: the outcome vector matches the job vector index for
    /// index, regardless of completion order.
    fn fan_out_boxed(&self, jobs: Vec<ErasedJob>) -> LocalBoxFuture<'static, Vec<ErasedOutcome>>;
}

/// Cheap-to-clone, `!Send` execution context handle.
///
/// Long-lived aggregates (`ProjectStorage`, `Workspace`, the LSP server state) store one;
/// free-standing pipelines take `&Exec`.
#[derive(Clone)]
pub struct Exec(Rc<dyn RawExec>);

impl core::fmt::Debug for Exec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Exec({})", self.0.name())
    }
}

impl Exec {
    /// The sequential executor: yields are single self-wakes, `spawn` drives the task to
    /// completion synchronously, and `fan_out` awaits items in order on the calling task.
    ///
    /// This is the default for tests and pure in-memory hosts, and the semantics `fan_out`
    /// results are defined against: every other runtime must produce identical output.
    pub fn inline() -> Self {
        Self(Rc::new(inline::InlineExec))
    }

    /// The browser executor: `spawn` is `wasm_bindgen_futures::spawn_local`, `fan_out` is
    /// sequential, and yields periodically escape to the macrotask queue so the page can paint.
    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    pub fn wasm() -> Self {
        Self(Rc::new(wasm::WasmExec::new()))
    }

    /// Cooperative yield through the runtime (participates in tokio's task budget natively).
    /// Hot loops should prefer the free [`Yielder`], which needs no handle.
    pub async fn yield_now(&self) {
        self.0.yield_boxed().await;
    }

    /// Spawns a detached local task. Dropping the returned [`Task`] detaches it; awaiting the
    /// [`Task`] yields the future's output.
    ///
    /// On the inline executor the future is driven to completion before `spawn` returns.
    pub fn spawn<T: 'static>(&self, fut: impl Future<Output = T> + 'static) -> Task<T> {
        let (task, wrapped) = Task::wrap(fut);
        self.0.spawn_boxed(wrapped);
        task
    }

    /// Ordered fan-out map: applies `f` to every item and returns the results in input order,
    /// regardless of completion order — output is byte-identical across runtimes and worker
    /// counts.
    ///
    /// Inputs and the closure are `Send`; the futures `f` builds are `!Send` and each is created
    /// and driven entirely on one worker. Workers are dedicated threads on the native runtime,
    /// so blocking directly inside a job is legal there; the inline and wasm executors await the
    /// jobs sequentially on the calling task.
    ///
    /// If a job panics, the first panic (in input order) is resumed on the caller after all jobs
    /// settle. Dropping the returned future lets already-queued jobs run to completion and
    /// discard their results.
    pub async fn fan_out<T, R, F, Fut>(&self, items: impl IntoIterator<Item = T>, f: F) -> Vec<R>
    where
        T: Send + 'static,
        R: Send + 'static,
        F: Fn(T) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = R> + 'static,
    {
        let f = Arc::new(f);
        let jobs: Vec<ErasedJob> = items
            .into_iter()
            .map(|item| {
                let f = Arc::clone(&f);
                let job: ErasedJob = Box::new(move || {
                    Box::pin(async move {
                        let out: Box<dyn Any + Send> = Box::new(f(item).await);
                        out
                    })
                });
                job
            })
            .collect();
        let outcomes = self.0.fan_out_boxed(jobs).await;

        let mut results = Vec::with_capacity(outcomes.len());
        let mut first_panic: Option<Box<dyn Any + Send>> = None;
        for outcome in outcomes {
            match outcome {
                Ok(value) => results.push(
                    *value
                        .downcast::<R>()
                        .expect("fan-out job produced a foreign type"),
                ),
                Err(payload) => {
                    if first_panic.is_none() {
                        first_panic = Some(payload);
                    }
                }
            }
        }
        if let Some(payload) = first_panic {
            resume_panic(payload);
        }
        results
    }
}

#[cfg(any(feature = "std", test))]
fn resume_panic(payload: Box<dyn Any + Send>) -> ! {
    std::panic::resume_unwind(payload)
}

/// Without `std` no runtime catches job panics, so a payload can never reach this point; the
/// message exists only to keep the signature total.
#[cfg(not(any(feature = "std", test)))]
fn resume_panic(_payload: Box<dyn Any + Send>) -> ! {
    panic!("fan-out job panicked")
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    #[test]
    fn inline_fan_out_preserves_input_order() {
        let exec = Exec::inline();
        let doubled = block_on_inline(exec.fan_out(0..8usize, |n| async move { n * 2 }));
        assert_eq!(doubled, alloc::vec![0, 2, 4, 6, 8, 10, 12, 14]);
    }

    #[test]
    fn inline_spawn_completes_synchronously_and_task_returns_output() {
        let exec = Exec::inline();
        let task = exec.spawn(async {
            yield_now().await;
            7u32
        });
        assert_eq!(block_on_inline(task), 7);
    }

    #[test]
    fn fan_out_jobs_observe_yields() {
        let exec = Exec::inline();
        let outputs = block_on_inline(exec.fan_out(0..3u32, |n| async move {
            let mut yielder = Yielder::every(1);
            yielder.tick().await;
            n + 1
        }));
        assert_eq!(outputs, alloc::vec![1, 2, 3]);
    }

    #[test]
    fn debug_names_the_runtime() {
        assert_eq!(alloc::format!("{:?}", Exec::inline()), "Exec(inline)");
    }

    #[test]
    fn dropping_a_task_detaches_without_panicking() {
        let exec = Exec::inline();
        let ran = Rc::new(Cell::new(false));
        let flag = Rc::clone(&ran);
        drop(exec.spawn(async move { flag.set(true) }));
        assert!(ran.get(), "inline spawn drives the task eagerly");
    }
}
