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

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use unicode_width::UnicodeWidthStr;

use crate::config::{Config, IndentStyle};
use crate::doc::{CommentKind, Doc};
use crate::wrap::Wrap;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

#[derive(Clone, Copy)]
struct Cmd<'a> {
    /// Indentation in display columns (not levels): a [`Doc::Indent`] adds `indent_cols()`, a
    /// [`Doc::ContinuationIndent`] / broken [`Doc::IndentIfBreak`] adds `continuation_cols()`.
    indent: usize,
    mode: Mode,
    doc: &'a Doc,
}

/// Tracks where the next content will land: either mid-line, or after a pending run of
/// line breaks (which are only written once content arrives).
pub(crate) struct Out<'c> {
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
    /// Indentation (in display columns) for the line after the pending break.
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
        Self::trim_trailing_blanks(&mut self.buf);
        // Number of '\n' still to write: when the line already has content we write all of
        // them; when we are already at line start one '\n' is implicitly present. At the
        // very start of the output there is no preceding line at all, so leading blank
        // lines (e.g. from a blank-before comment on the first item) are suppressed.
        let already = usize::from(!self.line_has_content);
        let newlines = if self.buf.is_empty() {
            0
        } else {
            self.pending_newlines.saturating_sub(already)
        };
        for _ in 0..newlines {
            self.buf.push_str(self.newline);
        }
        Self::push_indent(&mut self.buf, self.pending_indent, self.cfg);
        self.col = self.pending_indent;
        self.pending_newlines = 0;
        self.line_has_content = false;
    }

    /// Whether the next content would land at the start of a line (a pending break, or a
    /// line that has only indentation so far). A separator space there is meaningless.
    const fn at_line_start(&self) -> bool {
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

impl<'c> Out<'c> {
    /// Render `root` into a formatted string. `src` is the original input, consulted once to
    /// resolve an `Auto`/`Native` line ending.
    pub(crate) async fn print(root: &Doc, cfg: &'c Config, src: &str) -> String {
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

        let newline = cfg.newline(src);
        let mut out = Self {
            cfg,
            newline,
            buf: String::new(),
            col: 0,
            line_has_content: false,
            pending_newlines: 0,
            pending_indent: 0,
        };
        let mut yielder = jals_exec::Yielder::new();
        let mut suffixes: Vec<Cmd<'_>> = Vec::new();
        let mut stack: Vec<Cmd<'_>> = vec![Cmd {
            indent: 0,
            mode: Mode::Break,
            doc: root,
        }];

        while let Some(cmd) = stack.pop() {
            yielder.tick().await;
            let Cmd { indent, mode, doc } = cmd;
            match doc {
                Doc::Text(s) => out.text(s),
                Doc::RawText(s) => out.raw(s),
                Doc::Comment { kind, text } => out.render_comment(indent, *kind, text),
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
                    indent: indent + out.cfg.indent_cols(),
                    mode,
                    doc: d,
                }),
                Doc::ContinuationIndent(d) => stack.push(Cmd {
                    indent: indent + out.cfg.continuation_cols(),
                    mode,
                    doc: d,
                }),
                Doc::IndentIfBreak(d) => stack.push(Cmd {
                    indent: if mode == Mode::Break {
                        indent + out.cfg.continuation_cols()
                    } else {
                        indent
                    },
                    mode,
                    doc: d,
                }),
                Doc::Group { doc, should_break } => {
                    let m = if *should_break || !out.fits(indent, doc, &stack) {
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
                Doc::IfBreak { broken, flat } => stack.push(Cmd {
                    indent,
                    mode,
                    doc: if mode == Mode::Break { broken } else { flat },
                }),
                Doc::Fill(parts) => out.render_fill(indent, parts, &mut stack),
            }
        }

        // Render any line suffixes left at the very end.
        for s in suffixes.into_iter().rev() {
            stack.push(s);
        }
        while let Some(cmd) = stack.pop() {
            yielder.tick().await;
            match cmd.doc {
                Doc::Text(s) => out.text(s),
                Doc::RawText(s) => out.raw(s),
                Doc::Comment { kind, text } => out.render_comment(cmd.indent, *kind, text),
                Doc::Concat(v) => {
                    for d in v.iter().rev() {
                        stack.push(Cmd { doc: d, ..cmd });
                    }
                }
                _ => {}
            }
        }

        Self::finalize(out.buf, cfg, newline)
    }

    /// Emit a comment at indentation level `indent`. With `wrap-comments` off this reproduces
    /// the verbatim rendering (line comments as text, block comments raw); with it on the
    /// comment is reflowed to `comment-width` at this indentation.
    fn render_comment(&mut self, indent: usize, kind: CommentKind, text: &str) {
        if !self.cfg.wrap_comments {
            match kind {
                CommentKind::Line => self.text(text),
                CommentKind::Block | CommentKind::Doc => self.raw(text),
            }
            return;
        }
        let indent_str = Self::indent_string(indent, self.cfg);
        let indent_cols = indent;
        let newline = self.newline;
        let width = self.cfg.comment_width;
        let reflowed = match kind {
            CommentKind::Line => Wrap::reflow_line(text, &indent_str, indent_cols, newline, width),
            CommentKind::Block => {
                Wrap::reflow_block(text, false, &indent_str, indent_cols, newline, width)
            }
            CommentKind::Doc => {
                Wrap::reflow_block(text, true, &indent_str, indent_cols, newline, width)
            }
        };
        self.raw(&reflowed);
    }

    /// Build the leading-whitespace string for an indentation of `cols` display columns. In space
    /// style this is `cols` spaces; in tab style it is whole tabs (`cols / indent_cols()`) with any
    /// remainder emitted as spaces — the remainder is always zero, since every indent step (block or
    /// continuation) is a whole tab in tab style, so the output stays a whole number of tabs.
    fn indent_string(cols: usize, cfg: &Config) -> String {
        match cfg.indent_style {
            IndentStyle::Tab => {
                // One tab per indent level; any sub-level remainder (never produced in tab style,
                // where every step is a whole tab) falls back to spaces.
                let unit = cfg.indent_cols();
                let mut s = cfg.indent_unit().repeat(cols / unit);
                for _ in 0..(cols % unit) {
                    s.push(' ');
                }
                s
            }
            IndentStyle::Space => " ".repeat(cols),
        }
    }

    fn push_indent(out: &mut String, cols: usize, cfg: &Config) {
        out.push_str(&Self::indent_string(cols, cfg));
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
        Self::trim_trailing_blanks(&mut out);
        while out.ends_with('\n') {
            out.pop();
            if out.ends_with('\r') {
                out.pop();
            }
            Self::trim_trailing_blanks(&mut out);
        }
        if cfg.insert_final_newline && !out.is_empty() {
            out.push_str(newline);
        }
        out
    }

    /// Render a [`Doc::Fill`] greedily: pack as many items per line as fit `max_width`, breaking
    /// the separator *before* an item that would overflow. `parts` alternates
    /// `[content, sep, …, content]` (odd length). Each separator's flat/break choice is a pure
    /// function of the items' flat widths and the running column, so re-formatting the output
    /// reproduces the same layout (the fill is idempotent).
    fn render_fill<'a>(&self, indent: usize, parts: &'a [Doc], stack: &mut Vec<Cmd<'a>>) {
        let out = self;
        let max = out.cfg.max_width;
        // Column at the start of a freshly broken line (where wrapped items land).
        let base_col = indent;
        // Where the first item lands: the indent when we just broke (the enclosing group is laying
        // out vertically), else the live column (the group fits flat on the current line).
        let mut col = if out.at_line_start() {
            base_col
        } else {
            out.col
        };
        let mut seq: Vec<(Mode, &Doc)> = Vec::with_capacity(parts.len());
        let mut i = 0;
        while i < parts.len() {
            let content = &parts[i];
            let content_w = content.flat_width();
            // An item that cannot be laid out flat carries a forced break (e.g. an attached
            // comment); render it broken and resume the next item at the indent.
            let content_mode = if content_w.is_some() {
                Mode::Flat
            } else {
                Mode::Break
            };
            seq.push((content_mode, content));
            col = content_w.map_or(base_col, |w| col + w);
            if let Some(sep) = parts.get(i + 1) {
                let next_w = parts.get(i + 2).and_then(Doc::flat_width);
                // Keep the next item on this line only when this item is flat and the next one
                // still fits after a single separating space.
                let keep =
                    content_mode == Mode::Flat && next_w.is_some_and(|nw| col + 1 + nw <= max);
                if keep {
                    seq.push((Mode::Flat, sep));
                    col += 1;
                } else {
                    seq.push((Mode::Break, sep));
                    col = base_col;
                }
            }
            i += 2;
        }
        // Push in reverse so the items render front-to-back (the stack is LIFO).
        for (m, d) in seq.into_iter().rev() {
            stack.push(Cmd {
                indent,
                mode: m,
                doc: d,
            });
        }
    }

    /// Whether `group_doc`, rendered flat starting at the current column, fits within
    /// `max_width` together with the content that follows on the same line.
    fn fits(&self, indent: usize, group_doc: &Doc, rest: &[Cmd<'_>]) -> bool {
        let out = self;
        let start_col = if out.pending_newlines > 0 {
            out.pending_indent
        } else {
            out.col
        };
        let mut remaining = out.cfg.max_width.cast_signed() - start_col.cast_signed();
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
                    remaining -= UnicodeWidthStr::width(&**s).cast_signed();
                    if remaining < 0 {
                        return false;
                    }
                }
                Doc::RawText(s) => {
                    if let Some(pos) = s.find('\n') {
                        remaining -= UnicodeWidthStr::width(&s[..pos]).cast_signed();
                        return remaining >= 0;
                    }
                    remaining -= UnicodeWidthStr::width(&**s).cast_signed();
                    if remaining < 0 {
                        return false;
                    }
                }
                // Measure a comment by its first line, like `RawText`. Exact when reflow is off;
                // when on, comments are surrounded by forced breaks so this never drives layout.
                Doc::Comment { text, .. } => {
                    if let Some(pos) = text.find('\n') {
                        remaining -= UnicodeWidthStr::width(&text[..pos]).cast_signed();
                        return remaining >= 0;
                    }
                    remaining -= UnicodeWidthStr::width(&**text).cast_signed();
                    if remaining < 0 {
                        return false;
                    }
                }
                // A fill is measured one-line, exactly like a `Concat`: every separator counts as a
                // flat space and the items count flat. (`fits` only ever asks whether the enclosing
                // group's flat layout stays within `max-width`.)
                Doc::Concat(v) | Doc::Fill(v) => {
                    for d in v.iter().rev() {
                        work.push((ind, mode, d));
                    }
                }
                Doc::Indent(d) => work.push((ind + out.cfg.indent_cols(), mode, d)),
                Doc::ContinuationIndent(d) => {
                    work.push((ind + out.cfg.continuation_cols(), mode, d));
                }
                Doc::IndentIfBreak(d) => {
                    let ind = if mode == Mode::Break {
                        ind + out.cfg.continuation_cols()
                    } else {
                        ind
                    };
                    work.push((ind, mode, d));
                }
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
                Doc::IfBreak { broken, flat } => {
                    work.push((ind, mode, if mode == Mode::Break { broken } else { flat }));
                }
            }
        }
        remaining >= 0
    }
}
