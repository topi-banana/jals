#![cfg_attr(not(test), no_std)]
//! What a run is doing, as data.
//!
//! Long work in this workspace happens in portable crates — a jar is fetched by `jals-classpath`,
//! remapped and decompiled there too, a task plan is executed by `jals-project`, a tree is compiled
//! by `jals-build` — while the only thing that can draw a progress bar is the host that started
//! them. This crate is the seam between the two, and it is deliberately small: an emitter says
//! *what work it is doing*, and a consumer decides what that looks like.
//!
//! Three properties are load-bearing.
//!
//! - **Facts, not presentation.** [`Activity`] is `Fetch`, never "Downloading"; [`Outcome`] is
//!   `Fresh`, never a colour. The terminal's verbs live in `jals-cli`, exactly as `jals-hir` states
//!   a fact and the `jals-lint` rule that reports it owns the wording. The one concession is
//!   `Activity::label` — crate-internal, and there only because a written report has to put some
//!   word on a row.
//! - **Silent by default and free when silent.** [`Progress::SILENT`] allocates nothing, and every
//!   method on it is one branch. That is what makes it threadable through code a test, the browser,
//!   or the language server drives with nobody watching.
//! - **No clock.** Portable code here cannot read one. A host stamps events as they arrive and
//!   hands the number to [`Timeline::record`]; everything this crate reports about time is
//!   arithmetic on numbers it was given. `cargo`'s own `--timings` records host-side for the same
//!   reason.
//!
//! ```
//! use std::sync::Arc;
//! use std::sync::atomic::{AtomicUsize, Ordering};
//!
//! use jals_progress::{Activity, Event, Outcome, Progress, Sink};
//!
//! struct Count(AtomicUsize);
//! impl Sink for Count {
//!     fn emit(&self, _: &Event) {
//!         self.0.fetch_add(1, Ordering::Relaxed);
//!     }
//! }
//!
//! let sink = Arc::new(Count(AtomicUsize::new(0)));
//! let progress = Progress::to(sink.clone());
//! let task = progress.begin_bounded(Activity::Fetch, "client.jar", 1024);
//! task.advance(1024);
//! task.finish(Outcome::Completed);
//! // started, advanced, finished
//! assert_eq!(sink.0.load(Ordering::Relaxed), 3);
//! ```

extern crate alloc;

mod event;
mod handle;
mod html;
mod timeline;

#[cfg(test)]
mod tests;

pub use event::{Activity, Event, Outcome, PackageRef, Unit, UnitId};
pub use handle::{Progress, Sink, Task, Ticker};
pub use timeline::{ReportMeta, Timeline};
