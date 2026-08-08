//! Terminal-facing rendering for the `jals` CLI: rustfmt-style unified diffs on
//! stdout, and `ariadne`-rendered diagnostics on stderr.

use std::io::{IsTerminal, Write};
use std::ops::Range;

use ariadne::{Color, Config, IndexType, Label, Report, ReportKind, Source};
use jals_editor::{DiagnosticSeverity, FileDiagnostic};
use jals_fmt::FormatOutput;
use jals_project::{ProjectAnchor, ProjectDiagnostic};
use similar::{ChangeTag, TextDiff};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";

/// Terminal rendering for the CLI: rustfmt-style diffs on stdout and `ariadne`
/// diagnostics on stderr. A stateless namespace over the free-standing renderers.
pub(crate) struct Reporter;

impl Reporter {
    /// Whether ANSI color should be emitted to `stream` (a TTY with `NO_COLOR` unset).
    fn color_for(stream_is_tty: bool) -> bool {
        stream_is_tty && std::env::var_os("NO_COLOR").is_none()
    }

    fn paint(text: &str, code: &str, color: bool) -> String {
        if color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_owned()
        }
    }

    /// Print a rustfmt-style hunked diff of `original` → `formatted` to stdout, labelled
    /// with `label` (a file path or `<stdin>`). Does nothing if the two are identical.
    pub(crate) fn print_diff(label: &str, original: &str, formatted: &str) {
        if original == formatted {
            return;
        }
        let color = Self::color_for(std::io::stdout().is_terminal());
        let diff = TextDiff::from_lines(original, formatted);
        let mut out = std::io::stdout().lock();
        for group in diff.grouped_ops(3) {
            // 1-based line in the original where this hunk starts, à la rustfmt.
            let start = group.first().map_or(0, |op| op.old_range().start) + 1;
            let header = format!("Diff in {label} at line {start}:");
            let _ = writeln!(out, "{}", Self::paint(&header, BOLD, color));
            for op in &group {
                for change in diff.iter_changes(op) {
                    let value = change.value();
                    let line = value.strip_suffix('\n').unwrap_or(value);
                    let _ = match change.tag() {
                        ChangeTag::Delete => {
                            writeln!(out, "{}", Self::paint(&format!("-{line}"), RED, color))
                        }
                        ChangeTag::Insert => {
                            writeln!(out, "{}", Self::paint(&format!("+{line}"), GREEN, color))
                        }
                        ChangeTag::Equal => writeln!(out, " {line}"),
                    };
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
    pub(crate) fn report_format_warnings(label: &str, src: &str, out: &FormatOutput) {
        let mut doc = Doc::new(label, src);
        for w in &out.warnings {
            let Some(range) = &w.range else {
                eprintln!("warning: {}", w.message);
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
    pub(crate) fn report_format_fallback(label: &str, out: &FormatOutput) {
        if out.fell_back() {
            eprintln!(
                "warning: {label}: the formatter could not vouch for its output, so the file was \
                 left unchanged (this is a bug in jals-fmt, not in the source)",
            );
        }
    }

    /// Announce a migrated native formatter config on stderr, with any note it carried.
    ///
    /// Not an `ariadne` report: these have no source span to point at, and they belong to the run
    /// rather than to a file. They follow the CLI's plain `note:` / `warning:` convention.
    pub(crate) fn report_migration(migration: &crate::migrate::Migration) {
        let provenance = &migration.provenance;
        eprintln!(
            "note: migrating formatter settings from {} ({})",
            provenance.source, provenance.tool
        );
        for warning in &migration.warnings {
            eprintln!("warning: {warning}");
        }
    }

    /// Render one file's assembled diagnostics through `ariadne`, in the order
    /// [`jals_editor::FileDiagnostics`] produced them.
    ///
    /// Returns whether anything belongs in the problems list — that is, anything that is not a
    /// [`Hint`](DiagnosticSeverity::Hint). A hint is supplementary by definition (a `cfg`-disabled
    /// region, the dead branch of a constant condition); it is worth printing as `ariadne`
    /// *advice*, and it is not worth failing a run over.
    pub(crate) fn report_lint(label: &str, src: &str, diagnostics: &[FileDiagnostic]) -> bool {
        let mut doc = Doc::new(label, src);
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
    pub(crate) fn report_project(diagnostics: &[ProjectDiagnostic], script: Option<(&str, &str)>) {
        let mut doc = script.map(|(label, src)| Doc::new(label, src));
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
                    eprintln!("{lead}[{}]: {}", diagnostic.code, diagnostic.message);
                    // A `note:` line under the diagnostic is this channel's shape for a follow-on,
                    // the same one a migration note takes.
                    if let Some(remedy) = diagnostic.code.remedy() {
                        eprintln!("note: {remedy}");
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
    use_color: bool,
}

impl<'a> Doc<'a> {
    fn new(label: &'a str, src: &'a str) -> Self {
        Self {
            cache: (label, Source::from(src)),
            use_color: Reporter::color_for(std::io::stderr().is_terminal()),
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
            .with_color(self.use_color)
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
        let _ = builder.finish().eprint(&mut self.cache);
    }
}
