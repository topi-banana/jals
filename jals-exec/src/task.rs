//! The spawned-task handle.

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use crate::LocalBoxFuture;

enum Slot<T> {
    Running {
        waker: Option<Waker>,
    },
    Done(T),
    Taken,
    /// The task future was dropped (cancelled or unwound) before producing a value.
    Gone,
}

struct TaskState<T> {
    slot: RefCell<Slot<T>>,
}

impl<T> TaskState<T> {
    fn finish(&self, terminal: Slot<T>) {
        let previous = self.slot.replace(terminal);
        if let Slot::Running { waker: Some(waker) } = previous {
            waker.wake();
        }
    }
}

/// Arms the terminal state: completion defuses it; a drop while armed (cancellation or panic
/// unwind of the task future) records [`Slot::Gone`] so an awaiting [`Task`] fails loudly
/// instead of hanging.
struct CompletionGuard<T> {
    state: Option<Rc<TaskState<T>>>,
}

impl<T> CompletionGuard<T> {
    fn complete(mut self, value: T) {
        if let Some(state) = self.state.take() {
            state.finish(Slot::Done(value));
        }
    }
}

impl<T> Drop for CompletionGuard<T> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            state.finish(Slot::Gone);
        }
    }
}

/// Handle to a task started with [`Exec::spawn`](crate::Exec::spawn).
///
/// Dropping the handle detaches the task (it keeps running); awaiting it yields the task's
/// output.
///
/// # Panics
///
/// Awaiting a task whose future was cancelled or panicked panics on the awaiting side.
#[must_use = "dropping a Task detaches it; await it to observe the output"]
pub struct Task<T> {
    state: Rc<TaskState<T>>,
}

impl<T: 'static> Task<T> {
    /// Pairs a fresh handle with the unit future handed to the runtime.
    pub(crate) fn wrap(
        fut: impl Future<Output = T> + 'static,
    ) -> (Self, LocalBoxFuture<'static, ()>) {
        let state = Rc::new(TaskState {
            slot: RefCell::new(Slot::Running { waker: None }),
        });
        let guard = CompletionGuard {
            state: Some(Rc::clone(&state)),
        };
        let wrapped = Box::pin(async move {
            let value = fut.await;
            guard.complete(value);
        });
        (Self { state }, wrapped)
    }
}

impl<T> Future for Task<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let mut slot = self.state.slot.borrow_mut();
        match &mut *slot {
            Slot::Running { waker } => {
                *waker = Some(cx.waker().clone());
                Poll::Pending
            }
            Slot::Done(_) => {
                let Slot::Done(value) = core::mem::replace(&mut *slot, Slot::Taken) else {
                    unreachable!()
                };
                Poll::Ready(value)
            }
            Slot::Taken => panic!("Task polled after completion"),
            Slot::Gone => panic!("awaited task terminated without completing"),
        }
    }
}
