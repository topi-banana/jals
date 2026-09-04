//! `--timings`: where a run's time went.
//!
//! The ledger itself is `jals_progress::Timeline` — portable, so the same report can be produced
//! by a host that is not this one. What lives here is the half only a host can do: reading a clock,
//! and writing files.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use jals_progress::{Event, ReportMeta, Sink, Timeline};

use crate::shell::TimingsFormat;

/// Where a run's reports land, under the build output the project already owns.
const DIRECTORY: &str = "target/jals/timings";
/// The name that is overwritten every run, so a bookmark keeps working. Cargo's `cargo-timing.html`
/// serves the same purpose.
const LATEST: &str = "jals-timings";

/// A [`Sink`] that stamps each event and folds it into a [`Timeline`].
///
/// Stamping here is what makes the portable ledger possible: portable code has no clock, so the
/// host reads one as the event arrives and hands over a number.
pub(crate) struct Ledger {
    origin: Instant,
    timeline: Mutex<Timeline>,
}

impl Ledger {
    /// A ledger whose origin is now.
    pub(crate) fn new() -> Self {
        Self {
            origin: Instant::now(),
            timeline: Mutex::new(Timeline::new()),
        }
    }

    /// Write the renderings `formats` asks for, returning the report of each.
    ///
    /// Each format lands twice: once under a timestamp, so a run's report is never overwritten by
    /// the next one, and once under a stable name so a bookmark keeps working. Only the first is
    /// returned — it is the one worth naming, and cargo names exactly one too.
    ///
    /// `root` is the project the run was about; a command with no project writes under the working
    /// directory, which is where its user is standing.
    pub(crate) fn write(
        &self,
        root: &Path,
        formats: &[TimingsFormat],
        meta: &ReportMeta,
    ) -> Result<Vec<PathBuf>> {
        let directory = root.join(DIRECTORY);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        // Milliseconds, not seconds: two runs of a warm build finish inside one second, and a
        // stamp that collides silently replaces the report this function promises never to
        // overwrite.
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        let rendered: Vec<(&str, String)> = {
            let timeline = self.timeline();
            formats
                .iter()
                .map(|format| match format {
                    TimingsFormat::Html => ("html", timeline.html(meta)),
                    TimingsFormat::Json => ("json", timeline.json(meta)),
                })
                .collect()
        };
        let mut reports = Vec::new();
        for (extension, body) in &rendered {
            for (name, is_report) in [
                (format!("{LATEST}-{stamp}.{extension}"), true),
                (format!("{LATEST}.{extension}"), false),
            ] {
                let path = directory.join(&name);
                std::fs::write(&path, body)
                    .with_context(|| format!("writing {}", path.display()))?;
                if is_report {
                    reports.push(path);
                }
            }
        }
        Ok(reports)
    }

    fn timeline(&self) -> std::sync::MutexGuard<'_, Timeline> {
        self.timeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Sink for Ledger {
    fn emit(&self, event: &Event) {
        // A run long enough to overflow `u64` microseconds is 584 000 years; saturating is the
        // honest answer rather than a cast that would wrap.
        let at = u64::try_from(self.origin.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.timeline().record(event, at);
    }
}
