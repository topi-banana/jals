//! Diagnostic rendering for the `jals` CLI: rustfmt-style unified diffs, and `ariadne`-rendered
//! reports for anything that points at a source span.
//!
//! Every byte written here leaves through [`Shell`] — including `ariadne`'s, which writes to stderr
//! itself and is therefore wrapped in [`Shell::suspend`] so a live progress bar is out of its way.
//! Colour is the shell's answer too: this module used to keep a second ANSI palette and a second
//! TTY test, which is exactly how a `--color` flag ends up being honoured in one half of a run.

use std::ops::Range;

use ariadne::{Color, Config, IndexType, Label, Report, ReportKind, Source};
use jals_editor::{DiagnosticSeverity, FileDiagnostic};
use jals_fmt::FormatOutput;
use jals_project::{ProjectAnchor, ProjectDiagnostic};
use similar::{ChangeTag, TextDiff};

use crate::shell::{Shell, Style};

/// Diagnostic rendering for the CLI. A stateless namespace over the free-standing renderers.
pub(crate) struct Reporter;

impl Reporter {
    /// Print a rustfmt-style hunked diff of `original` → `formatted` to stdout, labelled
    /// with `label` (a file path or `<stdin>`). Does nothing if the two are identical.
    ///
    /// Stdout, because a diff is what `--diff` was asked to produce — the one human-shaped thing in
    /// this crate that is also the command's output.
    pub(crate) fn print_diff(shell: &Shell, label: &str, original: &str, formatted: &str) {
        if original == formatted {
            return;
        }
        let diff = TextDiff::from_lines(original, formatted);
        for group in diff.grouped_ops(3) {
            // 1-based line in the original where this hunk starts, à la rustfmt.
            let start = group.first().map_or(0, |op| op.old_range().start) + 1;
            let header = format!("Diff in {label} at line {start}:");
            shell.machine(shell.paint_machine(&header, Style::Plain));
            for op in &group {
                for change in diff.iter_changes(op) {
                    let value = change.value();
                    let line = value.strip_suffix('\n').unwrap_or(value);
                    match change.tag() {
                        ChangeTag::Delete => {
                            shell.machine(shell.paint_machine(&format!("-{line}"), Style::Bad));
                        }
                        ChangeTag::Insert => {
                            shell.machine(shell.paint_machine(&format!("+{line}"), Style::Good));
                        }
                        ChangeTag::Equal => shell.machine(format_args!(" {line}")),
                    }
                }
            }
        }
    }

    /// Render every formatter warning for one source.
    ///
    /// A warning with a range is a parser syntax error and gets an `ariadne` report pointing at
    /// it. A warning without one is about the *configuration* — a rule that reads input
    /// whitespace being rounded to the single engine's canonical value — so it has nothing to
    /// point at and follows the CLI's plain `warning:` convention instead.
    pub(crate) fn report_format_warnings(
        shell: &Shell,
        label: &str,
        src: &str,
        out: &FormatOutput,
    ) {
        let mut doc = Doc::new(shell, label, src);
        for w in &out.warnings {
            let Some(range) = &w.range else {
                shell.warn(&w.message);
                continue;
            };
            doc.emit(DiagnosticSeverity::Warning, None, &w.message, range);
        }
    }

    /// Announce that the formatter refused its own output for one source.
    ///
    /// Not an `ariadne` report and not a `Warning`: the fail-safe compares the whole file against
    /// the whole file, so there is no span to point at, and `jals-fmt` keeps this off its warning
    /// list on purpose (a range-less warning means something else there). It follows the CLI's plain
    /// `warning:` convention like the other file-less diagnostics.
    ///
    /// Worth saying out loud rather than leaving to the exit code, because the symptom is *absence*:
    /// the file comes back byte-identical, so without this line the run looks like a run that found
    /// nothing to do.
    pub(crate) fn report_format_fallback(shell: &Shell, label: &str, out: &FormatOutput) {
        if out.fell_back() {
            shell.warn(format_args!(
                "{label}: the formatter could not vouch for its output, so the file was left \
                 unchanged (this is a bug in jals-fmt, not in the source)",
            ));
        }
    }

    /// Announce the `jalslint.toml` keys this jals does not define.
    ///
    /// An unknown key is **kept**, so one stale name cannot stop the rest of the file from being
    /// read (`jals_config::lint::UnknownKeys` gives the reason). Keeping it silently would be the
    /// other failure — a rule the file plainly configures, doing nothing — so it is said out loud
    /// here, on the CLI's plain `warning:` convention: the key has no source span, and it belongs
    /// to the run rather than to any reported file.
    ///
    /// Deliberately not a finding: it does not set the exit code, because the file being linted is
    /// not the file with the problem.
    pub(crate) fn report_unknown_lint_keys(shell: &Shell, label: &str, keys: &[String]) {
        for key in keys {
            shell.warn(format_args!("{label}: unknown lint key `{key}`"));
        }
    }

    /// Announce a migrated native formatter config on stderr, with any note it carried.
    ///
    /// Not an `ariadne` report: these have no source span to point at, and they belong to the run
    /// rather than to a file. They follow the CLI's plain `note:` / `warning:` convention.
    pub(crate) fn report_migration(shell: &Shell, migration: &crate::migrate::Migration) {
        let provenance = &migration.provenance;
        shell.note(format_args!(
            "migrating formatter settings from {} ({})",
            provenance.source, provenance.tool
        ));
        for warning in &migration.warnings {
            shell.warn(warning);
        }
    }

    /// Render one file's assembled diagnostics through `ariadne`, in the order
    /// [`jals_editor::FileDiagnostics`] produced them.
    ///
    /// Returns whether anything belongs in the problems list — that is, anything that is not a
    /// [`Hint`](DiagnosticSeverity::Hint). A hint is supplementary by definition (a `cfg`-disabled
    /// region, the dead branch of a constant condition); it is worth printing as `ariadne`
    /// *advice*, and it is not worth failing a run over.
    pub(crate) fn report_lint(
        shell: &Shell,
        label: &str,
        src: &str,
        diagnostics: &[FileDiagnostic],
    ) -> bool {
        let mut doc = Doc::new(shell, label, src);
        for d in diagnostics {
            doc.emit(d.severity, d.code, &d.message, &d.range);
        }
        diagnostics
            .iter()
            .any(|d| d.severity != DiagnosticSeverity::Hint)
    }

    /// Render the project-assembly diagnostics for one run.
    ///
    /// `script` is the configured build script as `(label, source)`, when the caller has it: a
    /// diagnostic that resolved a span inside it points at the offending line through `ariadne`,
    /// exactly as a lint finding does. Everything else has no span in this project's tree — a
    /// dependency failure names a node, not a file here — so it follows the CLI's plain
    /// `error:` / `warning:` / `note:` convention, the same rule
    /// [`report_format_warnings`](Self::report_format_warnings) applies to a range-less warning.
    ///
    /// This host reads [`ProjectDiagnostic::span`] rather than `placement_in`: a terminal line can
    /// say "no location", so a diagnostic that has none gets none. Pointing `ariadne` at the head
    /// of `jals.toml` instead would draw a caret at a place a dependency failure is not.
    pub(crate) fn report_project(
        shell: &Shell,
        diagnostics: &[ProjectDiagnostic],
        script: Option<(&str, &str)>,
    ) {
        let mut doc = script.map(|(label, src)| Doc::new(shell, label, src));
        for diagnostic in diagnostics {
            // The assembly owns how one of these presents; this channel draws a `Hint` as
            // `ariadne` advice, and the plain lead below spells the same thing `note:`.
            let severity = DiagnosticSeverity::from(diagnostic.severity);
            match (&diagnostic.span, &mut doc) {
                (Some(span), Some(doc))
                    if matches!(diagnostic.anchor, ProjectAnchor::Script(_)) =>
                {
                    doc.emit(
                        severity,
                        Some(diagnostic.code.as_str()),
                        &diagnostic.message,
                        span,
                    );
                }
                _ => {
                    let lead = diagnostic.severity.lead();
                    // The code carries what a span would have shown: a message names its subject
                    // but not which part of the procedure produced it, and `warning: no toolchain`
                    // does not say whether the build script or the dependency graph said so. This
                    // is the same code `ariadne` prints for the arm above.
                    //
                    // The lead is the assembly's, so this is `plain` rather than `warn`/`error`:
                    // re-deriving a severity the diagnostic already states is how a warning starts
                    // reading as an error.
                    shell.plain(format_args!(
                        "{lead}[{}]: {}",
                        diagnostic.code, diagnostic.message
                    ));
                    // A `note:` line under the diagnostic is this channel's shape for a follow-on,
                    // the same one a migration note takes.
                    if let Some(remedy) = diagnostic.code.remedy() {
                        shell.note(remedy);
                    }
                }
            }
        }
    }
}

/// One source being reported on: the `ariadne` cache keyed by its label, and whether this run
/// paints.
///
/// The two travel together for the whole of a file's report, which is why they are one value rather
/// than parameters repeated at every emission.
struct Doc<'a> {
    /// `ariadne`'s `Cache`: the label a span is resolved against, and the parsed source.
    cache: (&'a str, Source<&'a str>),
    shell: &'a Shell,
}

impl<'a> Doc<'a> {
    fn new(shell: &'a Shell, label: &'a str, src: &'a str) -> Self {
        Self {
            cache: (label, Source::from(src)),
            shell,
        }
    }

    /// A byte range fit to display: parser ranges are often empty (`start == end`), which
    /// `ariadne` renders as an invisible caret, so widen those to one character (clamped to
    /// char boundaries, falling back to the preceding character at end-of-input).
    fn display_range(&self, range: &Range<usize>) -> Range<usize> {
        let src = self.cache.1.text();
        if range.start != range.end {
            return range.clone();
        }
        let at = range.start.min(src.len());
        if at < src.len() {
            let mut end = at + 1;
            while end < src.len() && !src.is_char_boundary(end) {
                end += 1;
            }
            at..end
        } else if at > 0 {
            let mut start = at - 1;
            while start > 0 && !src.is_char_boundary(start) {
                start -= 1;
            }
            start..at
        } else {
            0..0
        }
    }

    /// Render one diagnostic to stderr.
    ///
    /// The `ariadne` kind and color are derived from `severity` here and nowhere else, so this is
    /// the CLI's whole answer to "how does a severity look". `code` is optional because a syntax
    /// error has no producing rule to name.
    fn emit(
        &mut self,
        severity: DiagnosticSeverity,
        code: Option<&str>,
        message: &str,
        range: &Range<usize>,
    ) {
        let (kind, color) = match severity {
            DiagnosticSeverity::Error => (ReportKind::Error, Color::Red),
            DiagnosticSeverity::Warning => (ReportKind::Warning, Color::Yellow),
            // Advice, not a warning: a hint is not a problem, and the exit code agrees.
            DiagnosticSeverity::Hint => (ReportKind::Advice, Color::Cyan),
        };
        let span = self.display_range(range);
        let label = self.cache.0;
        let config = Config::new()
            .with_color(self.shell.stderr_color())
            .with_index_type(IndexType::Byte);
        let mut builder = Report::build(kind, (label, span.clone()))
            .with_config(config)
            .with_message(message)
            .with_label(
                Label::new((label, span))
                    .with_message(message)
                    .with_color(color),
            );
        if let Some(code) = code {
            builder = builder.with_code(code);
        }
        // `ariadne` writes to stderr itself, so the bar has to be out of the way around it.
        let report = builder.finish();
        self.shell.suspend(|| {
            let _ = report.eprint(&mut self.cache);
        });
    }
}
