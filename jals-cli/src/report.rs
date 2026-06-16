//! Terminal-facing rendering for the `jals` CLI: rustfmt-style unified diffs on
//! stdout, and `ariadne`-rendered diagnostics on stderr.

use std::io::{IsTerminal, Write};
use std::ops::Range;

use ariadne::{Color, Config, IndexType, Label, Report, ReportKind, Source};
use jals_fmt::FormatOutput;
use jals_lint::{LintOutput, Severity};
use similar::{ChangeTag, TextDiff};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";

/// Whether ANSI color should be emitted to `stream` (a TTY with `NO_COLOR` unset).
fn color_for(stream_is_tty: bool) -> bool {
    stream_is_tty && std::env::var_os("NO_COLOR").is_none()
}

fn paint(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Print a rustfmt-style hunked diff of `original` → `formatted` to stdout, labelled
/// with `label` (a file path or `<stdin>`). Does nothing if the two are identical.
pub(crate) fn print_diff(label: &str, original: &str, formatted: &str) {
    if original == formatted {
        return;
    }
    let color = color_for(std::io::stdout().is_terminal());
    let diff = TextDiff::from_lines(original, formatted);
    let mut out = std::io::stdout().lock();
    for group in diff.grouped_ops(3) {
        // 1-based line in the original where this hunk starts, à la rustfmt.
        let start = group.first().map_or(0, |op| op.old_range().start) + 1;
        let header = format!("Diff in {label} at line {start}:");
        let _ = writeln!(out, "{}", paint(&header, BOLD, color));
        for op in &group {
            for change in diff.iter_changes(op) {
                let value = change.value();
                let line = value.strip_suffix('\n').unwrap_or(value);
                let _ = match change.tag() {
                    ChangeTag::Delete => {
                        writeln!(out, "{}", paint(&format!("-{line}"), RED, color))
                    }
                    ChangeTag::Insert => {
                        writeln!(out, "{}", paint(&format!("+{line}"), GREEN, color))
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
    let span = display_range(src, range);
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

/// Render every formatter warning (parser syntax errors) for one source through `ariadne`.
pub(crate) fn report_format_warnings(label: &str, src: &str, out: &FormatOutput) {
    let use_color = color_for(std::io::stderr().is_terminal());
    let mut cache = (label, Source::from(src));
    for w in &out.warnings {
        emit(
            &mut cache,
            label,
            src,
            ReportKind::Warning,
            Color::Yellow,
            None,
            &w.message,
            &w.range,
            use_color,
        );
    }
}

/// Render every lint diagnostic (and parser error) for one source through `ariadne`.
/// Returns whether anything was reported.
pub(crate) fn report_lint(label: &str, src: &str, out: &LintOutput) -> bool {
    let use_color = color_for(std::io::stderr().is_terminal());
    let mut cache = (label, Source::from(src));
    for d in out.diagnostics.iter().chain(&out.parse_errors) {
        let (kind, color) = match d.severity {
            Severity::Error => (ReportKind::Error, Color::Red),
            _ => (ReportKind::Warning, Color::Yellow),
        };
        emit(
            &mut cache,
            label,
            src,
            kind,
            color,
            Some(d.rule),
            &d.message,
            &d.range,
            use_color,
        );
    }
    out.has_diagnostics()
}
