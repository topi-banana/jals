//! The sequential executor: the semantic baseline every other runtime must match.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::yields::{block_on_inline, fan_out_sequential, yield_now};
use crate::{ErasedJob, ErasedOutcome, LocalBoxFuture, RawExec, private};

pub(crate) struct InlineExec;

impl private::Sealed for InlineExec {}

impl RawExec for InlineExec {
    fn name(&self) -> &'static str {
        "inline"
    }

    fn yield_boxed(&self) -> LocalBoxFuture<'static, ()> {
        Box::pin(yield_now())
    }

    /// Inline spawn is synchronous: the task is driven to completion before this returns.
    /// Inline hosts (tests, pure in-memory storage) never wait on external events, so eager
    /// completion is both deterministic and deadlock-free.
    fn spawn_boxed(&self, fut: LocalBoxFuture<'static, ()>) {
        block_on_inline(fut);
    }

    fn fan_out_boxed(&self, jobs: Vec<ErasedJob>) -> LocalBoxFuture<'static, Vec<ErasedOutcome>> {
        Box::pin(fan_out_sequential(jobs))
    }
}
