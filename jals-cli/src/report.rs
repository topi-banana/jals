//! Terminal-facing rendering for the `jals` CLI: rustfmt-style unified diffs on
//! stdout, and `ariadne`-rendered diagnostics on stderr.

use std::io::{IsTerminal, Write};
use std::ops::Range;

use ariadne::{Color, Config, IndexType, Label, Report, ReportKind, Source};
use jals_editor::{DiagnosticSeverity, FileDiagnostic};
use jals_fmt::FormatOutput;
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

    /// A byte range fit to display: parser ranges are often empty (`start == end`), which
    /// `ariadne` renders as an invisible caret, so widen those to one character (clamped to
    /// char boundaries, falling back to the preceding character at end-of-input).
    fn display_range(src: &str, range: &Range<usize>) -> Range<usize> {
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

    /// Render one diagnostic through `ariadne` to stderr, reusing `cache` (the parsed source).
    #[allow(clippy::too_many_arguments)]
    fn emit<'a>(
        cache: &mut (&'a str, Source<&'a str>),
        label: &'a str,
        src: &str,
        kind: ReportKind<'_>,
        color: Color,
        code: Option<&str>,
        message: &str,
        range: &Range<usize>,
        use_color: bool,
    ) {
        let span = Self::display_range(src, range);
        let config = Config::new()
            .with_color(use_color)
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
        let _ = builder.finish().eprint(&mut *cache);
    }

    /// Render every formatter warning for one source.
    ///
    /// A warning with a range is a parser syntax error and gets an `ariadne` report pointing at
    /// it. A warning without one is about the *configuration* — a rule that reads input
    /// whitespace being rounded to the single engine's canonical value — so it has nothing to
    /// point at and follows the CLI's plain `warning:` convention instead.
    pub(crate) fn report_format_warnings(label: &str, src: &str, out: &FormatOutput) {
        let use_color = Self::color_for(std::io::stderr().is_terminal());
        let mut cache = (label, Source::from(src));
        for w in &out.warnings {
            let Some(range) = &w.range else {
                eprintln!("warning: {}", w.message);
                continue;
            };
            Self::emit(
                &mut cache,
                label,
                src,
                ReportKind::Warning,
                Color::Yellow,
                None,
                &w.message,
                range,
                use_color,
            );
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

    /// Render one file's canonical diagnostics — syntax errors, lint findings, and unresolved
    /// symbols, already assembled and ordered by [`jals_editor::FileDiagnostics`] — through
    /// `ariadne`. Returns whether anything was reported.
    ///
    /// This is the CLI's whole share of the diagnostics seam: the policy (which passes run, what a
    /// broken parse suppresses, the order) belongs to `jals-editor`, and mapping each
    /// [`FileDiagnostic`] onto a terminal report is the part that is this host's.
    ///
    /// [`Hint`](DiagnosticSeverity::Hint) diagnostics are skipped, in the output *and* in the
    /// return value. They exist for hosts that can fade code in place (a dead branch, a
    /// `cfg`-disabled region); a terminal has nothing to fade, and `jals-lint`'s `unnecessary` /
    /// `unnecessary_range` docs already record that the CLI ignores them.
    pub(crate) fn report_diagnostics(
        label: &str,
        src: &str,
        diagnostics: &[FileDiagnostic],
    ) -> bool {
        let use_color = Self::color_for(std::io::stderr().is_terminal());
        let mut cache = (label, Source::from(src));
        let mut reported = false;
        for d in diagnostics {
            let (kind, color) = match d.severity {
                DiagnosticSeverity::Error => (ReportKind::Error, Color::Red),
                DiagnosticSeverity::Warning => (ReportKind::Warning, Color::Yellow),
                DiagnosticSeverity::Hint => continue,
            };
            reported = true;
            Self::emit(
                &mut cache,
                label,
                src,
                kind,
                color,
                // A syntax error carries no rule; it is reported under the name the linter's own
                // parser-error diagnostics have always used.
                Some(d.code.unwrap_or("syntax-error")),
                &d.message,
                &d.range,
                use_color,
            );
        }
        reported
    }
}
