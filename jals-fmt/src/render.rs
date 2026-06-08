//! Render a [`Doc`] to a formatted string.
//!
//! A worklist of `(indent, mode, doc)` is processed LIFO. Each [`Doc::Group`] is rendered
//! flat when [`fits`] says the flat layout stays within `max_width`, otherwise broken.
//!
//! Line breaks are *coalesced*: a break is only realized when content follows it, taking
//! the strongest pending break (a blank line beats a plain break) and the indentation of
//! the last break. This makes redundant breaks (e.g. a structural break plus a comment's
//! own break) collapse to one, and guarantees no trailing whitespace — both essential for
//! idempotency. Trailing comments ride on [`Doc::LineSuffix`] so they stay on their line.

use unicode_width::UnicodeWidthStr;

use crate::config::Config;
use crate::doc::{CommentKind, Doc};
use crate::wrap;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

#[derive(Clone, Copy)]
struct Cmd<'a> {
    indent: usize,
    mode: Mode,
    doc: &'a Doc,
}

/// Tracks where the next content will land: either mid-line, or after a pending run of
/// line breaks (which are only written once content arrives).
struct Out<'c> {
    cfg: &'c Config,
    /// The resolved line terminator (`Auto`/`Native` already decided against the input).
    newline: &'static str,
    buf: String,
    col: usize,
    /// Whether the current line already has visible content.
    line_has_content: bool,
    /// Newlines buffered before the next content (0 = none, 1 = plain break, `n` = `n - 1`
    /// blank lines).
    pending_newlines: usize,
    /// Indentation level for the line after the pending break.
    pending_indent: usize,
}

impl Out<'_> {
    /// Buffer a line break before the next content. `blank_lines` is the run of blank lines
    /// the source had (0 for a plain break); it is clamped to `max_blank_lines` here, making
    /// the renderer the single place that enforces the blank-line policy.
    fn request_break(&mut self, indent: usize, blank_lines: usize) {
        let n = blank_lines.min(self.cfg.max_blank_lines) + 1;
        self.pending_newlines = self.pending_newlines.max(n);
        self.pending_indent = indent;
    }

    fn flush_break(&mut self) {
        if self.pending_newlines == 0 {
            return;
        }
        trim_trailing_blanks(&mut self.buf);
        // Number of '\n' still to write: when the line already has content we write all of
        // them; when we are already at line start one '\n' is implicitly present. At the
        // very start of the output there is no preceding line at all, so leading blank
        // lines (e.g. from a blank-before comment on the first item) are suppressed.
        let already = if self.line_has_content { 0 } else { 1 };
        let newlines = if self.buf.is_empty() {
            0
        } else {
            self.pending_newlines.saturating_sub(already)
        };
        for _ in 0..newlines {
            self.buf.push_str(self.newline);
        }
        push_indent(&mut self.buf, self.pending_indent, self.cfg);
        self.col = self.pending_indent * self.cfg.indent_cols();
        self.pending_newlines = 0;
        self.line_has_content = false;
    }

    /// Whether the next content would land at the start of a line (a pending break, or a
    /// line that has only indentation so far). A separator space there is meaningless.
    fn at_line_start(&self) -> bool {
        self.pending_newlines > 0 || !self.line_has_content
    }

    fn text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        // Drop a separator space that would sit at the start of a line.
        if s == " " && self.at_line_start() {
            return;
        }
        self.flush_break();
        self.buf.push_str(s);
        self.col += UnicodeWidthStr::width(s);
        self.line_has_content = true;
    }

    fn raw(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.flush_break();
        self.buf.push_str(s);
        self.col = match s.rfind('\n') {
            Some(pos) => UnicodeWidthStr::width(&s[pos + 1..]),
            None => self.col + UnicodeWidthStr::width(s),
        };
        self.line_has_content = true;
    }

    fn space(&mut self) {
        if self.at_line_start() {
            return;
        }
        self.buf.push(' ');
        self.col += 1;
        self.line_has_content = true;
    }
}

/// Render `root` into a formatted string. `src` is the original input, consulted once to
/// resolve an `Auto`/`Native` line ending.
pub(crate) fn print(root: &Doc, cfg: &Config, src: &str) -> String {
    let newline = cfg.newline(src);
    let mut out = Out {
        cfg,
        newline,
        buf: String::new(),
        col: 0,
        line_has_content: false,
        pending_newlines: 0,
        pending_indent: 0,
    };
    let mut suffixes: Vec<Cmd<'_>> = Vec::new();
    let mut stack: Vec<Cmd<'_>> = vec![Cmd {
        indent: 0,
        mode: Mode::Break,
        doc: root,
    }];

    while let Some(cmd) = stack.pop() {
        let Cmd { indent, mode, doc } = cmd;
        match doc {
            Doc::Text(s) => out.text(s),
            Doc::RawText(s) => out.raw(s),
            Doc::Comment { kind, text } => render_comment(&mut out, indent, *kind, text),
            Doc::Concat(v) => {
                for d in v.iter().rev() {
                    stack.push(Cmd {
                        indent,
                        mode,
                        doc: d,
                    });
                }
            }
            Doc::Indent(d) => stack.push(Cmd {
                indent: indent + 1,
                mode,
                doc: d,
            }),
            Doc::Group { doc, should_break } => {
                let m = if *should_break || !fits(&out, indent, doc, &stack) {
                    Mode::Break
                } else {
                    Mode::Flat
                };
                stack.push(Cmd {
                    indent,
                    mode: m,
                    doc,
                });
            }
            Doc::Line => {
                if mode == Mode::Flat {
                    out.space();
                } else if !flush_suffixes(&mut stack, &mut suffixes, cmd) {
                    out.request_break(indent, 0);
                }
            }
            Doc::SoftLine => {
                if mode == Mode::Break && !flush_suffixes(&mut stack, &mut suffixes, cmd) {
                    out.request_break(indent, 0);
                }
            }
            Doc::HardLine => {
                if !flush_suffixes(&mut stack, &mut suffixes, cmd) {
                    out.request_break(indent, 0);
                }
            }
            Doc::BlankLine(blank_lines) => {
                if !flush_suffixes(&mut stack, &mut suffixes, cmd) {
                    out.request_break(indent, *blank_lines);
                }
            }
            Doc::LineSuffix(d) => suffixes.push(Cmd {
                indent,
                mode,
                doc: d,
            }),
        }
    }

    // Render any line suffixes left at the very end.
    for s in suffixes.drain(..).rev() {
        stack.push(s);
    }
    while let Some(cmd) = stack.pop() {
        match cmd.doc {
            Doc::Text(s) => out.text(s),
            Doc::RawText(s) => out.raw(s),
            Doc::Comment { kind, text } => render_comment(&mut out, cmd.indent, *kind, text),
            Doc::Concat(v) => {
                for d in v.iter().rev() {
                    stack.push(Cmd { doc: d, ..cmd });
                }
            }
            _ => {}
        }
    }

    finalize(out.buf, cfg, newline)
}

/// Push the current break command back and queue pending line suffixes ahead of it.
/// Returns `true` when suffixes were flushed (the caller should not request the break yet).
fn flush_suffixes<'a>(
    stack: &mut Vec<Cmd<'a>>,
    suffixes: &mut Vec<Cmd<'a>>,
    current: Cmd<'a>,
) -> bool {
    if suffixes.is_empty() {
        return false;
    }
    stack.push(current);
    while let Some(s) = suffixes.pop() {
        stack.push(s);
    }
    true
}

/// Emit a comment at indentation level `indent`. With `wrap-comments` off this reproduces
/// the verbatim rendering (line comments as text, block comments raw); with it on the
/// comment is reflowed to `comment-width` at this indentation.
fn render_comment(out: &mut Out<'_>, indent: usize, kind: CommentKind, text: &str) {
    if !out.cfg.wrap_comments {
        match kind {
            CommentKind::Line => out.text(text),
            CommentKind::Block | CommentKind::Doc => out.raw(text),
        }
        return;
    }
    let indent_str = out.cfg.indent_unit().repeat(indent);
    let indent_cols = indent * out.cfg.indent_cols();
    let newline = out.newline;
    let width = out.cfg.comment_width;
    let reflowed = match kind {
        CommentKind::Line => wrap::reflow_line(text, &indent_str, indent_cols, newline, width),
        CommentKind::Block => {
            wrap::reflow_block(text, false, &indent_str, indent_cols, newline, width)
        }
        CommentKind::Doc => {
            wrap::reflow_block(text, true, &indent_str, indent_cols, newline, width)
        }
    };
    out.raw(&reflowed);
}

fn push_indent(out: &mut String, indent: usize, cfg: &Config) {
    let unit = cfg.indent_unit();
    for _ in 0..indent {
        out.push_str(&unit);
    }
}

/// Remove trailing spaces and tabs (renderer-added; never part of a `RawText` token,
/// which always ends in a delimiter such as `"` or `*/`).
fn trim_trailing_blanks(out: &mut String) {
    while matches!(out.as_bytes().last(), Some(b' ' | b'\t')) {
        out.pop();
    }
}

/// Apply the final-newline policy.
fn finalize(mut out: String, cfg: &Config, newline: &str) -> String {
    trim_trailing_blanks(&mut out);
    while out.ends_with('\n') {
        out.pop();
        if out.ends_with('\r') {
            out.pop();
        }
        trim_trailing_blanks(&mut out);
    }
    if cfg.insert_final_newline && !out.is_empty() {
        out.push_str(newline);
    }
    out
}

/// Whether `group_doc`, rendered flat starting at the current column, fits within
/// `max_width` together with the content that follows on the same line.
fn fits(out: &Out<'_>, indent: usize, group_doc: &Doc, rest: &[Cmd<'_>]) -> bool {
    let start_col = if out.pending_newlines > 0 {
        out.pending_indent * out.cfg.indent_cols()
    } else {
        out.col
    };
    let mut remaining = out.cfg.max_width as isize - start_col as isize;
    if remaining < 0 {
        return false;
    }
    let mut work: Vec<(usize, Mode, &Doc)> = Vec::with_capacity(rest.len() + 1);
    for cmd in rest {
        work.push((cmd.indent, cmd.mode, cmd.doc));
    }
    work.push((indent, Mode::Flat, group_doc));

    while let Some((ind, mode, doc)) = work.pop() {
        match doc {
            Doc::Text(s) => {
                remaining -= UnicodeWidthStr::width(&**s) as isize;
                if remaining < 0 {
                    return false;
                }
            }
            Doc::RawText(s) => {
                if let Some(pos) = s.find('\n') {
                    remaining -= UnicodeWidthStr::width(&s[..pos]) as isize;
                    return remaining >= 0;
                }
                remaining -= UnicodeWidthStr::width(&**s) as isize;
                if remaining < 0 {
                    return false;
                }
            }
            // Measure a comment by its first line, like `RawText`. Exact when reflow is off;
            // when on, comments are surrounded by forced breaks so this never drives layout.
            Doc::Comment { text, .. } => {
                if let Some(pos) = text.find('\n') {
                    remaining -= UnicodeWidthStr::width(&text[..pos]) as isize;
                    return remaining >= 0;
                }
                remaining -= UnicodeWidthStr::width(&**text) as isize;
                if remaining < 0 {
                    return false;
                }
            }
            Doc::Concat(v) => {
                for d in v.iter().rev() {
                    work.push((ind, mode, d));
                }
            }
            Doc::Indent(d) => work.push((ind + 1, mode, d)),
            Doc::Group { doc, should_break } => {
                let m = if *should_break {
                    Mode::Break
                } else {
                    Mode::Flat
                };
                work.push((ind, m, doc));
            }
            Doc::Line => match mode {
                Mode::Flat => {
                    remaining -= 1;
                    if remaining < 0 {
                        return false;
                    }
                }
                Mode::Break => return true,
            },
            Doc::SoftLine => {
                if mode == Mode::Break {
                    return true;
                }
            }
            Doc::HardLine | Doc::BlankLine(_) => return true,
            Doc::LineSuffix(_) => {}
        }
    }
    remaining >= 0
}
