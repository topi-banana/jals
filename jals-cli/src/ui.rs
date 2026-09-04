//! The live display: `jals_progress` events turned into cargo-shaped status lines and bars.
//!
//! This is where the CLI's verbs are chosen. A portable crate reports the *fact* that it is
//! fetching; whether that reads as `Downloading` while it runs, `Downloaded` in an aggregate at the
//! end, or nothing at all because the bar already showed it, is decided here and nowhere else.
//!
//! Nothing here writes to a stream directly — every line goes through [`Shell`], so a bar is
//! suspended around it and `--color` is answered once.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use indicatif::{ProgressBar, ProgressStyle};
use jals_progress::{Activity, Event, Outcome, Sink, Unit, UnitId};

use crate::shell::{Shell, Style, Verb};

/// A bar that counts bytes: what a download is.
const BYTES_TEMPLATE: &str =
    "{prefix} [{elapsed_precise}] [{wide_bar}] {bytes}/{total_bytes} ({bytes_per_sec}) {msg}";
/// A bar that counts things: archive members, classes, files.
const COUNT_TEMPLATE: &str = "{prefix} [{elapsed_precise}] [{wide_bar}] {pos}/{len} {msg}";
/// Work whose size nobody could state up front.
const SPINNER_TEMPLATE: &str = "{prefix} {spinner} {msg}";
/// How often a spinner redraws. Slow enough to be cheap, fast enough to look alive.
const TICK_MILLIS: u64 = 120;

/// One unit the display is currently showing.
struct Tracked {
    unit: Unit,
    bar: Option<ProgressBar>,
    done: u64,
}

/// What the display knows between events.
#[derive(Default)]
struct State {
    units: HashMap<u64, Tracked>,
    /// Downloads are aggregated rather than announced one by one, the way cargo aggregates
    /// `Downloaded 61 crates`: a Minecraft build fetches sixty library jars, and sixty status
    /// lines are noise where one is news.
    fetches: Batch,
}

/// A run of downloads, summarized when the last of them finishes.
#[derive(Default)]
struct Batch {
    active: usize,
    finished: usize,
    bytes: u64,
    started: Option<Instant>,
}

/// The terminal display.
pub(crate) struct Display {
    shell: Arc<Shell>,
    state: Mutex<State>,
}

impl Display {
    /// A display drawing through `shell`.
    pub(crate) fn new(shell: Arc<Shell>) -> Self {
        Self {
            shell,
            state: Mutex::new(State::default()),
        }
    }

    /// The verb a running unit of this kind leads its line with.
    ///
    /// Exhaustive on purpose: a new activity has to be given a word here, rather than silently
    /// arriving as a bar with no name.
    const fn verb(activity: Activity) -> Verb {
        match activity {
            Activity::Script => Verb::Preparing,
            Activity::Resolve => Verb::Resolving,
            Activity::Fetch => Verb::Downloading,
            Activity::Extract => Verb::Extracting,
            Activity::Remap => Verb::Remapping,
            Activity::Merge => Verb::Merging,
            Activity::Decompile => Verb::Decompiling,
            Activity::Publish => Verb::Publishing,
            Activity::Index => Verb::Indexing,
            Activity::Compile => Verb::Compiling,
            Activity::Package => Verb::Packaging,
            Activity::Run => Verb::Running,
            Activity::Test => Verb::Testing,
            Activity::Format => Verb::Formatting,
            Activity::Lint => Verb::Checking,
        }
    }

    /// Whether a unit of this kind announces itself when it starts.
    ///
    /// A fetch does not: its bar is already the announcement, and the batch it belongs to is
    /// summarized when it drains. Everything else does, because a remap or a decompile can hold the
    /// terminal for minutes and a reader needs to know what is holding it.
    const fn announces(activity: Activity) -> bool {
        !matches!(activity, Activity::Fetch)
    }

    /// A started unit's bar, when this run draws any.
    fn bar(&self, unit: &Unit) -> Option<ProgressBar> {
        let bars = self.shell.bars()?;
        let (bar, template) = match unit.total {
            Some(total) if matches!(unit.activity, Activity::Fetch) => {
                (ProgressBar::new(total), BYTES_TEMPLATE)
            }
            Some(total) => (ProgressBar::new(total), COUNT_TEMPLATE),
            None => (ProgressBar::new_spinner(), SPINNER_TEMPLATE),
        };
        if let Ok(style) = ProgressStyle::with_template(template) {
            bar.set_style(style.progress_chars("=> "));
        }
        // Painted and padded here rather than in the template: an escape is zero-width to a
        // terminal and very wide to `{:>12}`, so a coloured verb inside a template would push the
        // whole bar sideways.
        bar.set_prefix(
            self.shell
                .pad(Self::verb(unit.activity).label(), Style::Good),
        );
        bar.set_message(unit.describe());
        if unit.total.is_none() {
            bar.enable_steady_tick(std::time::Duration::from_millis(TICK_MILLIS));
        }
        Some(bars.add(bar))
    }

    fn started(&self, id: UnitId, unit: &Unit) {
        // Another phase beginning is what closes a batch of downloads, so the summary lands above
        // the line that follows it. Draining on `active == 0` alone would not do it: the task plan
        // walks its nodes serially, so every `tasks.fetch_jar` is a batch of one, and a Minecraft
        // build would print sixty `Downloaded 1 file` lines instead of the one it promises.
        if unit.activity != Activity::Fetch {
            self.drain_downloads();
        }
        if unit.activity == Activity::Test {
            // `jals test` draws this phase itself, in the `cargo nextest` shape the command is
            // modelled on. Not tracking the unit is the whole stand-down: its later events find
            // nothing here and return, and the ledger — which is what the unit is for — still
            // gets its span.
            return;
        }
        let bar = self.bar(unit);
        if Self::announces(unit.activity) {
            self.shell
                .status(Self::verb(unit.activity), unit.describe());
        } else {
            self.shell
                .verbose_status(Self::verb(unit.activity), unit.describe());
        }
        let mut state = self.lock();
        if unit.activity == Activity::Fetch {
            state.fetches.active += 1;
            state.fetches.started.get_or_insert_with(Instant::now);
        }
        state.units.insert(
            id.get(),
            Tracked {
                unit: unit.clone(),
                bar,
                done: 0,
            },
        );
    }

    fn advanced(&self, id: UnitId, done: u64, total: Option<u64>) {
        // The bar is `Arc`-backed, so it is cloned out and drawn to *outside* the lock. A remap
        // ticks once per class from every fan-out worker, and holding the display's one mutex
        // across an indicatif draw serializes the very parallelism the bar is reporting on.
        let drawn = {
            let mut state = self.lock();
            let Some(tracked) = state.units.get_mut(&id.get()) else {
                return;
            };
            tracked.done = done;
            if let Some(total) = total {
                tracked.unit.total = Some(total);
            }
            let drawn = tracked.bar.clone().map(|bar| (bar, tracked.unit.activity));
            drop(state);
            drawn
        };
        let Some((bar, activity)) = drawn else {
            return;
        };
        // A download that learns its length from `Content-Length` becomes a bar here, having
        // started as a spinner.
        if let Some(total) = total
            && bar.length() != Some(total)
        {
            bar.set_length(total);
            Self::restyle(&bar, activity);
        }
        bar.set_position(done);
    }

    fn finished(&self, id: UnitId, outcome: Outcome) {
        let mut state = self.lock();
        let Some(tracked) = state.units.remove(&id.get()) else {
            drop(state);
            return;
        };
        if let Some(bar) = &tracked.bar {
            bar.finish_and_clear();
        }
        if tracked.unit.activity == Activity::Fetch {
            state.fetches.active = state.fetches.active.saturating_sub(1);
            if outcome == Outcome::Completed {
                state.fetches.finished += 1;
                state.fetches.bytes = state.fetches.bytes.saturating_add(tracked.done);
            }
        }
        drop(state);

        if outcome == Outcome::Fresh {
            self.shell
                .verbose_status(Verb::Fresh, tracked.unit.describe());
        }
    }

    /// Summarize the downloads that have finished, if a batch of them is waiting to be summarized.
    ///
    /// Called when another phase begins and once more when the run ends, which between them cover
    /// both shapes a batch takes: the dependency resolver's concurrent fan of fetches, and the task
    /// plan's one-at-a-time walk.
    pub(crate) fn drain_downloads(&self) {
        let mut state = self.lock();
        if state.fetches.active > 0 {
            drop(state);
            return;
        }
        // Taken whenever nothing is in flight, and only *summarized* when something completed. A
        // batch of pure cache hits or failures has nothing to announce but has still stamped its
        // clock, and leaving it in place would make the next real download's line read
        // `in 48.9s` — the whole run rather than the transfer.
        let batch = core::mem::take(&mut state.fetches);
        drop(state);
        if batch.finished == 0 {
            return;
        }
        self.summarize(&batch);
    }

    /// The one line a drained batch of downloads leaves behind.
    fn summarize(&self, batch: &Batch) {
        let files = batch.finished;
        let elapsed = batch
            .started
            .map(|started| started.elapsed().as_secs_f64())
            .unwrap_or_default();
        self.shell.status(
            Verb::Downloaded,
            format_args!(
                "{files} file{} ({}) in {elapsed:.1}s",
                if files == 1 { "" } else { "s" },
                Self::bytes(batch.bytes)
            ),
        );
    }

    /// Give a bar the style its now-known shape asks for.
    fn restyle(bar: &ProgressBar, activity: Activity) {
        let template = if matches!(activity, Activity::Fetch) {
            BYTES_TEMPLATE
        } else {
            COUNT_TEMPLATE
        };
        if let Ok(style) = ProgressStyle::with_template(template) {
            bar.set_style(style.progress_chars("=> "));
        }
        bar.disable_steady_tick();
    }

    /// A byte count a person reads, to one decimal, in integer arithmetic.
    ///
    /// No `f64`: a download is measured in bytes and a mantissa that cannot hold them exactly is
    /// the wrong tool for a number that is going to be read back off the screen.
    fn bytes(bytes: u64) -> String {
        const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
        let mut scaled = bytes;
        let mut divisor = 1u64;
        let mut unit = 0;
        while scaled >= 1024 && unit + 1 < UNITS.len() {
            scaled /= 1024;
            divisor = divisor.saturating_mul(1024);
            unit += 1;
        }
        if unit == 0 {
            return format!("{bytes} B");
        }
        let tenths = bytes.saturating_mul(10) / divisor % 10;
        format!("{scaled}.{tenths} {}", UNITS[unit])
    }

    /// The display's state, recovered rather than propagated if a worker panicked while holding it:
    /// a poisoned display is still better than a second panic on the way out.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Sink for Display {
    fn emit(&self, event: &Event) {
        match event {
            Event::Started { id, unit } => self.started(*id, unit),
            Event::Advanced { id, done, total } => self.advanced(*id, *done, *total),
            Event::Finished { id, outcome } => self.finished(*id, *outcome),
        }
    }
}

/// Every event as one JSON object per line on stdout, for `--message-format json`.
///
/// It can be switched off part-way through a run, because stdout is a contract with one holder: a
/// command whose *own* machine output is what `--message-format json` has always meant — `jals
/// test`'s result objects — takes the stream back before it starts, and the events stop rather than
/// interleaving a second schema into the same lines.
pub(crate) struct JsonStream {
    shell: Arc<Shell>,
    live: AtomicBool,
}

impl JsonStream {
    pub(crate) const fn new(shell: Arc<Shell>) -> Self {
        Self {
            shell,
            live: AtomicBool::new(true),
        }
    }

    /// Stop writing: somebody else owns stdout now.
    pub(crate) fn silence(&self) {
        self.live.store(false, Ordering::Relaxed);
    }
}

impl Sink for JsonStream {
    fn emit(&self, event: &Event) {
        if !self.live.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(line) = serde_json::to_string(event) {
            self.shell.machine(line);
        }
    }
}

/// Several sinks behind one handle.
///
/// A run draws, records timings, and streams JSON from the same event sequence; teeing here is what
/// keeps `Progress` carrying exactly one sink and the emitters knowing nothing about how many
/// consumers there are.
pub(crate) struct Tee {
    sinks: Vec<Arc<dyn Sink>>,
}

impl Tee {
    pub(crate) const fn new(sinks: Vec<Arc<dyn Sink>>) -> Self {
        Self { sinks }
    }
}

impl Sink for Tee {
    fn emit(&self, event: &Event) {
        for sink in &self.sinks {
            sink.emit(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Display;

    #[test]
    fn a_byte_count_reads_the_way_a_person_would_say_it() {
        // Integer arithmetic throughout: a download is measured in bytes, and a mantissa that
        // cannot hold them exactly is the wrong tool for a number read back off a screen.
        assert_eq!(Display::bytes(0), "0 B");
        assert_eq!(Display::bytes(999), "999 B");
        assert_eq!(Display::bytes(1024), "1.0 KiB");
        assert_eq!(Display::bytes(1536), "1.5 KiB");
        assert_eq!(Display::bytes(54_857_400), "52.3 MiB");
        assert_eq!(Display::bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
        // The largest unit does not run out: a terabyte is still counted in GiB rather than
        // rendered as a number nobody can read.
        assert_eq!(Display::bytes(1024 * 1024 * 1024 * 1024), "1024.0 GiB");
    }
}
