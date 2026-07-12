//! A Wadler/Prettier-style document intermediate representation.
//!
//! Lowering (`lower.rs`) turns the CST into a single [`Doc`]; the renderer
//! (`render.rs`) turns a [`Doc`] into the formatted string, choosing for each
//! [`Doc::Group`] whether it fits flat on the current line or must break.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

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
    Concat(Vec<Self>),
    /// A space when flat; a newline + indent when broken.
    Line,
    /// Nothing when flat; a newline + indent when broken.
    SoftLine,
    /// Always a newline + indent. Forces every enclosing group to break.
    HardLine,
    /// One or more blank lines (the source count; the renderer clamps it to
    /// `max_blank_lines`). Forces enclosing groups to break.
    BlankLine(usize),
    /// Increase the indentation of the child by one block level (`indent-width`). Used for block
    /// bodies (`{ … }`).
    Indent(Box<Self>),
    /// Increase the indentation of the child by one *continuation* width (`continuation-indent`,
    /// falling back to `indent-width`). Used where an expression or statement wraps — method
    /// chains, wrapped binary / ternary operators, and delimited lists — as opposed to a block
    /// body.
    ContinuationIndent(Box<Self>),
    /// Increase the indentation of the child by one *continuation* width only when the governing
    /// group renders broken; leave it at the current level when flat. Lets
    /// `overflow-delimited-expr` place the hanging last argument inside the list (continuation)
    /// indent in the vertical layout but at the call's own level in the overflow layout
    /// (prettier's `indentIfBreak`).
    IndentIfBreak(Box<Self>),
    /// A break point: render flat if it fits, otherwise broken.
    Group {
        /// The grouped content.
        doc: Box<Self>,
        /// Precomputed: the content contains a forced break, so it must render broken.
        should_break: bool,
    },
    /// Content appended at the end of the current line, after any following break.
    /// Used for trailing comments so they stay on their line even when a group breaks.
    LineSuffix(Box<Self>),
    /// Render `broken` when the enclosing group breaks, `flat` when it is laid out flat. Lets a
    /// trailing comma appear only in the vertical (broken) layout (`trailing-comma = vertical`).
    IfBreak {
        /// Emitted when the enclosing group renders broken.
        broken: Box<Self>,
        /// Emitted when the enclosing group renders flat.
        flat: Box<Self>,
    },
    /// A fill: an alternating, odd-length sequence `[content, sep, content, sep, …, content]`.
    /// The renderer greedily keeps each item on the current line, breaking the *preceding*
    /// separator (a [`Line`](Doc::Line)) only when the next item would overflow `max-width`.
    /// Unlike a [`Group`](Doc::Group) — which is all-or-nothing — a fill packs as many items
    /// per line as fit. Used for the `Compressed` parameter layout (`fn-params-layout`).
    Fill(Vec<Self>),
}

impl Doc {
    /// Empty document.
    pub(crate) fn nil() -> Self {
        Self::Text("".into())
    }

    /// Verbatim text (must not contain a newline).
    pub(crate) fn text<S: Into<Box<str>>>(s: S) -> Self {
        Self::Text(s.into())
    }

    /// Verbatim text that may contain newlines, never reflowed or reindented.
    pub(crate) fn raw<S: Into<Box<str>>>(s: S) -> Self {
        Self::RawText(s.into())
    }

    /// A comment that the renderer reflows to `comment-width` when `wrap-comments` is on.
    pub(crate) fn comment<S: Into<Box<str>>>(kind: CommentKind, s: S) -> Self {
        Self::Comment {
            kind,
            text: s.into(),
        }
    }

    /// Concatenate documents.
    pub(crate) const fn concat(docs: Vec<Self>) -> Self {
        Self::Concat(docs)
    }

    /// A space-or-break.
    pub(crate) const fn line() -> Self {
        Self::Line
    }

    /// A nothing-or-break.
    pub(crate) const fn softline() -> Self {
        Self::SoftLine
    }

    /// A forced line break.
    pub(crate) const fn hardline() -> Self {
        Self::HardLine
    }

    /// A forced run of `count` blank lines (clamped to `max_blank_lines` when rendered).
    pub(crate) const fn blank_line(count: usize) -> Self {
        Self::BlankLine(count)
    }

    /// Indent a document by one block level.
    pub(crate) fn indent(doc: Self) -> Self {
        Self::Indent(Box::new(doc))
    }

    /// Indent a document by one continuation width — used for the wrapped lines of an expression or
    /// statement (method chains, wrapped binary / ternary operators, delimited lists).
    pub(crate) fn continuation_indent(doc: Self) -> Self {
        Self::ContinuationIndent(Box::new(doc))
    }

    /// Indent a document by one level only when the governing group renders broken.
    fn indent_if_break(doc: Self) -> Self {
        Self::IndentIfBreak(Box::new(doc))
    }

    /// Group a document so it renders flat when it fits, otherwise broken.
    pub(crate) fn group(doc: Self) -> Self {
        let should_break = Self::contains_forced_break(&doc);
        Self::Group {
            doc: Box::new(doc),
            should_break,
        }
    }

    /// Group a document forced to render broken regardless of width — every [`Line`](Doc::Line) /
    /// [`SoftLine`](Doc::SoftLine) inside breaks. Used for the `Vertical` parameter layout, which
    /// lays out one parameter per line even when the list would fit on one line.
    pub(crate) fn group_always_break(doc: Self) -> Self {
        Self::Group {
            doc: Box::new(doc),
            should_break: true,
        }
    }

    /// Build a [`Doc::Fill`] from already-rendered items, interleaving a [`Doc::line`] separator
    /// between consecutive items. The renderer packs as many items per line as fit `max-width`.
    /// An empty or single-item list needs no separators and is returned as-is inside the fill.
    pub(crate) fn fill(items: Vec<Self>) -> Self {
        let mut parts = Vec::with_capacity(items.len().saturating_mul(2).saturating_sub(1));
        let mut first = true;
        for item in items {
            if !first {
                parts.push(Self::line());
            }
            parts.push(item);
            first = false;
        }
        Self::Fill(parts)
    }

    /// Group a document under a flat-width budget narrower than `max-width`: it is forced broken
    /// when its flat width would exceed `max_flat` (or it cannot be laid out flat at all). Within
    /// the budget it still renders flat only if it also fits `max-width` at its position, exactly
    /// like [`Doc::group`]. Used for `chain-width`.
    pub(crate) fn group_within(doc: Self, max_flat: usize) -> Self {
        let should_break = doc.flat_width().is_none_or(|w| w > max_flat);
        Self::Group {
            doc: Box::new(doc),
            should_break,
        }
    }

    /// Group an `overflow-delimited-expr` argument list: `head` is the open delimiter plus the
    /// leading items (inside the list `Indent`), `last` the hanging item (indented only when
    /// broken), `tail` the closing softline + delimiter. Forced broken when `head` cannot be
    /// laid out flat — an earlier item or comment forces a break, so the overflow layout is
    /// unavailable — or when the first rendered line of the overflow layout would exceed
    /// `max_flat` (`fn-call-width` for a call; `None` for an annotation list, which breaks
    /// against `max-width` alone like a plain [`Doc::group`]). Within budget it still renders flat
    /// only if its first line also fits `max-width` at its position, exactly like [`Doc::group`].
    pub(crate) fn group_overflow(
        head: Self,
        last: Self,
        tail: Self,
        max_flat: Option<usize>,
    ) -> Self {
        let should_break = match head.flat_width() {
            None => true,
            Some(_) => max_flat.is_some_and(|b| Self::first_line_width(&[&head, &last, &tail]) > b),
        };
        Self::Group {
            doc: Box::new(Self::Concat(vec![head, Self::indent_if_break(last), tail])),
            should_break,
        }
    }

    /// The display width of the first rendered line of `docs`, laid out with every unforced
    /// break flat: the prefix up to the first forced newline (a hard/blank line, a break inside
    /// an already-breaking group, or the first newline of a multi-line raw token / comment), or
    /// the full flat width when nothing forces one. Mirrors the renderer's `fits` measurement,
    /// which also stops at the first forced newline, so the precomputed overflow decision and
    /// the render-time fit never disagree about what lands on the first line.
    fn first_line_width(docs: &[&Self]) -> usize {
        let mut width = 0usize;
        // Worklist of (in-broken-group, doc), LIFO like the renderer's.
        let mut work: Vec<(bool, &Self)> = docs.iter().rev().map(|d| (false, *d)).collect();
        while let Some((broken, doc)) = work.pop() {
            match doc {
                Self::Text(s) => width += UnicodeWidthStr::width(&**s),
                Self::RawText(s) | Self::Comment { text: s, .. } => match s.find('\n') {
                    Some(pos) => return width + UnicodeWidthStr::width(&s[..pos]),
                    None => width += UnicodeWidthStr::width(&**s),
                },
                Self::Concat(v) => work.extend(v.iter().rev().map(|d| (broken, d))),
                Self::Indent(d) | Self::ContinuationIndent(d) | Self::IndentIfBreak(d) => {
                    work.push((broken, d));
                }
                Self::Group { doc, should_break } => work.push((*should_break, doc)),
                Self::Fill(v) => work.extend(v.iter().rev().map(|d| (broken, d))),
                Self::Line | Self::SoftLine if broken => return width,
                Self::Line => width += 1,
                Self::SoftLine | Self::LineSuffix(_) => {}
                Self::HardLine | Self::BlankLine(_) => return width,
                Self::IfBreak { broken: b, flat } => {
                    work.push((broken, if broken { b } else { flat }));
                }
            }
        }
        width
    }

    /// The display width of `self` laid out fully flat, or `None` when it cannot be flat — it
    /// contains a forced break (hard/blank line or an already-breaking group) or a multi-line
    /// raw token. `Line` counts as one space, `SoftLine` as nothing, mirroring the renderer's
    /// flat mode.
    pub(crate) fn flat_width(&self) -> Option<usize> {
        let w = match self {
            Self::Text(s) => UnicodeWidthStr::width(&**s),
            Self::RawText(s) => {
                if s.contains('\n') {
                    return None;
                }
                UnicodeWidthStr::width(&**s)
            }
            Self::Comment { text, .. } => {
                if text.contains('\n') {
                    return None;
                }
                UnicodeWidthStr::width(&**text)
            }
            Self::Concat(v) | Self::Fill(v) => {
                let mut total = 0;
                for d in v {
                    total += d.flat_width()?;
                }
                total
            }
            Self::Line => 1,
            // Line suffixes (trailing comments) are deferred past the next break; they never
            // contribute to the flat width of the line they ride on.
            Self::SoftLine | Self::LineSuffix(_) => 0,
            Self::HardLine | Self::BlankLine(_) => return None,
            Self::Indent(d) | Self::ContinuationIndent(d) | Self::IndentIfBreak(d) => {
                d.flat_width()?
            }
            Self::Group { doc, should_break } => {
                if *should_break {
                    return None;
                }
                doc.flat_width()?
            }
            // Laid out flat, an `IfBreak` renders its flat branch.
            Self::IfBreak { flat, .. } => flat.flat_width()?,
        };
        Some(w)
    }

    /// Defer content to the end of the current line.
    pub(crate) fn line_suffix(doc: Self) -> Self {
        Self::LineSuffix(Box::new(doc))
    }

    /// Content that renders as `broken` when its enclosing group breaks and as `flat` when the
    /// group is laid out flat.
    pub(crate) fn if_break(broken: Self, flat: Self) -> Self {
        Self::IfBreak {
            broken: Box::new(broken),
            flat: Box::new(flat),
        }
    }

    /// Interleave `sep` between `items`.
    pub(crate) fn join(sep: &Self, items: Vec<Self>) -> Self {
        let mut out = Vec::with_capacity(items.len().saturating_mul(2));
        let mut first = true;
        for item in items {
            if !first {
                out.push(sep.clone_doc());
            }
            out.push(item);
            first = false;
        }
        Self::Concat(out)
    }

    /// Whether the document forces a break (contains a hardline / blank line, possibly
    /// inside a nested group). Used to precompute `Group::should_break`.
    fn contains_forced_break(doc: &Self) -> bool {
        match doc {
            Self::HardLine | Self::BlankLine(_) => true,
            Self::Concat(v) | Self::Fill(v) => v.iter().any(Self::contains_forced_break),
            Self::Indent(d)
            | Self::ContinuationIndent(d)
            | Self::IndentIfBreak(d)
            | Self::LineSuffix(d) => Self::contains_forced_break(d),
            Self::Group { should_break, .. } => *should_break,
            _ => false,
        }
    }

    /// Clone a document. Only used by [`Doc::join`] to duplicate separators.
    fn clone_doc(&self) -> Self {
        match self {
            Self::Text(s) => Self::Text(s.clone()),
            Self::RawText(s) => Self::RawText(s.clone()),
            Self::Comment { kind, text } => Self::Comment {
                kind: *kind,
                text: text.clone(),
            },
            Self::Concat(v) => Self::Concat(v.iter().map(Self::clone_doc).collect()),
            Self::Line => Self::Line,
            Self::SoftLine => Self::SoftLine,
            Self::HardLine => Self::HardLine,
            Self::BlankLine(n) => Self::BlankLine(*n),
            Self::Indent(d) => Self::Indent(Box::new(d.clone_doc())),
            Self::ContinuationIndent(d) => Self::ContinuationIndent(Box::new(d.clone_doc())),
            Self::IndentIfBreak(d) => Self::IndentIfBreak(Box::new(d.clone_doc())),
            Self::Group { doc, should_break } => Self::Group {
                doc: Box::new(doc.clone_doc()),
                should_break: *should_break,
            },
            Self::LineSuffix(d) => Self::LineSuffix(Box::new(d.clone_doc())),
            Self::IfBreak { broken, flat } => Self::IfBreak {
                broken: Box::new(broken.clone_doc()),
                flat: Box::new(flat.clone_doc()),
            },
            Self::Fill(v) => Self::Fill(v.iter().map(Self::clone_doc).collect()),
        }
    }
}
