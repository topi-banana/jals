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

impl core::fmt::Debug for Progress {
    /// Says whether anything is listening and what the work is attributed to — never who is
    /// listening. A sink is a consumer's live terminal or its ledger, and printing one into the
    /// `{:?}` of some options struct would be neither meaningful nor bounded.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Progress")
            .field("live", &self.is_live())
            .field("package", &self.package.as_deref())
            .finish_non_exhaustive()
    }
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
    /// Crate-internal until an emitter has a loop hot enough to want it: the guard is for skipping
    /// a subject a silent run would throw away, and nothing formats one per archive member yet.
    const fn is_live(&self) -> bool {
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
            done: Arc::new(AtomicUsize::new(0)),
            total: Cell::new(total),
            ended: Cell::new(false),
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
#[derive(Clone)]
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
    /// Shared with every [`Ticker`] this task hands out, which is what lets a fan-out worker count
    /// into the same unit the main task started. `usize` rather than `u64` because
    /// `AtomicUsize` is the one width every target this workspace builds for has — including
    /// `wasm32`, where it is 32 bits and a count above 4 GiB saturates.
    done: Arc<AtomicUsize>,
    total: Cell<Option<u64>>,
    /// Whether this unit has already reported how it ended.
    ///
    /// A unit ends exactly once. The flag is what lets a step deep inside the work end its
    /// caller's unit as [`fresh`](Self::fresh) without the caller's own `finish` — or the `Drop`
    /// behind it — saying it again, differently.
    ended: Cell<bool>,
}

impl Task {
    /// A unit nobody is watching.
    ///
    /// For a caller that has to hand *some* task to a step that reports into one, where the work
    /// itself is not worth a line of its own — which is cheaper to say than to thread an
    /// `Option<&Task>` through every layer beneath it.
    pub fn silent() -> Self {
        Self {
            handle: None,
            done: Arc::new(AtomicUsize::new(0)),
            total: Cell::new(None),
            ended: Cell::new(false),
        }
    }

    /// Add `amount` to what this unit has done.
    pub fn advance(&self, amount: u64) {
        if self.handle.is_none() {
            return;
        }
        let amount = Self::narrow(amount);
        // `saturating_add`, as [`Ticker::advance`] does on the same counter: `fetch_add` wraps, so
        // a plain `+` here is a debug-build panic and a release-build wrap on the one target where
        // the counter is narrow enough to reach — `wasm32`, where `usize` is 32 bits.
        let done = self
            .done
            .fetch_add(amount, Ordering::Relaxed)
            .saturating_add(amount);
        self.emit_progress(done as u64);
    }

    /// Set what this unit has done, for a producer that counts absolutely rather than by delta.
    pub fn set_done(&self, done: u64) {
        if self.handle.is_none() {
            return;
        }
        self.done.store(Self::narrow(done), Ordering::Relaxed);
        self.emit_progress(done);
    }

    /// A counter into this unit that a fan-out worker can hold.
    ///
    /// [`Exec::fan_out`] requires `Send + Sync + 'static`, which a `Task` is not and must not be:
    /// it ends exactly once, from the place that started it. A ticker only *counts*, shares this
    /// task's counter, and is as cheap to clone as the handle it carries — so a CPU pass that
    /// remaps ten thousand classes on worker threads fills the same bar the main task opened.
    #[must_use]
    pub fn ticker(&self) -> Ticker {
        Ticker {
            handle: self.handle.clone(),
            done: Arc::clone(&self.done),
            total: self.total.get(),
        }
    }

    /// A `u64` count as the shared counter's width, saturating rather than wrapping.
    fn narrow(value: u64) -> usize {
        usize::try_from(value).unwrap_or(usize::MAX)
    }

    /// Declare the total this unit counts up to, once it becomes known.
    ///
    /// A download learns its size from `Content-Length` after the request goes out, so the unit it
    /// started as a spinner becomes a bar here.
    pub fn set_total(&self, total: u64) {
        self.total.set(Some(total));
        self.emit_progress(self.done.load(Ordering::Relaxed) as u64);
    }

    /// End this unit as a memo hit, having done none of the work it names.
    ///
    /// Named rather than left to `finish(Outcome::Fresh)` at each call site because a cache hit is
    /// reported from *inside* the step that would have done the work, where the caller who started
    /// the unit is not looking — and it is the one outcome a step decides for its own caller.
    pub fn fresh(&self) {
        self.end(Outcome::Fresh);
    }

    /// End this unit.
    ///
    /// A second ending is ignored, which is what makes [`fresh`](Self::fresh) usable from inside a
    /// step whose caller will also finish the unit it started.
    pub fn finish(self, outcome: Outcome) {
        self.end(outcome);
    }

    fn end(&self, outcome: Outcome) {
        if self.ended.replace(true) {
            return;
        }
        if let Some(handle) = &self.handle {
            handle.core.sink.emit(&Event::Finished {
                id: handle.id,
                outcome,
            });
        }
    }

    /// The unit's id.
    #[cfg(test)]
    pub(crate) fn id(&self) -> Option<UnitId> {
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

/// A counter into a unit somebody else started and will finish.
///
/// The half of a [`Task`] that crosses `Exec::fan_out`. It cannot start or end a unit — that stays
/// with the one place that owns it — and it holds no `Cell`, so it is `Send + Sync` like everything
/// else a fan-out closure captures.
#[derive(Clone)]
pub struct Ticker {
    handle: Option<TaskHandle>,
    done: Arc<AtomicUsize>,
    total: Option<u64>,
}

impl Ticker {
    /// Count one more item done.
    pub fn tick(&self) {
        self.advance(1);
    }

    /// Count `amount` more items done.
    fn advance(&self, amount: u64) {
        // Nothing is listening, so nothing counts: a silent ticker crossing `Exec::fan_out` would
        // otherwise do one contended read-modify-write per item on a counter nobody can read.
        let Some(handle) = &self.handle else {
            return;
        };
        let amount = usize::try_from(amount).unwrap_or(usize::MAX);
        let done = self
            .done
            .fetch_add(amount, Ordering::Relaxed)
            .saturating_add(amount);
        handle.core.sink.emit(&Event::Advanced {
            id: handle.id,
            done: done as u64,
            total: self.total,
        });
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.end(Outcome::Abandoned);
    }
}
