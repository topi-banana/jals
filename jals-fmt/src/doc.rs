//! A Wadler/Prettier-style document intermediate representation.
//!
//! Lowering (`lower.rs`) turns the CST into a single [`Doc`]; the renderer
//! (`render.rs`) turns a [`Doc`] into the formatted string, choosing for each
//! [`Doc::Group`] whether it fits flat on the current line or must break.

/// A formatting document.
#[derive(Debug)]
pub(crate) enum Doc {
    /// Verbatim text with no interior newline. Width is its unicode display width.
    Text(Box<str>),
    /// Verbatim text that may contain newlines (string literals, text blocks,
    /// block-comment bodies). The renderer never reindents its interior.
    RawText(Box<str>),
    /// A sequence of documents.
    Concat(Vec<Doc>),
    /// A space when flat; a newline + indent when broken.
    Line,
    /// Nothing when flat; a newline + indent when broken.
    SoftLine,
    /// Always a newline + indent. Forces every enclosing group to break.
    HardLine,
    /// A blank line (clamped to `max_blank_lines`). Forces enclosing groups to break.
    BlankLine,
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

/// A forced blank line.
pub(crate) fn blank_line() -> Doc {
    Doc::BlankLine
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
        Doc::HardLine | Doc::BlankLine => true,
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
        Doc::Concat(v) => Doc::Concat(v.iter().map(clone_doc).collect()),
        Doc::Line => Doc::Line,
        Doc::SoftLine => Doc::SoftLine,
        Doc::HardLine => Doc::HardLine,
        Doc::BlankLine => Doc::BlankLine,
        Doc::Indent(d) => Doc::Indent(Box::new(clone_doc(d))),
        Doc::Group { doc, should_break } => Doc::Group {
            doc: Box::new(clone_doc(doc)),
            should_break: *should_break,
        },
        Doc::LineSuffix(d) => Doc::LineSuffix(Box::new(clone_doc(d))),
    }
}
