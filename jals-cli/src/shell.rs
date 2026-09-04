//! The CLI's one way out to a terminal.
//!
//! Everything the `jals` binary shows a person passes through a [`Shell`]: status lines, warnings,
//! errors, `ariadne` diagnostics, unified diffs, the test runner's own vocabulary, and the machine
//! output a script consumes. It is the only module in the crate allowed to name a print macro, and
//! `.ast-grep/rules/no-raw-print.yml` is what keeps that true rather than merely intended.
//!
//! Two rules live here and nowhere else:
//!
//! - **Human output goes to stderr, machine output to stdout.** `jals test` already stated this
//!   (`--list` and `--message-format json` are what a script reads, and a progress bar must never
//!   interleave with them); a shell that every command shares is what extends it to all of them.
//! - **One answer about colour.** Whether ANSI reaches a stream is decided once, per stream, from
//!   `--color`, the stream's TTY-ness, and the environment — instead of the two independent
//!   answers the crate used to carry.

use std::{
    fmt::Display,
    io::{IsTerminal, Write as _},
};

use clap::{ArgAction, Args, ValueEnum};
use indicatif::MultiProgress;

/// ANSI escapes. The workspace deliberately carries no styling crate; this is the whole palette.
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";

/// The column a status verb is right-aligned in, which is what makes a run's left edge a readable
/// column of verbs. Cargo's width, because the whole point is to look like it.
const VERB_WIDTH: usize = 12;

/// How much a run says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Verbosity {
    /// Warnings and errors only. No status lines, no bars.
    Quiet,
    /// Status lines and bars.
    Normal,
    /// Also the lines a normal run folds away: memo hits, individual fetches, the command a
    /// backend is about to run.
    Verbose,
}

/// When ANSI colour is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
#[value(rename_all = "lower")]
pub(crate) enum ColorWhen {
    /// Colour a terminal, and nothing else.
    #[default]
    Auto,
    Always,
    Never,
}

/// When a live progress display is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
#[value(rename_all = "lower")]
pub(crate) enum ProgressWhen {
    /// Draw when stderr is a terminal and the run is not quiet.
    #[default]
    Auto,
    Always,
    Never,
}

/// What the machine-readable half of a run looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
#[value(rename_all = "kebab-case")]
pub(crate) enum MessageFormat {
    /// Prose for a person; stdout carries only what a command was asked to produce.
    #[default]
    Human,
    /// One JSON object per line on stdout.
    Json,
}

/// Which renderings of the timing ledger a run writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub(crate) enum TimingsFormat {
    Html,
    Json,
}

/// The options every subcommand shares.
///
/// Global, so they may be written before or after the subcommand — `jals --quiet build` and
/// `jals build --quiet` are the same run, as they are in cargo.
#[derive(Debug, Clone, Args)]
pub(crate) struct OutputArgs {
    /// Print warnings and errors only.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub(crate) quiet: bool,
    /// Say more. Repeat for the whole event stream.
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub(crate) verbose: u8,
    /// When to use ANSI colour.
    #[arg(long, global = true, value_name = "WHEN", default_value_t = ColorWhen::Auto)]
    pub(crate) color: ColorWhen,
    /// How to render what a script reads.
    #[arg(long, global = true, value_name = "FORMAT", default_value_t = MessageFormat::Human)]
    pub(crate) message_format: MessageFormat,
    /// When to draw the live progress display.
    #[arg(long, global = true, value_name = "WHEN", default_value_t = ProgressWhen::Auto)]
    pub(crate) progress: ProgressWhen,
    /// Superseded by `--progress never`, kept so an existing invocation still runs.
    #[arg(long, global = true, hide = true)]
    pub(crate) hide_progress_bar: bool,
    /// Write a report of where the run's time went.
    #[arg(
        long,
        global = true,
        value_name = "FORMAT",
        num_args = 0..=1,
        default_missing_value = "html",
        value_delimiter = ','
    )]
    pub(crate) timings: Option<Vec<TimingsFormat>>,
}

impl OutputArgs {
    /// How much this run says.
    pub(crate) const fn verbosity(&self) -> Verbosity {
        if self.quiet {
            Verbosity::Quiet
        } else if self.verbose > 0 {
            Verbosity::Verbose
        } else {
            Verbosity::Normal
        }
    }
}

/// How a piece of text is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Style {
    /// Bold, uncoloured. The lead of something neutral.
    Plain,
    /// Bold green: something completed.
    Good,
    /// Bold yellow: something needs attention.
    Warn,
    /// Bold red: something failed.
    Bad,
    /// Bold cyan: something was set aside.
    Note,
    /// Dim: supporting text a reader skims past.
    Faint,
}

impl Style {
    const fn code(self) -> &'static str {
        match self {
            Self::Plain => BOLD,
            Self::Good => GREEN,
            Self::Warn => YELLOW,
            Self::Bad => RED,
            Self::Note => CYAN,
            Self::Faint => DIM,
        }
    }

    /// Whether the text is also bold. Every status verb is; supporting text is not.
    const fn bold(self) -> bool {
        !matches!(self, Self::Faint)
    }
}

/// The verb that leads a status line.
///
/// The CLI's vocabulary, conjugated from the `jals_progress::Activity` an emitter reported: a fact
/// says `Fetch`, and this says `Downloading` while it runs and `Downloaded` when it is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Preparing,
    Resolving,
    Downloading,
    Downloaded,
    Extracting,
    Remapping,
    Merging,
    Decompiling,
    Publishing,
    Indexing,
    Compiling,
    Packaging,
    Checking,
    Formatting,
    Testing,
    Running,
    Fresh,
    Removing,
    Created,
    Skipping,
    Timing,
    Finished,
}

impl Verb {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Preparing => "Preparing",
            Self::Resolving => "Resolving",
            Self::Downloading => "Downloading",
            Self::Downloaded => "Downloaded",
            Self::Extracting => "Extracting",
            Self::Remapping => "Remapping",
            Self::Merging => "Merging",
            Self::Decompiling => "Decompiling",
            Self::Publishing => "Publishing",
            Self::Indexing => "Indexing",
            Self::Compiling => "Compiling",
            Self::Packaging => "Packaging",
            Self::Checking => "Checking",
            Self::Formatting => "Formatting",
            Self::Testing => "Testing",
            Self::Running => "Running",
            Self::Fresh => "Fresh",
            Self::Removing => "Removing",
            Self::Created => "Created",
            Self::Skipping => "Skipping",
            Self::Timing => "Timing",
            Self::Finished => "Finished",
        }
    }

    const fn style(self) -> Style {
        match self {
            Self::Skipping => Style::Warn,
            _ => Style::Good,
        }
    }
}

/// The terminal a run writes to.
///
/// Shared behind an `Arc`: the progress sinks report from fan-out worker threads, so this is one of
/// the few things in the CLI that has to be `Sync`. It holds no lock of its own — `MultiProgress`
/// serializes its own drawing, and a line written outside one is a single `writeln!`.
pub(crate) struct Shell {
    verbosity: Verbosity,
    stdout_color: bool,
    stderr_color: bool,
    bars: Option<MultiProgress>,
}

impl Shell {
    /// Build the shell this run's options ask for.
    pub(crate) fn new(options: &OutputArgs) -> Self {
        let verbosity = options.verbosity();
        let stderr_tty = std::io::stderr().is_terminal();
        let draw = match options.progress {
            _ if options.hide_progress_bar => false,
            ProgressWhen::Never => false,
            ProgressWhen::Always => true,
            // A redirected run must produce the same bytes every time, so a bar needs a terminal
            // — and a quiet run has nothing to draw.
            ProgressWhen::Auto => stderr_tty && verbosity > Verbosity::Quiet,
        };
        Self {
            verbosity,
            stdout_color: Self::colored(options.color, std::io::stdout().is_terminal()),
            stderr_color: Self::colored(options.color, stderr_tty),
            bars: draw.then(MultiProgress::new),
        }
    }

    /// Whether ANSI reaches a stream.
    ///
    /// `NO_COLOR` disables and `CLICOLOR_FORCE` enables, both per the informal conventions those
    /// variables carry; `TERM=dumb` is a terminal that cannot render the escapes. `--color` is
    /// above all of them, because it is what the user said on this run.
    fn colored(choice: ColorWhen, stream_is_tty: bool) -> bool {
        match choice {
            ColorWhen::Always => true,
            ColorWhen::Never => false,
            ColorWhen::Auto => {
                if std::env::var_os("NO_COLOR").is_some() {
                    return false;
                }
                if std::env::var_os("CLICOLOR_FORCE").is_some_and(|value| value != "0") {
                    return true;
                }
                let dumb = std::env::var_os("TERM").is_some_and(|term| term == "dumb");
                stream_is_tty && !dumb
            }
        }
    }

    /// Whether stderr takes colour. Read by `ariadne`, which paints for itself.
    pub(crate) const fn stderr_color(&self) -> bool {
        self.stderr_color
    }

    /// The live display's drawing target, when there is one.
    pub(crate) const fn bars(&self) -> Option<&MultiProgress> {
        self.bars.as_ref()
    }

    /// Run `body` with the live display out of the way.
    ///
    /// For anything that writes to the terminal without going through this shell: `ariadne`, and a
    /// child process that inherits stdio.
    pub(crate) fn suspend<R>(&self, body: impl FnOnce() -> R) -> R {
        match &self.bars {
            Some(bars) => bars.suspend(body),
            None => body(),
        }
    }

    /// Take the live display down for good.
    ///
    /// Before spawning a program that owns the terminal from here on — `jals run`'s target, or a
    /// test with `--no-capture` — where suspending is not enough because the child never gives the
    /// terminal back.
    pub(crate) fn clear_progress(&self) {
        if let Some(bars) = &self.bars {
            let _ = bars.clear();
        }
    }

    /// A cargo-style status line: a right-aligned verb, then what it is about.
    pub(crate) fn status(&self, verb: Verb, message: impl Display) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        self.labelled(verb.label(), verb.style(), &message);
    }

    /// A status line only a `--verbose` run shows.
    pub(crate) fn verbose_status(&self, verb: Verb, message: impl Display) {
        if self.verbosity < Verbosity::Verbose {
            return;
        }
        self.labelled(verb.label(), verb.style(), &message);
    }

    /// A warning. Shown even when the run is quiet: quiet asks for less narration, not less news.
    pub(crate) fn warn(&self, message: impl Display) {
        self.stderr_line(&format_args!(
            "{}: {message}",
            self.paint("warning", Style::Warn)
        ));
    }

    /// An error.
    pub(crate) fn error(&self, message: impl Display) {
        self.stderr_line(&format_args!(
            "{}: {message}",
            self.paint("error", Style::Bad)
        ));
    }

    /// A follow-on line under whatever was just said.
    pub(crate) fn note(&self, message: impl Display) {
        self.stderr_line(&format_args!(
            "{}: {message}",
            self.paint("note", Style::Note)
        ));
    }

    /// One line of prose to stderr, exactly as given.
    ///
    /// For the blocks that are already assembled by the time they get here: a captured test's
    /// replayed output, a summary rule. Not gated on verbosity — the caller has decided.
    pub(crate) fn plain(&self, text: impl Display) {
        self.stderr_line(&text);
    }

    /// One line of machine output to stdout.
    ///
    /// Takes `&self` although it reads nothing from the shell: every write in this crate goes
    /// through a shell, and an associated function would be the one output a caller could reach
    /// without holding one.
    #[allow(clippy::unused_self)] // See above: the receiver is the funnel, not a data dependency.
    pub(crate) fn machine(&self, line: impl Display) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
    }

    /// Bytes to stdout, unchanged.
    ///
    /// `jals fmt` reading stdin writes the formatted source and nothing else, so this is the one
    /// output in the crate that is not line-oriented — and the one whose failure the caller has to
    /// see, because a truncated formatted file is not a formatted file.
    #[allow(clippy::unused_self)] // As `machine`: the receiver is the funnel.
    pub(crate) fn machine_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut out = std::io::stdout().lock();
        out.write_all(bytes)
    }

    /// A label right-aligned in the verb column and painted.
    ///
    /// The lead of a line something else assembles: a bar's prefix, and `jals test`'s own
    /// `PASS`/`FAIL` vocabulary — which stays that command's because `cargo nextest` is what it is
    /// modelled on, while the alignment and the colour decision stay here.
    pub(crate) fn pad(&self, label: &str, style: Style) -> String {
        self.paint(&format!("{label:>VERB_WIDTH$}"), style)
    }

    /// `text` wrapped in this run's escapes for stderr, or unchanged when it does not paint.
    pub(crate) fn paint(&self, text: &str, style: Style) -> String {
        if !self.stderr_color {
            return text.to_owned();
        }
        let bold = if style.bold() { BOLD } else { "" };
        format!("{bold}{}{text}{RESET}", style.code())
    }

    /// `text` wrapped in this run's escapes for stdout.
    ///
    /// Separate from [`paint`](Self::paint) because the two streams are answered separately: a diff
    /// piped to a file must not carry escapes because the terminal it did not go to would have.
    pub(crate) fn paint_machine(&self, text: &str, style: Style) -> String {
        if !self.stdout_color {
            return text.to_owned();
        }
        let bold = if style.bold() { BOLD } else { "" };
        format!("{bold}{}{text}{RESET}", style.code())
    }

    fn labelled(&self, label: &str, style: Style, message: &dyn Display) {
        // Pad before painting: escapes are zero-width to a terminal and very wide to `{:>12}`.
        self.stderr_line(&format_args!("{} {message}", self.pad(label, style)));
    }

    fn stderr_line(&self, text: &dyn Display) {
        self.suspend(|| {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "{text}");
        });
    }
}

impl Display for ColorWhen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        })
    }
}

impl Display for ProgressWhen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        })
    }
}

impl Display for MessageFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Human => "human",
            Self::Json => "json",
        })
    }
}
