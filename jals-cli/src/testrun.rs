//! Presentation for `jals test`: the progress bar, the status lines, and the summary.
//!
//! Everything here is display. What to run is `jals_build::TestFilter`'s decision and how to run it
//! is `jals_build::TestLauncher`'s; this turns the events they produce into the shape a person
//! reads, following `cargo nextest`'s conventions because that is what the command is modelled on.
//!
//! The stream split matters and is easy to get wrong: **machine output goes to stdout, everything
//! else to stderr**. `--list` and `--message-format json` are the two things a script consumes, so
//! they are the two things that must not be interleaved with a progress bar.

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::time::Duration;

use clap::ValueEnum;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

use jals_build::{TestCase, TestOutcome, TestVerdict};

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
            TestVerdict::Failed(_) | TestVerdict::TimedOut => self >= Self::Fail,
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

/// How `--list` and the run report themselves.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum MessageFormat {
    /// For a person.
    Human,
    /// One JSON object per line, on stdout.
    Json,
}

/// When to colour.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ColorWhen {
    /// Colour when stderr is a terminal and `NO_COLOR` is unset.
    Auto,
    Always,
    Never,
}

impl ColorWhen {
    /// Whether this run colours its output.
    pub(crate) fn enabled(self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }
}

/// ANSI escapes, matching `report.rs` rather than pulling a second styling crate in.
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";

/// The progress line: a `cargo`-style verb, the elapsed time, a bar filling the width left over,
/// and the test currently running.
const BAR_TEMPLATE: &str = "{prefix:>12} [{elapsed_precise}] {wide_bar} {pos}/{len}: {msg}";

/// How a [`TestReporter`] presents a run.
#[derive(Clone, Copy)]
pub(crate) struct ReporterConfig {
    /// How many tests the bar counts up to.
    pub(crate) total: u64,
    pub(crate) color: bool,
    /// Whether a progress bar is wanted at all. It is still suppressed when stderr is not a
    /// terminal.
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
    color: bool,
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
    /// straight to this terminal and would fight the bar for it), or when stderr is not a
    /// terminal — a redirected run must produce the same bytes every time.
    pub(crate) fn new(config: ReporterConfig) -> Self {
        let bar =
            (config.show_bar && std::io::stderr().is_terminal() && config.total > 0).then(|| {
                let bar =
                    ProgressBar::with_draw_target(Some(config.total), ProgressDrawTarget::stderr());
                if let Ok(style) = ProgressStyle::with_template(BAR_TEMPLATE) {
                    bar.set_style(style.progress_chars("=> "));
                }
                bar.set_prefix("Running");
                bar
            });
        Self {
            color: config.color,
            status_level: config.status_level,
            final_status_level: config.final_status_level,
            failure_output: config.failure_output,
            success_output: config.success_output,
            slow_timeout: config.slow_timeout,
            bar,
        }
    }

    /// Paint `text` when colour is on.
    fn paint(&self, style: &str, text: &str) -> String {
        if self.color {
            format!("{style}{text}{RESET}")
        } else {
            text.to_owned()
        }
    }

    /// Write one line to stderr, suspending the bar so it is not overwritten.
    fn line(&self, text: &str) {
        match &self.bar {
            Some(bar) => bar.suspend(|| eprintln!("{text}")),
            None => eprintln!("{text}"),
        }
    }

    /// A `cargo`-style leading verb: right-aligned in twelve columns, coloured.
    fn verb(&self, style: &str, verb: &str) -> String {
        self.paint(style, &format!("{verb:>12}"))
    }

    /// Announce what the run is about to do.
    pub(crate) fn starting(&self, selected: usize, classes: usize, skipped: usize) {
        let mut message = format!(
            "{} {selected} test{} across {classes} class{}",
            self.verb(&format!("{BOLD}{GREEN}"), "Starting"),
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
        let (style, verb) = match &outcome.verdict {
            TestVerdict::Passed if outcome.attempts > 1 => (format!("{BOLD}{YELLOW}"), "TRY OK"),
            TestVerdict::Passed if outcome.is_slow(self.slow_timeout) => {
                (format!("{BOLD}{YELLOW}"), "SLOW")
            }
            TestVerdict::Passed => (format!("{BOLD}{GREEN}"), "PASS"),
            TestVerdict::Failed(_) => (format!("{BOLD}{RED}"), "FAIL"),
            TestVerdict::TimedOut => (format!("{BOLD}{RED}"), "TIMEOUT"),
            TestVerdict::Skipped => (format!("{BOLD}{CYAN}"), "SKIP"),
        };
        format!(
            "{} [{:>8.3}s] {}",
            self.verb(&style, verb),
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
        // The program's own words first. A target reports *why* a test failed, and that sentence is
        // the whole difference between this and a harness run, where an exit status is all there
        // is — burying it under a game's standard output would throw away the better answer.
        if let TestVerdict::Failed(jals_build::FailureKind::Reported(why)) = &outcome.verdict
            && !why.is_empty()
        {
            let _ = writeln!(
                block,
                "{}",
                self.paint(&format!("{BOLD}{RED}"), &format!("    {why}"))
            );
        }
        // Screenshots next: when a test failed because a picture disagreed, that is the thing the
        // reader came for, and a game's standard output is thousands of lines long.
        for shot in &outcome.shots {
            if let Some(line) = self.shot_line(shot) {
                let _ = writeln!(block, "{line}");
            }
        }
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
                self.paint(DIM, &format!("--- {label}: {} ---", outcome.id))
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

    /// One screenshot's verdict, indented under its test — `None` for a match, which needs no line.
    ///
    /// Every failing case names **where to look**. A screenshot difference is not something a
    /// number settles: the reader has to open the picture, so the paths are the message and the
    /// count is the headline.
    fn shot_line(&self, shot: &jals_build::ShotOutcome) -> Option<String> {
        use jals_build::ShotOutcome;
        match shot {
            ShotOutcome::Matched { .. } => None,
            ShotOutcome::NoReference { name, actual } => Some(format!(
                "{}\n        actual  {}",
                self.paint(
                    &format!("{BOLD}{YELLOW}"),
                    &format!("    no reference image for `{name}`")
                ),
                actual.display()
            )),
            ShotOutcome::Missing { name, actual } => Some(format!(
                "{}\n        expected at  {}",
                self.paint(
                    &format!("{BOLD}{RED}"),
                    &format!("    the run wrote no screenshot named `{name}`")
                ),
                actual.display()
            )),
            ShotOutcome::Unreadable { name, path, reason } => Some(format!(
                "{}\n        {}\n        {}",
                self.paint(
                    &format!("{BOLD}{RED}"),
                    &format!("    `{name}` could not be read")
                ),
                path.display(),
                reason
            )),
            ShotOutcome::Differed(diff) => {
                let headline = match diff.size_mismatch {
                    Some((ew, eh, aw, ah)) => {
                        format!(
                            "    `{}` is {aw}x{ah}, but the reference is {ew}x{eh}",
                            diff.name
                        )
                    }
                    None => format!(
                        "    `{}`: {} of {} pixels differ ({:.4}%)",
                        diff.name,
                        diff.differing,
                        diff.compared,
                        diff.ratio * 100.0
                    ),
                };
                let mut text = self.paint(&format!("{BOLD}{RED}"), &headline);
                let _ = write!(text, "\n        reference  {}", diff.reference.display());
                let _ = write!(text, "\n        actual     {}", diff.actual.display());
                if let Some(path) = &diff.diff {
                    let _ = write!(text, "\n        difference {}", path.display());
                }
                Some(text)
            }
        }
    }

    /// A one-line summary of a test's failing screenshots, for the JSON report's `reason`.
    fn shot_summary(outcome: &TestOutcome) -> String {
        use jals_build::ShotOutcome;
        let parts: Vec<String> = outcome
            .shots
            .iter()
            .filter(|shot| shot.is_failure())
            .map(|shot| match shot {
                ShotOutcome::Differed(diff) if diff.size_mismatch.is_some() => {
                    format!("{}: wrong size", diff.name)
                }
                ShotOutcome::Differed(diff) => {
                    format!("{}: {} pixels differ", diff.name, diff.differing)
                }
                ShotOutcome::Missing { name, .. } => format!("{name}: not written"),
                ShotOutcome::Unreadable { name, .. } => format!("{name}: unreadable"),
                ShotOutcome::Matched { .. } | ShotOutcome::NoReference { .. } => String::new(),
            })
            .collect();
        parts.join("; ")
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

        self.line(&self.paint(DIM, "------------"));
        let style = if failed > 0 {
            format!("{BOLD}{RED}")
        } else {
            format!("{BOLD}{GREEN}")
        };
        let mut tail = format!("{passed} passed");
        if failed > 0 {
            let _ = write!(tail, ", {failed} failed");
        }
        if skipped > 0 {
            let _ = write!(tail, ", {skipped} skipped");
        }
        self.line(&format!(
            "{} [{:>8.3}s] {} test{} run: {tail}",
            self.verb(&style, "Summary"),
            elapsed.as_secs_f64(),
            outcomes.len(),
            if outcomes.len() == 1 { "" } else { "s" },
        ));
        failed > 0
    }

    /// Print the selected tests and nothing else. **stdout**, because this is what a script reads.
    pub(crate) fn list(cases: &[TestCase], format: MessageFormat) {
        match format {
            MessageFormat::Human => {
                for case in cases {
                    println!("{}", case.id());
                }
            }
            MessageFormat::Json => {
                for case in cases {
                    println!(
                        "{{\"id\":{},\"class\":{},\"method\":{},\"ignore\":{},\"should_fail\":{}}}",
                        Self::json_string(case.id()),
                        Self::json_string(case.class()),
                        Self::json_string(case.method()),
                        case.is_ignored(),
                        case.should_fail()
                    );
                }
            }
        }
    }

    /// One line of JSON per outcome, on stdout.
    pub(crate) fn report_json(outcomes: &[TestOutcome]) {
        for outcome in outcomes {
            // `reason` carries what a status code cannot: the program's own words for a reported
            // failure, and the pixel count for a screenshot that disagreed. A consumer that only
            // reads `verdict` is unaffected — the field is `null` where there is nothing to say.
            let (verdict, code, reason) = match &outcome.verdict {
                TestVerdict::Passed => ("passed", None, None),
                TestVerdict::Failed(jals_build::FailureKind::Process { code }) => {
                    ("failed", *code, None)
                }
                TestVerdict::Failed(jals_build::FailureKind::Reported(why)) => {
                    ("failed", None, Some(why.clone()))
                }
                TestVerdict::Failed(jals_build::FailureKind::Screenshot) => {
                    ("failed", None, Some(Self::shot_summary(outcome)))
                }
                TestVerdict::TimedOut => ("timed-out", None, None),
                TestVerdict::Skipped => ("skipped", None, None),
            };
            println!(
                "{{\"id\":{},\"verdict\":\"{verdict}\",\"exit-code\":{},\"duration-ms\":{},\
                 \"attempts\":{},\"reason\":{}}}",
                Self::json_string(&outcome.id),
                code.map_or_else(|| "null".to_owned(), |code| code.to_string()),
                outcome.duration.as_millis(),
                outcome.attempts,
                reason.map_or_else(|| "null".to_owned(), |text| Self::json_string(&text))
            );
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

    /// Say that a compile is starting, matching `cargo`'s leading verb.
    pub(crate) fn compiling(&self, name: &str) {
        self.line(&format!(
            "{} {name}",
            self.verb(&format!("{BOLD}{GREEN}"), "Compiling")
        ));
    }

    /// Say that a run selected nothing.
    pub(crate) fn no_tests(&self, reason: &str) {
        self.line(&format!(
            "{} {reason}",
            self.verb(&format!("{BOLD}{YELLOW}"), "Warning")
        ));
    }

    /// How many distinct classes a selection spans, for the opening line.
    pub(crate) fn class_count(cases: &[TestCase]) -> usize {
        let mut classes: Vec<&str> = cases.iter().map(TestCase::class).collect();
        classes.sort_unstable();
        classes.dedup();
        classes.len()
    }
}
