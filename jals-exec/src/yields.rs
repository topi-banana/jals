//! Runtime-free cooperation primitives.
//!
//! These are free functions on purpose: parsing, inference, digesting, and every other portable
//! hot loop cooperates through [`yield_now`]/[`Yielder`] without ever holding an [`Exec`]
//! handle, so CPU crates take no execution parameter at all.
//!
//! [`Exec`]: crate::Exec

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

// `YieldNow` is nameable through `yield_now`'s return type; re-export the rest.
#[cfg(test)]
pub(crate) use api::YieldNow;
pub use api::{Yielder, block_on_inline, join_ordered, yield_now};

/// The free-function surface, grouped per the repository's no-free-functions layout; `lib.rs`
/// re-exports these at the crate root.
mod api {
    use super::{Box, Context, Future, Pin, Poll, Vec, Waker};

    /// Yields once: wakes itself and returns `Pending` a single time, sending the task to the
    /// back of the current executor's queue.
    pub const fn yield_now() -> YieldNow {
        YieldNow { yielded: false }
    }

    /// Future returned by [`yield_now`].
    #[must_use = "futures do nothing unless awaited"]
    pub struct YieldNow {
        yielded: bool,
    }

    impl Future for YieldNow {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.yielded {
                Poll::Ready(())
            } else {
                self.yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    /// Amortized cooperative point for hot loops.
    ///
    /// [`tick`](Self::tick) is a counter decrement on all but every `period`-th call, where it
    /// performs one real [`yield_now`]; awaits are nearly-free ready polls except at yield points.
    #[derive(Debug)]
    pub struct Yielder {
        left: u32,
        period: u32,
    }

    impl Yielder {
        /// Default period: cooperation roughly every 512 ticks.
        pub const DEFAULT_PERIOD: u32 = 512;

        pub const fn new() -> Self {
            Self::every(Self::DEFAULT_PERIOD)
        }

        /// A yielder that yields on every `period`-th tick. A period of 0 is treated as 1.
        pub const fn every(period: u32) -> Self {
            let period = if period == 0 { 1 } else { period };
            Self {
                left: period,
                period,
            }
        }

        /// Counts one unit of work; yields once per period.
        #[inline]
        pub async fn tick(&mut self) {
            self.left -= 1;
            if self.left == 0 {
                self.left = self.period;
                yield_now().await;
            }
        }
    }

    impl Default for Yielder {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Drives all futures concurrently on the current task and returns their outputs in input
    /// order.
    ///
    /// This is single-thread concurrency for `!Send` futures — the right shape for overlapping
    /// waits (network fetches); CPU parallelism is [`Exec::fan_out`](crate::Exec::fan_out).
    pub async fn join_ordered<T, F>(futures: impl IntoIterator<Item = F>) -> Vec<T>
    where
        F: Future<Output = T>,
    {
        let mut slots: Vec<Option<Pin<Box<F>>>> = futures
            .into_iter()
            .map(|future| Some(Box::pin(future)))
            .collect();
        let mut results: Vec<Option<T>> = slots.iter().map(|_| None).collect();
        let mut remaining = slots.len();

        core::future::poll_fn(move |cx| {
            for (slot, result) in slots.iter_mut().zip(results.iter_mut()) {
                if let Some(future) = slot
                    && let Poll::Ready(value) = future.as_mut().poll(cx)
                {
                    *result = Some(value);
                    *slot = None;
                    remaining -= 1;
                }
            }
            if remaining == 0 {
                let outputs = results
                    .iter_mut()
                    .map(|result| result.take().expect("every join_ordered slot is filled"))
                    .collect();
                Poll::Ready(outputs)
            } else {
                Poll::Pending
            }
        })
        .await
    }

    /// Drives a future to completion by polling in a spin loop with a no-op waker.
    ///
    /// Correct for the workspace's ready-poll futures (inline yields make progress on every
    /// poll); a future waiting on a genuinely external event would spin. Hosts have real
    /// runtimes — this exists for tests and for the inline executor itself.
    pub fn block_on_inline<T>(future: impl Future<Output = T>) -> T {
        let mut future = core::pin::pin!(future);
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(value) => return value,
                Poll::Pending => core::hint::spin_loop(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yield_now_is_pending_exactly_once() {
        let mut polls = 0u32;
        block_on_inline(async {
            let mut yield_once = core::pin::pin!(yield_now());
            core::future::poll_fn(|cx| {
                polls += 1;
                yield_once.as_mut().poll(cx)
            })
            .await;
        });
        assert_eq!(polls, 2);
    }

    #[test]
    fn yielder_yields_on_period_boundaries() {
        block_on_inline(async {
            let mut yielder = Yielder::every(3);
            for _ in 0..9 {
                yielder.tick().await;
            }
        });
    }

    #[test]
    fn join_ordered_returns_outputs_in_input_order() {
        let outputs = block_on_inline(join_ordered((0..5u32).map(|n| async move {
            // Stagger readiness so later futures finish first.
            for _ in 0..(5 - n) {
                yield_now().await;
            }
            n
        })));
        assert_eq!(outputs, alloc::vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn join_ordered_of_nothing_is_empty() {
        let outputs: Vec<()> = block_on_inline(join_ordered(Vec::<YieldNow>::new()));
        assert!(outputs.is_empty());
    }
}
