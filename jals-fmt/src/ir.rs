//! The layout IR — a faithful port of google-java-format's `Doc` vocabulary.
//!
//! # This is not a Wadler/prettier document
//!
//! The crate had a prettier-style `Doc` (`Group` / `Line` / `SoftLine` / `Fill`) before the
//! rewrite and it was removed on purpose. Prettier's `fits` scans **past** a group's boundary to
//! the next hard newline, so a group followed by more content on the same line breaks differently
//! than google-java-format's, which measures the [`Level`]'s **own precomputed width** and stops
//! at the boundary. The two also disagree about mixing: GJF puts `UNIFIED` (all-or-nothing) and
//! `INDEPENDENT` (fill) breaks inside *one* level, where prettier needs `group` and `fill` to be
//! different nodes. Do not reintroduce either construct — `DESIGN.md` §2.2 is the argument in
//! full, and §8.3 makes engine consistency outrank every other goal.
//!
//! # The five shapes
//!
//! `Doc` has exactly the five GJF subclasses. Comments and whitespace ride **inside** the tree as
//! [`Doc::Tok`]; there is no separate comment node.
//!
//! | shape | what it is |
//! |---|---|
//! | [`Doc::Level`] | a group with its own indent, the only recursive shape |
//! | [`Doc::Token`] | one significant token's text |
//! | [`Doc::Break`] | a break point: `flat` text when it stays, a newline + indent when it goes |
//! | [`Doc::Space`] | a non-breaking space, width 1 |
//! | [`Doc::Tok`] | verbatim text that may contain newlines (comments, disabled regions) |
//!
//! The resolution algorithm over them lives in [`engine`](crate::engine); this module is the data
//! plus the two bottom-up measurements it needs ([`Doc::width`] and [`Doc::write_flat`]).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// Column measurement.
///
/// google-java-format counts columns in **UTF-16 code units** (`String.length()`), not display
/// cells: a CJK ideograph is width 1 to it, and East-Asian-width tables are never consulted. The
/// pre-rewrite formatter used `UnicodeWidthStr::width` and diverged from GJF on every file with a
/// wide character. This is engine behavior, not a rule — nothing in `Config` switches it
/// (`DESIGN.md` §2.6, seam list §8.1).
pub(crate) struct Width;

impl Width {
    /// The sentinel width of something that can never sit on one line: a forced break, a token
    /// holding a newline, a `//` comment. Saturating arithmetic keeps it absorbing, so any level
    /// containing one fails `column + width <= max_width` at every column.
    pub(crate) const INFINITE: usize = usize::MAX;

    /// The width of `text` in UTF-16 code units.
    pub(crate) fn utf16(text: &str) -> usize {
        text.chars().map(char::len_utf16).sum()
    }

    /// The width of `text` as a *token*: [`INFINITE`](Self::INFINITE) when it spans lines (a text
    /// block, a disabled region), otherwise its UTF-16 width.
    pub(crate) fn token(text: &str) -> usize {
        if text.contains('\n') {
            Self::INFINITE
        } else {
            Self::utf16(text)
        }
    }

    /// The width of `text` as a *comment*: its first line.
    ///
    /// A `//` comment swallows the rest of its line, but that is enforced by forcing the break
    /// that follows it ([`Ops::force_next_break`](crate::ops::Ops::force_next_break)), not by
    /// making it infinitely wide. Measuring it as infinite would additionally poison every
    /// enclosing level, so a trailing `// note` on a field would break the field's initializer
    /// onto its own line — a comment changing the layout of the code it annotates.
    pub(crate) fn tok(text: &str) -> usize {
        text.find('\n')
            .map_or_else(|| Self::utf16(text), |at| Self::utf16(&text[..at]))
    }
}

/// A correlated-break label — google-java-format's `BreakTag`, prettier's group id.
///
/// A [`Break`] may carry one; once the engine has decided that break, an [`Indent::If`] elsewhere
/// in the document can read the decision. Ids are handed out sequentially by
/// [`Ops`](crate::ops::Ops), so the engine can hold the decisions in a flat `Vec` instead of a map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BreakTag(pub(crate) u32);

/// How a [`Break`] participates in its level's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FillMode {
    /// All-or-nothing: if the level does not fit flat, **every** unified break in it goes.
    /// Prettier's `group`.
    Unified,
    /// Fill: this break goes only when the next chunk would not fit. Prettier's `fill`, except
    /// that GJF lets it share a level with [`Unified`](Self::Unified) breaks.
    Independent,
    /// Always breaks, and makes the level's width [`Width::INFINITE`] so it can never be flat.
    Forced,
}

/// An indent amount. A type rather than an `i32` because google-java-format's `Indent.If` makes an
/// amount conditional on whether a tagged break was taken.
#[derive(Debug, Clone)]
pub(crate) enum Indent {
    /// A fixed number of columns. The style's multiplier is baked in at construction, so the
    /// engine never sees an unscaled level count.
    Const(i32),
    /// [`broken`](Self::If::broken) columns when `tag`'s break was taken, else
    /// [`flat`](Self::If::flat).
    If {
        /// The break whose decision selects the branch.
        tag: BreakTag,
        /// Used when that break was taken.
        broken: Box<Self>,
        /// Used when it was not.
        flat: Box<Self>,
    },
}

impl Indent {
    /// No extra indent.
    pub(crate) const ZERO: Self = Self::Const(0);

    /// A fixed amount.
    pub(crate) const fn columns(n: i32) -> Self {
        Self::Const(n)
    }

    /// `broken` columns when `tag`'s break was taken, `flat` otherwise.
    pub(crate) fn when_broken(tag: BreakTag, broken: Self, flat: Self) -> Self {
        Self::If {
            tag,
            broken: Box::new(broken),
            flat: Box::new(flat),
        }
    }
}

/// A break point.
///
/// `flat` is what is emitted when the break is *not* taken (usually `""` or `" "`); `plus_indent`
/// is what is added to the enclosing level's indent when it *is*.
#[derive(Debug)]
pub(crate) struct Break {
    /// How this break participates in its level's decision.
    pub(crate) fill: FillMode,
    /// Text emitted when the break is not taken.
    pub(crate) flat: Box<str>,
    /// Extra columns for the line this break starts.
    pub(crate) plus_indent: Indent,
    /// Optional label, so an [`Indent::If`] can read this break's decision.
    pub(crate) tag: Option<BreakTag>,
    /// Empty lines emitted *in addition to* this break's own newline.
    ///
    /// This is how the one input-whitespace fact the engine reads — whether two significant
    /// tokens had a blank line between them — reaches the output. The visitor resolves the count
    /// against `[blank-lines]` before the document exists, so the resolution algorithm stays a
    /// pure function of the tree (`DESIGN.md` §17). A non-zero count only makes sense on a break
    /// that is always taken, so [`Ops`](crate::ops::Ops) forces the fill mode when it sets one.
    pub(crate) blank_lines: usize,
    /// Resolved by the engine: whether the break was taken.
    pub(crate) broken: bool,
    /// Resolved by the engine: the column the following line starts at.
    pub(crate) new_indent: usize,
}

impl Break {
    /// A break that renders as `flat` when it stays on the line.
    pub(crate) fn new(
        fill: FillMode,
        flat: &str,
        plus_indent: Indent,
        tag: Option<BreakTag>,
    ) -> Self {
        Self {
            fill,
            flat: flat.into(),
            plus_indent,
            tag,
            blank_lines: 0,
            broken: false,
            new_indent: 0,
        }
    }

    /// Whether this break always goes — which also makes its level unable to be flat.
    pub(crate) const fn is_forced(&self) -> bool {
        matches!(self.fill, FillMode::Forced)
    }

    /// The flat width, or [`Width::INFINITE`] when forced (`Break.computeWidth`).
    pub(crate) fn width(&self) -> usize {
        if self.is_forced() {
            Width::INFINITE
        } else {
            Width::utf16(&self.flat)
        }
    }
}

/// A group with its own indent — the only recursive shape, and the unit the engine decides.
#[derive(Debug)]
pub(crate) struct Level {
    /// Columns added to the enclosing indent when this level breaks.
    pub(crate) plus_indent: Indent,
    /// The level's contents.
    pub(crate) docs: Vec<Doc>,
    /// The flat width of [`docs`](Self::docs), precomputed bottom-up by
    /// [`Doc::measure`](Doc::measure).
    pub(crate) width: usize,
    /// Resolved by the engine: the whole level fit on one line.
    pub(crate) one_line: bool,
}

impl Level {
    /// An empty level.
    pub(crate) const fn new(plus_indent: Indent) -> Self {
        Self {
            plus_indent,
            docs: Vec::new(),
            width: 0,
            one_line: false,
        }
    }
}

/// One node of the layout document.
#[derive(Debug)]
pub(crate) enum Doc {
    /// A group with its own indent.
    Level(Level),
    /// One significant token's text. A text block or a formatter-disabled region arrives here
    /// with newlines in it, which makes its width [`Width::INFINITE`].
    Token {
        /// The token text, emitted verbatim.
        text: Box<str>,
    },
    /// A break point.
    Break(Break),
    /// A non-breaking space.
    Space,
    /// Comment (or otherwise verbatim) text, which may contain newlines.
    Tok {
        /// The text, emitted verbatim except for the re-indent below.
        text: Box<str>,
        /// Re-align this text's continuation lines to the column it starts at. Set for a
        /// multi-line block / Javadoc comment, whose interior `*` column must follow the code it
        /// was moved with; clear for a formatter-disabled region, which must stay byte-identical.
        reindent: bool,
    },
}

impl Doc {
    /// A significant token.
    pub(crate) fn token(text: &str) -> Self {
        Self::Token { text: text.into() }
    }

    /// A comment whose continuation lines follow the code's indent.
    pub(crate) fn comment(text: &str) -> Self {
        Self::Tok {
            text: text.into(),
            reindent: true,
        }
    }

    /// Text that must survive byte-identical — a formatter-disabled region.
    pub(crate) fn verbatim(text: &str) -> Self {
        Self::Tok {
            text: text.into(),
            reindent: false,
        }
    }

    /// The flat width of this node, reading the cache a [`Level`] carries.
    pub(crate) fn width(&self) -> usize {
        match self {
            Self::Level(level) => level.width,
            Self::Token { text } => Width::token(text),
            Self::Break(brk) => brk.width(),
            Self::Space => 1,
            Self::Tok { text, .. } => Width::tok(text),
        }
    }

    /// The flat width of a run of nodes.
    pub(crate) fn width_of(docs: &[Self]) -> usize {
        docs.iter()
            .fold(0usize, |acc, doc| acc.saturating_add(doc.width()))
    }

    /// Append this node's flat rendering to `out` — what a level emits when it fits on one line.
    pub(crate) fn write_flat(&self, out: &mut String) {
        match self {
            Self::Level(level) => {
                for child in &level.docs {
                    child.write_flat(out);
                }
            }
            Self::Token { text } | Self::Tok { text, .. } => out.push_str(text),
            Self::Break(brk) => out.push_str(&brk.flat),
            Self::Space => out.push(' '),
        }
    }
}
