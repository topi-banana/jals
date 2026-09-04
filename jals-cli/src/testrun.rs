//! Presentation for `jals test`: the progress bar, the status lines, and the summary.
//!
//! Everything here is display. What to run is `jals_build::TestFilter`'s decision and how to run it
//! is `jals_build::TestLauncher`'s; this turns the events they produce into the shape a person
//! reads, following `cargo nextest`'s conventions because that is what the command is modelled on.
//!
//! The stream split matters and is easy to get wrong: **machine output goes to stdout, everything
//! else to stderr**. `--list` and `--message-format json` are the two things a script consumes, so
//! they are the two things that must not be interleaved with a progress bar. That rule now belongs
//! to [`Shell`], which every command in the crate shares; what stays here is this command's own
//! vocabulary, because `cargo nextest` is what it is modelled on.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use clap::ValueEnum;
use indicatif::{ProgressBar, ProgressStyle};

use jals_build::{TestCase, TestOutcome, TestVerdict};

use crate::shell::{MessageFormat, Shell, Style};

/// How much a run says about each test as it finishes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub(crate) enum StatusLevel {
    /// Say nothing per test.
    None,
    /// Only failures.
    Fail,
    /// Failures and retries.
    Retry,
    /// The above, plus tests that ran slower than `--slow-timeout`.
    Slow,
    /// The above, plus passes.
    Pass,
    /// The above, plus tests the filters skipped.
    Skip,
    /// Everything.
    All,
}

impl StatusLevel {
    /// Whether an outcome is reported at this level.
    fn shows(self, outcome: &TestOutcome, slow: Option<Duration>) -> bool {
        match outcome.verdict {
            TestVerdict::Failed { .. } | TestVerdict::TimedOut => self >= Self::Fail,
            TestVerdict::Skipped => self >= Self::Skip,
            TestVerdict::Passed => {
                if outcome.attempts > 1 {
                    self >= Self::Retry
                } else if outcome.is_slow(slow) {
                    self >= Self::Slow
                } else {
                    self >= Self::Pass
                }
            }
        }
    }
}

/// When a test's captured output is replayed.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputWhen {
    /// As the test finishes.
    Immediate,
    /// As it finishes, and again in the final summary.
    ImmediateFinal,
    /// Only in the final summary.
    Final,
    /// Never.
    Never,
}

impl OutputWhen {
    const fn at_finish(self) -> bool {
        matches!(self, Self::Immediate | Self::ImmediateFinal)
    }

    const fn at_summary(self) -> bool {
        matches!(self, Self::Final | Self::ImmediateFinal)
    }
}

/// What a run with no tests does.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum NoTests {
    /// Succeed.
    Pass,
    /// Succeed, saying so.
    Warn,
    /// Fail. The default: a filter that matched nothing is usually a typo, and a green run is the
    /// worst way to find that out.
    Fail,
}

/// The progress line: a `cargo`-style verb, the elapsed time, a bar filling the width left over,
/// and the test currently running. The prefix is painted and padded before it gets here, so the
/// template counts no escapes.
const BAR_TEMPLATE: &str = "{prefix} [{elapsed_precise}] {wide_bar} {pos}/{len}: {msg}";

/// How a [`TestReporter`] presents a run.
#[derive(Clone, Copy)]
pub(crate) struct ReporterConfig {
    /// How many tests the bar counts up to.
    pub(crate) total: u64,
    /// Whether a progress bar is wanted at all. It is still suppressed when the shell draws none —
    /// stderr is not a terminal, the run is quiet, or `--progress never`.
    pub(crate) show_bar: bool,
    pub(crate) status_level: StatusLevel,
    pub(crate) final_status_level: StatusLevel,
    pub(crate) failure_output: OutputWhen,
    pub(crate) success_output: OutputWhen,
    /// The threshold past which a passing test is reported as slow.
    pub(crate) slow_timeout: Option<Duration>,
}

/// The live display and the report, sharing one set of policies.
pub(crate) struct TestReporter {
    shell: Arc<Shell>,
    status_level: StatusLevel,
    final_status_level: StatusLevel,
    failure_output: OutputWhen,
    success_output: OutputWhen,
    slow_timeout: Option<Duration>,
    bar: Option<ProgressBar>,
}

impl TestReporter {
    /// Build a reporter, deciding once whether there is a progress bar at all.
    ///
    /// There is none when the caller asked for none, when output is not captured (the tests write
    /// straight to this terminal and would fight the bar for it), or when the shell draws none at
    /// all — a redirected run must produce the same bytes every time.
    pub(crate) fn new(shell: Arc<Shell>, config: ReporterConfig) -> Self {
        let bar = shell
            .bars()
            .filter(|_| config.show_bar && config.total > 0)
            .map(|bars| {
                let bar = ProgressBar::new(config.total);
                if let Ok(style) = ProgressStyle::with_template(BAR_TEMPLATE) {
                    bar.set_style(style.progress_chars("=> "));
                }
                bar.set_prefix(shell.pad("Running", Style::Good));
                bars.add(bar)
            });
        Self {
            shell,
            status_level: config.status_level,
            final_status_level: config.final_status_level,
            failure_output: config.failure_output,
            success_output: config.success_output,
            slow_timeout: config.slow_timeout,
            bar,
        }
    }

    /// Write one line to stderr. The shell suspends the live display around it.
    fn line(&self, text: &str) {
        self.shell.plain(text);
    }

    /// A `cargo`-style leading verb: right-aligned in twelve columns, coloured.
    fn verb(&self, style: Style, verb: &str) -> String {
        self.shell.pad(verb, style)
    }

    /// Announce what the run is about to do.
    pub(crate) fn starting(&self, selected: usize, classes: usize, skipped: usize) {
        let mut message = format!(
            "{} {selected} test{} across {classes} class{}",
            self.verb(Style::Good, "Starting"),
            if selected == 1 { "" } else { "s" },
            if classes == 1 { "" } else { "es" },
        );
        if skipped > 0 {
            let _ = write!(message, " ({skipped} skipped)");
        }
        self.line(&message);
    }

    /// A test's process is starting.
    pub(crate) fn started(&self, id: &str) {
        if let Some(bar) = &self.bar {
            bar.set_message(id.to_owned());
        }
    }

    /// A test finished.
    pub(crate) fn finished(&self, outcome: &TestOutcome) {
        if let Some(bar) = &self.bar {
            bar.inc(1);
        }
        if self.status_level.shows(outcome, self.slow_timeout) {
            self.line(&self.status_line(outcome));
        }
        if self.output_when(outcome).at_finish() {
            self.replay(outcome);
        }
    }

    /// Which `--*-output` policy governs this outcome's captured streams.
    const fn output_when(&self, outcome: &TestOutcome) -> OutputWhen {
        if outcome.verdict.is_failure() {
            self.failure_output
        } else {
            self.success_output
        }
    }

    /// One `PASS`/`FAIL` line: verb, duration, id.
    fn status_line(&self, outcome: &TestOutcome) -> String {
        let (style, verb) = match outcome.verdict {
            TestVerdict::Passed if outcome.attempts > 1 => (Style::Warn, "TRY OK"),
            TestVerdict::Passed if outcome.is_slow(self.slow_timeout) => (Style::Warn, "SLOW"),
            TestVerdict::Passed => (Style::Good, "PASS"),
            TestVerdict::Failed { .. } => (Style::Bad, "FAIL"),
            TestVerdict::TimedOut => (Style::Bad, "TIMEOUT"),
            TestVerdict::Skipped => (Style::Note, "SKIP"),
        };
        format!(
            "{} [{:>8.3}s] {}",
            self.verb(style, verb),
            outcome.duration.as_secs_f64(),
            outcome.id
        )
    }

    /// Replay a test's captured streams, indented under a header.
    ///
    /// Built into one block and written once. `replay` runs on the fan-out worker that ran the
    /// test, so a line-at-a-time write would let a second failing test's output interleave with
    /// this one's — and `--failure-output` defaults to `immediate`, so that is the default
    /// reading when more than one test fails, which is when it is read.
    fn replay(&self, outcome: &TestOutcome) {
        let mut block = String::new();
        for (label, path) in [("stdout", &outcome.stdout), ("stderr", &outcome.stderr)] {
            let Some(path) = path else { continue };
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let body = Self::strip_harness_frames(&text);
            if body.trim().is_empty() {
                continue;
            }
            let _ = writeln!(
                block,
                "{}",
                self.shell
                    .paint(&format!("--- {label}: {} ---", outcome.id), Style::Faint)
            );
            for line in body.lines() {
                let _ = writeln!(block, "    {line}");
            }
        }
        if !block.is_empty() {
            // Trailing newline already written by the last `writeln!`.
            block.pop();
            self.line(&block);
        }
    }

    /// Drop the generated harness's own stack frames from a trace.
    ///
    /// They are always the innermost two and never say anything about the test: the reader wants
    /// the line they wrote, and the frames below it are an implementation detail of how it was
    /// reached.
    fn strip_harness_frames(text: &str) -> String {
        text.lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !(trimmed.starts_with("at ")
                    && (trimmed.contains(jals_frontend::HARNESS_CLASS)
                        || trimmed.contains(jals_frontend::SHIM_PREFIX)))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The closing summary, plus whatever `--failure-output final` deferred.
    ///
    /// Returns whether the run failed.
    pub(crate) fn summary(&self, outcomes: &[TestOutcome], elapsed: Duration) -> bool {
        if let Some(bar) = &self.bar {
            bar.finish_and_clear();
        }
        let passed = outcomes
            .iter()
            .filter(|o| o.verdict == TestVerdict::Passed)
            .count();
        let failed = outcomes.iter().filter(|o| o.verdict.is_failure()).count();
        let skipped = outcomes
            .iter()
            .filter(|o| o.verdict == TestVerdict::Skipped)
            .count();

        for outcome in outcomes {
            // Per outcome, not per run: the question is whether *this* line was already printed
            // live, so comparing the two levels once would either repeat every line the live level
            // already showed or — at the defaults, `pass` live and `fail` final — repeat nothing
            // at all and make `--final-status-level` inert.
            if self.final_status_level.shows(outcome, self.slow_timeout)
                && !self.status_level.shows(outcome, self.slow_timeout)
            {
                self.line(&self.status_line(outcome));
            }
            if self.output_when(outcome).at_summary() {
                self.replay(outcome);
            }
        }

        self.line(&self.shell.paint("------------", Style::Faint));
        let style = if failed > 0 { Style::Bad } else { Style::Good };
        let mut tail = format!("{passed} passed");
        if failed > 0 {
            let _ = write!(tail, ", {failed} failed");
        }
        if skipped > 0 {
            let _ = write!(tail, ", {skipped} skipped");
        }
        self.line(&format!(
            "{} [{:>8.3}s] {} test{} run: {tail}",
            self.verb(style, "Summary"),
            elapsed.as_secs_f64(),
            outcomes.len(),
            if outcomes.len() == 1 { "" } else { "s" },
        ));
        failed > 0
    }

    /// Print the selected tests and nothing else. **stdout**, because this is what a script reads.
    pub(crate) fn list(shell: &Shell, cases: &[TestCase], format: MessageFormat) {
        match format {
            MessageFormat::Human => {
                for case in cases {
                    shell.machine(case.id());
                }
            }
            MessageFormat::Json => {
                for case in cases {
                    shell.machine(format_args!(
                        "{{\"id\":{},\"class\":{},\"method\":{},\"ignore\":{},\"should_fail\":{}}}",
                        Self::json_string(case.id()),
                        Self::json_string(case.class()),
                        Self::json_string(case.method()),
                        case.is_ignored(),
                        case.should_fail()
                    ));
                }
            }
        }
    }

    /// One line of JSON per outcome, on stdout.
    pub(crate) fn report_json(shell: &Shell, outcomes: &[TestOutcome]) {
        for outcome in outcomes {
            let (verdict, code) = match outcome.verdict {
                TestVerdict::Passed => ("passed", None),
                TestVerdict::Failed { code } => ("failed", code),
                TestVerdict::TimedOut => ("timed-out", None),
                TestVerdict::Skipped => ("skipped", None),
            };
            shell.machine(format_args!(
                "{{\"id\":{},\"verdict\":\"{verdict}\",\"exit-code\":{},\"duration-ms\":{},\
                 \"attempts\":{}}}",
                Self::json_string(&outcome.id),
                code.map_or_else(|| "null".to_owned(), |code| code.to_string()),
                outcome.duration.as_millis(),
                outcome.attempts
            ));
        }
    }

    /// Encode a string as a JSON literal.
    ///
    /// Written out rather than pulling in a serializer: a test id is a Java binary name plus a
    /// method name, so the only characters that can need escaping are the ones any string can
    /// carry, and the control-character range is handled rather than assumed away.
    fn json_string(value: &str) -> String {
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for ch in value.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                ch if (ch as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", ch as u32);
                }
                ch => out.push(ch),
            }
        }
        out.push('"');
        out
    }

    /// Say that a run selected nothing.
    pub(crate) fn no_tests(&self, reason: &str) {
        self.line(&format!("{} {reason}", self.verb(Style::Warn, "Warning")));
    }

    /// How many distinct classes a selection spans, for the opening line.
    pub(crate) fn class_count(cases: &[TestCase]) -> usize {
        let mut classes: Vec<&str> = cases.iter().map(TestCase::class).collect();
        classes.sort_unstable();
        classes.dedup();
        classes.len()
    }
}
