//! A Wadler/Prettier-style document intermediate representation.
//!
//! Lowering (`lower.rs`) turns the CST into a single [`Doc`]; the renderer
//! (`render.rs`) turns a [`Doc`] into the formatted string, choosing for each
//! [`Doc::Group`] whether it fits flat on the current line or must break.

use unicode_width::UnicodeWidthStr;

/// The flavour of a comment, controlling how it reflows under `wrap-comments`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommentKind {
    /// `// ...` line comment.
    Line,
    /// `/* ... */` block comment.
    Block,
    /// `/** ... */` documentation comment (Javadoc).
    Doc,
}

/// A formatting document.
#[derive(Debug)]
pub(crate) enum Doc {
    /// Verbatim text with no interior newline. Width is its unicode display width.
    Text(Box<str>),
    /// Verbatim text that may contain newlines (string literals, text blocks,
    /// block-comment bodies). The renderer never reindents its interior.
    RawText(Box<str>),
    /// A standalone comment, emitted verbatim unless `wrap-comments` is on, in which case
    /// the renderer reflows it to `comment-width` at the current indentation.
    Comment {
        /// Which comment flavour this is.
        kind: CommentKind,
        /// The comment's source text (`//`, `/* */`, or `/** */`, verbatim).
        text: Box<str>,
    },
    /// A sequence of documents.
    Concat(Vec<Doc>),
    /// A space when flat; a newline + indent when broken.
    Line,
    /// Nothing when flat; a newline + indent when broken.
    SoftLine,
    /// Always a newline + indent. Forces every enclosing group to break.
    HardLine,
    /// One or more blank lines (the source count; the renderer clamps it to
    /// `max_blank_lines`). Forces enclosing groups to break.
    BlankLine(usize),
    /// Increase the indentation level of the child by one.
    Indent(Box<Doc>),
    /// A break point: render flat if it fits, otherwise broken.
    Group {
        /// The grouped content.
        doc: Box<Doc>,
        /// Precomputed: the content contains a forced break, so it must render broken.
        should_break: bool,
    },
    /// Content appended at the end of the current line, after any following break.
    /// Used for trailing comments so they stay on their line even when a group breaks.
    LineSuffix(Box<Doc>),
}

/// Empty document.
pub(crate) fn nil() -> Doc {
    Doc::Text("".into())
}

/// Verbatim text (must not contain a newline).
pub(crate) fn text<S: Into<Box<str>>>(s: S) -> Doc {
    Doc::Text(s.into())
}

/// Verbatim text that may contain newlines, never reflowed or reindented.
pub(crate) fn raw<S: Into<Box<str>>>(s: S) -> Doc {
    Doc::RawText(s.into())
}

/// A comment that the renderer reflows to `comment-width` when `wrap-comments` is on.
pub(crate) fn comment<S: Into<Box<str>>>(kind: CommentKind, s: S) -> Doc {
    Doc::Comment {
        kind,
        text: s.into(),
    }
}

/// Concatenate documents.
pub(crate) fn concat(docs: Vec<Doc>) -> Doc {
    Doc::Concat(docs)
}

/// A space-or-break.
pub(crate) fn line() -> Doc {
    Doc::Line
}

/// A nothing-or-break.
pub(crate) fn softline() -> Doc {
    Doc::SoftLine
}

/// A forced line break.
pub(crate) fn hardline() -> Doc {
    Doc::HardLine
}

/// A forced run of `count` blank lines (clamped to `max_blank_lines` when rendered).
pub(crate) fn blank_line(count: usize) -> Doc {
    Doc::BlankLine(count)
}

/// Indent a document by one level.
pub(crate) fn indent(doc: Doc) -> Doc {
    Doc::Indent(Box::new(doc))
}

/// Group a document so it renders flat when it fits, otherwise broken.
pub(crate) fn group(doc: Doc) -> Doc {
    let should_break = contains_forced_break(&doc);
    Doc::Group {
        doc: Box::new(doc),
        should_break,
    }
}

/// Group a document under a flat-width budget narrower than `max-width`: it is forced broken
/// when its flat width would exceed `max_flat` (or it cannot be laid out flat at all). Within
/// the budget it still renders flat only if it also fits `max-width` at its position, exactly
/// like [`group`]. Used for `chain-width`.
pub(crate) fn group_within(doc: Doc, max_flat: usize) -> Doc {
    let should_break = match flat_width(&doc) {
        Some(w) => w > max_flat,
        None => true,
    };
    Doc::Group {
        doc: Box::new(doc),
        should_break,
    }
}

/// The display width of `doc` laid out fully flat, or `None` when it cannot be flat — it
/// contains a forced break (hard/blank line or an already-breaking group) or a multi-line
/// raw token. `Line` counts as one space, `SoftLine` as nothing, mirroring the renderer's
/// flat mode.
fn flat_width(doc: &Doc) -> Option<usize> {
    let w = match doc {
        Doc::Text(s) => UnicodeWidthStr::width(&**s),
        Doc::RawText(s) => {
            if s.contains('\n') {
                return None;
            }
            UnicodeWidthStr::width(&**s)
        }
        Doc::Comment { text, .. } => {
            if text.contains('\n') {
                return None;
            }
            UnicodeWidthStr::width(&**text)
        }
        Doc::Concat(v) => {
            let mut total = 0;
            for d in v {
                total += flat_width(d)?;
            }
            total
        }
        Doc::Line => 1,
        Doc::SoftLine => 0,
        Doc::HardLine | Doc::BlankLine(_) => return None,
        Doc::Indent(d) => flat_width(d)?,
        Doc::Group { doc, should_break } => {
            if *should_break {
                return None;
            }
            flat_width(doc)?
        }
        // Line suffixes (trailing comments) are deferred past the next break; they never
        // contribute to the flat width of the line they ride on.
        Doc::LineSuffix(_) => 0,
    };
    Some(w)
}

/// Defer content to the end of the current line.
pub(crate) fn line_suffix(doc: Doc) -> Doc {
    Doc::LineSuffix(Box::new(doc))
}

/// Interleave `sep` between `items`.
pub(crate) fn join(sep: Doc, items: Vec<Doc>) -> Doc {
    let mut out = Vec::with_capacity(items.len().saturating_mul(2));
    let mut first = true;
    for item in items {
        if !first {
            out.push(clone_doc(&sep));
        }
        out.push(item);
        first = false;
    }
    Doc::Concat(out)
}

/// Whether the document forces a break (contains a hardline / blank line, possibly
/// inside a nested group). Used to precompute `Group::should_break`.
fn contains_forced_break(doc: &Doc) -> bool {
    match doc {
        Doc::HardLine | Doc::BlankLine(_) => true,
        Doc::Concat(v) => v.iter().any(contains_forced_break),
        Doc::Indent(d) | Doc::LineSuffix(d) => contains_forced_break(d),
        Doc::Group { should_break, .. } => *should_break,
        _ => false,
    }
}

/// Clone a document. Only used by [`join`] to duplicate separators.
fn clone_doc(doc: &Doc) -> Doc {
    match doc {
        Doc::Text(s) => Doc::Text(s.clone()),
        Doc::RawText(s) => Doc::RawText(s.clone()),
        Doc::Comment { kind, text } => Doc::Comment {
            kind: *kind,
            text: text.clone(),
        },
        Doc::Concat(v) => Doc::Concat(v.iter().map(clone_doc).collect()),
        Doc::Line => Doc::Line,
        Doc::SoftLine => Doc::SoftLine,
        Doc::HardLine => Doc::HardLine,
        Doc::BlankLine(n) => Doc::BlankLine(*n),
        Doc::Indent(d) => Doc::Indent(Box::new(clone_doc(d))),
        Doc::Group { doc, should_break } => Doc::Group {
            doc: Box::new(clone_doc(doc)),
            should_break: *should_break,
        },
        Doc::LineSuffix(d) => Doc::LineSuffix(Box::new(clone_doc(d))),
    }
}
