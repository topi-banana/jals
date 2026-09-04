//! One run's output context: the terminal it writes to, the event sink it reports through, and the
//! report it leaves behind.
//!
//! Every subcommand takes one. It exists so the wiring — which sinks a run has, in what order, and
//! what happens to them at the end — is written once instead of once per command.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Result, bail};
use jals_exec::Exec;
use jals_progress::{PackageRef, Progress, ReportMeta, Sink};

use crate::{
    shell::{MessageFormat, OutputArgs, Shell, TimingsFormat, Verb},
    timings::Ledger,
    ui::{Display, JsonStream, Tee},
};

/// The output half of one `jals` invocation.
pub(crate) struct Session {
    exec: Exec,
    shell: Arc<Shell>,
    message_format: MessageFormat,
    display: Arc<Display>,
    /// `None` unless `--message-format json` put the event stream on stdout.
    json: Option<Arc<JsonStream>>,
    progress: Progress,
    ledger: Option<Arc<Ledger>>,
    timings: Vec<TimingsFormat>,
    command: String,
    started: Instant,
    /// Where the report goes and what it is about, once a command has discovered a project.
    ///
    /// A command that never finds one — `jals fmt` over loose files, a failed run — still gets a
    /// report, written under the directory its user is standing in.
    project: RefCell<Option<(PathBuf, Option<String>)>>,
}

impl Session {
    /// Wire a run's sinks onto the shell it was given.
    ///
    /// The shell is the caller's because it outlives the runtime: a failure to *start* the runtime
    /// still has to be reported, and it has to be reported the same way as every other failure.
    pub(crate) fn new(shell: Arc<Shell>, exec: Exec, options: &OutputArgs) -> Self {
        let ledger = options.timings.as_ref().map(|_| Arc::new(Ledger::new()));

        let display = Arc::new(Display::new(Arc::clone(&shell)));
        let json = (options.message_format == MessageFormat::Json)
            .then(|| Arc::new(JsonStream::new(Arc::clone(&shell))));

        let mut sinks: Vec<Arc<dyn Sink>> = vec![Arc::clone(&display) as Arc<dyn Sink>];
        if let Some(json) = &json {
            sinks.push(Arc::clone(json) as Arc<dyn Sink>);
        }
        if let Some(ledger) = &ledger {
            sinks.push(Arc::clone(ledger) as Arc<dyn Sink>);
        }

        Self {
            exec,
            shell,
            message_format: options.message_format,
            display,
            json,
            progress: Progress::to(Arc::new(Tee::new(sinks))),
            ledger,
            timings: options.timings.clone().unwrap_or_default(),
            command: Self::command_line(),
            started: Instant::now(),
            project: RefCell::new(None),
        }
    }

    /// The terminal this run writes to.
    pub(crate) const fn shell(&self) -> &Arc<Shell> {
        &self.shell
    }

    /// The execution context every async step runs on.
    pub(crate) const fn exec(&self) -> &Exec {
        &self.exec
    }

    /// What a script reading this run's stdout gets.
    pub(crate) const fn message_format(&self) -> MessageFormat {
        self.message_format
    }

    /// The handle every emitter reports through.
    pub(crate) const fn progress(&self) -> &Progress {
        &self.progress
    }

    /// Hand stdout to the caller: this command's own machine output is the contract there.
    ///
    /// `jals test --message-format json` is what this exists for. `json` has always named *that
    /// command's* result objects, which a script parses, and the event stream is a second schema on
    /// the same lines — so the command takes the stream back rather than the two sharing it.
    pub(crate) fn owns_stdout(&self) {
        if let Some(json) = &self.json {
            json.silence();
        }
    }

    /// Refuse a flag whose output would have to share stdout with the event stream.
    ///
    /// Unlike [`owns_stdout`](Self::owns_stdout) these two are both things the user asked for in
    /// the same breath, and neither is a reading of the other: `--dry-run` prints a command line to
    /// be copied, `--diff` prints a patch to be applied. Dropping either silently is worse than
    /// saying they do not go together.
    pub(crate) fn stdout_is_free(&self, what: &str) -> Result<()> {
        if self.message_format == MessageFormat::Json {
            bail!(
                "{what} writes to stdout, which `--message-format json` is already using; pass \
                 one or the other"
            );
        }
        Ok(())
    }

    /// This run's work attributed to `package`.
    pub(crate) fn for_package(&self, package: PackageRef) -> Progress {
        self.progress.for_package(package)
    }

    /// How long the run has been going.
    pub(crate) fn elapsed(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    /// The closing `Finished` line, in cargo's shape.
    pub(crate) fn finished(&self, what: &str) {
        self.shell.status(
            Verb::Finished,
            format_args!("{what} in {:.2}s", self.elapsed().as_secs_f64()),
        );
    }

    /// Name the project this run turned out to be about.
    ///
    /// Called where a manifest is discovered rather than passed to
    /// [`write_timings`](Self::write_timings), so that one call at the end of `main` covers every
    /// command — including the ones that end early, and the ones that never find a project at all.
    pub(crate) fn note_project(&self, root: &Path, name: Option<&str>) {
        *self.project.borrow_mut() = Some((root.to_path_buf(), name.map(ToOwned::to_owned)));
    }

    /// Close the display down: the last batch of downloads still owes its summary.
    ///
    /// A run whose final phase was a fetch — an offline-refused resolve, a `--dry-run` that only
    /// downloaded — has no following phase to drain it, so the end of the run is that phase.
    pub(crate) fn finish_display(&self) {
        self.display.drain_downloads();
    }

    /// Write whatever `--timings` asked for.
    ///
    /// Reported rather than propagated: a build that succeeded did not fail because its report
    /// could not be written, and a report nobody could write is exactly the thing a person needs
    /// told rather than swallowed.
    pub(crate) fn write_timings(&self) {
        let Some(ledger) = &self.ledger else {
            return;
        };
        if self.timings.is_empty() {
            return;
        }
        let noted = self.project.borrow();
        let (root, project) = noted.as_ref().map_or_else(
            || (PathBuf::from("."), None),
            |(root, name)| (root.clone(), name.clone()),
        );
        let meta = ReportMeta {
            command: self.command.clone(),
            project,
            total_micros: u64::try_from(self.elapsed().as_micros()).unwrap_or(u64::MAX),
        };
        match ledger.write(&root, &self.timings, &meta) {
            Ok(reports) => {
                for path in reports {
                    self.shell.status(
                        Verb::Timing,
                        format_args!("report saved to {}", path.display()),
                    );
                }
            }
            Err(error) => self.shell.warn(format_args!("{error:#}")),
        }
    }

    /// The invocation as the user would recognize it, for the report's header.
    fn command_line() -> String {
        let mut rendered = String::from("jals");
        for argument in std::env::args().skip(1) {
            rendered.push(' ');
            rendered.push_str(&argument);
        }
        rendered
    }
}
