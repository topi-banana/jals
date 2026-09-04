//! The emitter's half: a cheap-clone handle, the sink it writes to, and the RAII task that keeps
//! the two ends honest.

use alloc::{string::String, sync::Arc};
use core::{
    cell::Cell,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::event::{Activity, Event, Outcome, PackageRef, Unit, UnitId};

/// Where events go.
///
/// `Send + Sync` because an emitter crosses `Exec::fan_out`: `jals-classpath` decodes jar members
/// and remaps classes on worker threads, and those are exactly the phases long enough to want a
/// bar. It is the same boundary, for the same reason, as `jals_build::TestLauncher::run`'s
/// `Arc<dyn Fn(TestEvent) + Send + Sync>` — and it is the *only* reason the bound is here, since
/// every other runtime in this workspace is deliberately current-thread.
///
/// Implementations must not block: an emitter calls `emit` from inside the work it is describing.
pub trait Sink: Send + Sync {
    /// Handle one event. Called in emission order on whichever thread emitted it, so a sink that
    /// draws has to serialize itself.
    fn emit(&self, event: &Event);
}

/// The sink plus the run's id counter — one allocation shared by every clone of a [`Progress`].
struct Core {
    sink: Arc<dyn Sink>,
    next: AtomicUsize,
}

/// A cheap-clone handle an emitter holds.
///
/// Silent by default, which is what makes it free to thread through portable code that a test, the
/// browser, or the language server drives with nobody watching: [`Progress::SILENT`] allocates
/// nothing and every method on it is a branch.
///
/// It is a value rather than something hung off `Exec` on purpose. `Exec` is `!Send`, so it cannot
/// reach the fan-out workers this is most worth reporting from; and CPU crates in this workspace
/// take no execution parameter at all, so tying reporting to `Exec` would deny it to exactly the
/// crates that will want it next.
#[derive(Clone, Default)]
pub struct Progress {
    core: Option<Arc<Core>>,
    package: Option<Arc<PackageRef>>,
}

impl Progress {
    /// A handle nobody is watching. Every method is a branch and no event is built.
    pub const SILENT: Self = Self {
        core: None,
        package: None,
    };

    /// A handle that reports to `sink`.
    ///
    /// The caller keeps its own `Arc` when it needs to read the sink afterwards — which is how
    /// `--timings` gets its ledger back at the end of a run.
    #[must_use]
    pub fn to(sink: Arc<dyn Sink>) -> Self {
        Self {
            core: Some(Arc::new(Core {
                sink,
                next: AtomicUsize::new(0),
            })),
            package: None,
        }
    }

    /// A handle whose units are attributed to `package`.
    ///
    /// Attribution is set once, where the package is known — the graph's per-node loop, the root
    /// command — instead of at every `begin`. That is what keeps "which package is this for" from
    /// being re-answered, differently, at each of the dozen places that start work.
    #[must_use]
    pub fn for_package(&self, package: PackageRef) -> Self {
        Self {
            core: self.core.clone(),
            package: Some(Arc::new(package)),
        }
    }

    /// Whether anything is listening.
    ///
    /// Ask before building a subject a silent run would throw away — a loop that formats one
    /// string per archive member, say.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        self.core.is_some()
    }

    /// Begin a unit of work whose size is not known.
    pub fn begin(&self, activity: Activity, subject: impl Into<String>) -> Task {
        self.start(activity, subject, None)
    }

    /// Begin a unit of work that counts up to `total`.
    pub fn begin_bounded(
        &self,
        activity: Activity,
        subject: impl Into<String>,
        total: u64,
    ) -> Task {
        self.start(activity, subject, Some(total))
    }

    fn start(&self, activity: Activity, subject: impl Into<String>, total: Option<u64>) -> Task {
        let Some(core) = &self.core else {
            return Task::silent();
        };
        let id = UnitId::new(core.next.fetch_add(1, Ordering::Relaxed) as u64);
        let unit = Unit {
            package: self.package.as_deref().cloned(),
            activity,
            subject: subject.into(),
            total,
        };
        core.sink.emit(&Event::Started { id, unit });
        Task {
            handle: Some(TaskHandle {
                id,
                core: Arc::clone(core),
            }),
            done: Cell::new(0),
            total: Cell::new(total),
        }
    }

    /// Report a unit that began and ended in the same breath.
    ///
    /// For work whose whole duration is one call — a cache hit, a step a policy declined — where a
    /// [`Task`] would be created and finished on the next line.
    pub fn record(&self, activity: Activity, subject: impl Into<String>, outcome: Outcome) {
        self.start(activity, subject, None).finish(outcome);
    }
}

/// The half of a live [`Task`] that has somewhere to report to.
struct TaskHandle {
    id: UnitId,
    core: Arc<Core>,
}

/// One unit of work in flight.
///
/// Finish it explicitly. The `Drop` that reports [`Outcome::Abandoned`] is a net for the case
/// nobody meant to leave — an error path that returns without saying it failed — not the way a
/// failure is meant to be reported, because "abandoned" tells a reader the emitter has a hole in it
/// and "failed" tells them the build does.
#[must_use = "a task that is never finished reports as abandoned"]
pub struct Task {
    handle: Option<TaskHandle>,
    done: Cell<u64>,
    total: Cell<Option<u64>>,
}

impl Task {
    /// A task nobody is watching.
    const fn silent() -> Self {
        Self {
            handle: None,
            done: Cell::new(0),
            total: Cell::new(None),
        }
    }

    /// Add `amount` to what this unit has done.
    pub fn advance(&self, amount: u64) {
        let done = self.done.get().saturating_add(amount);
        self.done.set(done);
        self.emit_progress(done);
    }

    /// Set what this unit has done, for a producer that counts absolutely rather than by delta.
    pub fn set_done(&self, done: u64) {
        self.done.set(done);
        self.emit_progress(done);
    }

    /// Declare the total this unit counts up to, once it becomes known.
    ///
    /// A download learns its size from `Content-Length` after the request goes out, so the unit it
    /// started as a spinner becomes a bar here.
    pub fn set_total(&self, total: u64) {
        self.total.set(Some(total));
        self.emit_progress(self.done.get());
    }

    /// End this unit.
    pub fn finish(mut self, outcome: Outcome) {
        if let Some(handle) = self.handle.take() {
            handle.core.sink.emit(&Event::Finished {
                id: handle.id,
                outcome,
            });
        }
    }

    /// The unit's id, for an emitter that has to correlate something with it.
    #[must_use]
    pub fn id(&self) -> Option<UnitId> {
        self.handle.as_ref().map(|handle| handle.id)
    }

    fn emit_progress(&self, done: u64) {
        if let Some(handle) = &self.handle {
            handle.core.sink.emit(&Event::Advanced {
                id: handle.id,
                done,
                total: self.total.get(),
            });
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.core.sink.emit(&Event::Finished {
                id: handle.id,
                outcome: Outcome::Abandoned,
            });
        }
    }
}
