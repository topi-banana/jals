//! The ledger a run leaves behind: one span per unit of work, and the two renderings of it.
//!
//! Time comes in from the caller. Portable code here has no clock — the same reason
//! `jals_classpath`'s retry jitter is derived from a locator rather than drawn — so a host stamps
//! each event as it arrives and this only does arithmetic on the numbers it is given. `cargo`'s
//! own `--timings` records host-side for the same reason.

use alloc::{borrow::ToOwned, string::String, vec::Vec};

use serde::Serialize;

use crate::{
    event::{Activity, Event, Outcome, Unit, UnitId},
    html::Page,
};

/// One unit of work, from the moment it started to the moment it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Span {
    pub(crate) unit: Unit,
    /// Microseconds since the run's own origin, not since any epoch.
    pub(crate) start_micros: u64,
    pub(crate) end_micros: u64,
    pub(crate) outcome: Outcome,
    /// The last count the unit reported, which is what it actually got through.
    ///
    /// Serialized into the JSON report and read back by nothing, which is the point: a report says
    /// how far a unit got, and this crate has no reason to ask.
    done: u64,
}

impl Span {
    /// How long the unit took.
    pub(crate) const fn duration_micros(&self) -> u64 {
        self.end_micros.saturating_sub(self.start_micros)
    }
}

/// What a report says about the run as a whole.
///
/// The host's, because every field of it is: which command ran, which project it ran in, and how
/// long the process was alive — including the parts no unit covered.
#[derive(Debug, Clone)]
pub struct ReportMeta {
    /// The command line as the user would recognize it, such as `jals build --features client`.
    pub command: String,
    /// The root package, when the command had one.
    pub project: Option<String>,
    /// The whole run's wall time. Never shorter than the last span's end, and usually longer —
    /// the difference is the work nobody reported.
    pub total_micros: u64,
}

/// Every span of one run, in the order the units started.
#[derive(Debug, Default)]
pub struct Timeline {
    spans: Vec<Span>,
    /// `(id, index into spans)` for units that started and have not finished.
    open: Vec<(UnitId, usize)>,
}

impl Timeline {
    /// An empty ledger.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            spans: Vec::new(),
            open: Vec::new(),
        }
    }

    /// Fold one event in, stamped `at_micros` after the run's origin.
    ///
    /// Events for a unit this ledger never saw start are dropped rather than invented: a partial
    /// stream is a partial report, not a fabricated one.
    pub fn record(&mut self, event: &Event, at_micros: u64) {
        match event {
            Event::Started { id, unit } => {
                self.open.push((*id, self.spans.len()));
                self.spans.push(Span {
                    unit: unit.clone(),
                    start_micros: at_micros,
                    end_micros: at_micros,
                    outcome: Outcome::Abandoned,
                    done: 0,
                });
            }
            Event::Advanced { id, done, total } => {
                if let Some(span) = self.open_span(*id) {
                    span.done = *done;
                    if total.is_some() {
                        span.unit.total = *total;
                    }
                }
            }
            Event::Finished { id, outcome } => {
                let Some(position) = self.open.iter().position(|(open, _)| open == id) else {
                    return;
                };
                let (_, index) = self.open.swap_remove(position);
                let span = &mut self.spans[index];
                span.end_micros = at_micros;
                span.outcome = *outcome;
            }
        }
    }

    /// The span of a unit that has started and not finished.
    fn open_span(&mut self, id: UnitId) -> Option<&mut Span> {
        let index = self
            .open
            .iter()
            .find(|(open, _)| *open == id)
            .map(|(_, index)| *index)?;
        self.spans.get_mut(index)
    }

    /// Every span recorded, in the order the units started.
    pub(crate) fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// Total time spent in each activity, summed over the units that ran it.
    ///
    /// Overlapping units are counted once each, so on a concurrent run these add up to more than
    /// the wall clock. That is the point: it is where the time went, not how long it took.
    pub(crate) fn by_activity(&self) -> Vec<(Activity, u64, usize)> {
        let mut totals: Vec<(Activity, u64, usize)> = Vec::new();
        for span in &self.spans {
            let activity = span.unit.activity;
            if let Some(entry) = totals.iter_mut().find(|(known, _, _)| *known == activity) {
                entry.1 = entry.1.saturating_add(span.duration_micros());
                entry.2 += 1;
            } else {
                totals.push((activity, span.duration_micros(), 1));
            }
        }
        totals.sort_by_key(|(_, time, _)| core::cmp::Reverse(*time));
        totals
    }

    /// The number of units running at once, sampled `samples` times across the run.
    pub(crate) fn concurrency(&self, total_micros: u64, samples: usize) -> Vec<usize> {
        (0..samples)
            .map(|sample| {
                let at = total_micros.saturating_mul(sample as u64) / samples.max(1) as u64;
                self.spans
                    .iter()
                    .filter(|span| {
                        span.start_micros <= at && at < span.end_micros.max(span.start_micros + 1)
                    })
                    .count()
            })
            .collect()
    }

    /// The whole ledger as one JSON document.
    #[must_use]
    pub fn json(&self, meta: &ReportMeta) -> String {
        let document = Document {
            command: &meta.command,
            project: meta.project.as_deref(),
            total_micros: meta.total_micros,
            spans: &self.spans,
        };
        serde_json::to_string(&document).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The whole ledger as one self-contained HTML page.
    ///
    /// No script, no stylesheet, no font, and no image is fetched: a build report has to open from
    /// a file:// URL on a machine that is offline, which is exactly the machine that wanted to know
    /// why the build was slow.
    #[must_use]
    pub fn html(&self, meta: &ReportMeta) -> String {
        Page::render(self, meta)
    }
}

/// The JSON shape. Separate from [`ReportMeta`] because that one is the host's input and this is
/// the file's schema.
#[derive(Serialize)]
struct Document<'a> {
    command: &'a str,
    project: Option<&'a str>,
    total_micros: u64,
    spans: &'a [Span],
}
