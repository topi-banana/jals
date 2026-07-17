//! The browser runtime adapter.
//!
//! `spawn` rides `wasm_bindgen_futures::spawn_local`; `fan_out` degrades to the sequential
//! inline shape (the browser is single-threaded); yields escape to the macrotask queue every
//! [`MACROTASK_PERIOD`]-th time so long computations let the page paint — pure self-wake yields
//! would only bounce on the microtask queue and never yield the event loop.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::Cell;

use crate::yields::yield_now;
use crate::{ErasedJob, ErasedOutcome, LocalBoxFuture, RawExec, private};

const MACROTASK_PERIOD: u32 = 64;

pub(crate) struct WasmExec {
    until_macrotask: Cell<u32>,
}

impl WasmExec {
    pub(crate) fn new() -> Self {
        Self {
            until_macrotask: Cell::new(MACROTASK_PERIOD),
        }
    }
}

impl private::Sealed for WasmExec {}

impl RawExec for WasmExec {
    fn name(&self) -> &'static str {
        "wasm"
    }

    fn yield_boxed(&self) -> LocalBoxFuture<'static, ()> {
        let left = self.until_macrotask.get() - 1;
        if left == 0 {
            self.until_macrotask.set(MACROTASK_PERIOD);
            Box::pin(gloo_timers::future::TimeoutFuture::new(0))
        } else {
            self.until_macrotask.set(left);
            Box::pin(yield_now())
        }
    }

    fn spawn_boxed(&self, fut: LocalBoxFuture<'static, ()>) {
        wasm_bindgen_futures::spawn_local(fut);
    }

    fn fan_out_boxed(&self, jobs: Vec<ErasedJob>) -> LocalBoxFuture<'static, Vec<ErasedOutcome>> {
        Box::pin(async move {
            let mut outcomes = Vec::with_capacity(jobs.len());
            for job in jobs {
                outcomes.push(Ok(job().await));
                yield_now().await;
            }
            outcomes
        })
    }
}
