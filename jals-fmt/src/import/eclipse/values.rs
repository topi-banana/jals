//! The value vocabularies an Eclipse JDT setting can take.
//!
//! Every family module types its fields with one of these rather than with a `String`, so two
//! distinct native values can never collapse before the projection decides. The vocabularies come
//! from `DefaultCodeFormatterConstants`' own javadoc (`- possible values:` on each option).

use crate::import::serde_kv;
use serde::{Deserialize, Deserializer};

/// `tabulation.char` — what an indentation level is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabChar {
    /// Real tab characters.
    Tab,
    /// Spaces.
    Space,
    /// Tabs to the last full stop, spaces for the remainder. The one mode in which
    /// `indentation.size` and `tabulation.size` differ.
    Mixed,
}

/// A `brace_position_for_*` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BracePosition {
    /// K&R — the brace ends the header line.
    EndOfLine,
    /// Allman — the brace on its own line.
    NextLine,
    /// The brace on its own line, indented one extra level (Whitesmiths).
    NextLineShifted,
    /// On its own line only when the header wrapped.
    NextLineOnWrap,
}

/// An `insert_space_*` / `insert_new_line_*` toggle.
///
/// Eclipse spells these `insert` / `do not insert` — note the interior spaces. A two-valued
/// enum, deliberately not a `bool`, so the native spelling stays recoverable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Insert {
    /// `insert`.
    #[serde(rename = "insert")]
    Insert,
    /// `do not insert`.
    #[serde(rename = "do not insert")]
    DoNotInsert,
}

impl Insert {
    /// Whether this toggle emits the space / newline.
    #[must_use]
    pub const fn is_insert(self) -> bool {
        matches!(self, Self::Insert)
    }
}

/// A `keep_*_on_one_line` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OneLine {
    /// `one_line_never`.
    OneLineNever,
    /// `one_line_if_empty`.
    OneLineIfEmpty,
    /// `one_line_if_single_item`.
    OneLineIfSingleItem,
    /// `one_line_always`.
    OneLineAlways,
    /// `one_line_preserve` — reads the input's line breaks.
    OneLinePreserve,
}

/// A `parentheses_positions_in_*` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParenthesisPositions {
    /// `common_lines` — both delimiters share a line with the adjacent item.
    CommonLines,
    /// `separate_lines_if_wrapped`.
    SeparateLinesIfWrapped,
    /// `separate_lines_if_not_empty`.
    SeparateLinesIfNotEmpty,
    /// `separate_lines`.
    SeparateLines,
    /// `preserve_positions` — keeps the source's delimiter placement.
    PreservePositions,
}

/// A `text_block_indentation` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextBlockIndentation {
    /// `indent_preserve`.
    IndentPreserve,
    /// `indent_by_one`.
    IndentByOne,
    /// `indent_default`.
    IndentDefault,
    /// `indent_on_column`.
    IndentOnColumn,
}

/// An `alignment_for_*` value: a decimal integer whose **bits** encode the wrap policy.
///
/// Treating it as an opaque id is the classic mistake — `16` and `49` differ only in flag bits.
/// The split mode lives in [`SPLIT_MASK`](Self::SPLIT_MASK); [`FORCE`](Self::FORCE) turns
/// "wrap when too long" into "always wrap"; [`INDENT_ON_COLUMN`](Self::INDENT_ON_COLUMN) and
/// [`INDENT_BY_ONE`](Self::INDENT_BY_ONE) choose the continuation indent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alignment(pub u32);

impl Alignment {
    /// `M_FORCE` — wrap always, rather than only when too long.
    pub const FORCE: u32 = 1;
    /// `M_INDENT_ON_COLUMN` — align continuations under the opening delimiter's column.
    pub const INDENT_ON_COLUMN: u32 = 2;
    /// `M_INDENT_BY_ONE` — indent continuations by exactly one level.
    pub const INDENT_BY_ONE: u32 = 4;
    /// The bits holding the split mode.
    pub const SPLIT_MASK: u32 = 0x70;
    /// `M_NO_ALIGNMENT` — never wrap at this position.
    pub const NO_ALIGNMENT: u32 = 0;
    /// `M_COMPACT_SPLIT` — fill as many items per line as fit.
    pub const COMPACT: u32 = 16;
    /// `M_COMPACT_FIRST_BREAK_SPLIT` — like compact, but break before the first element.
    pub const COMPACT_FIRST_BREAK: u32 = 32;
    /// `M_ONE_PER_LINE_SPLIT` — one item per line.
    pub const ONE_PER_LINE: u32 = 48;
    /// `M_NEXT_SHIFTED_SPLIT` — one per line, continuations shifted one level.
    pub const NEXT_SHIFTED: u32 = 64;
    /// `M_NEXT_PER_LINE_SPLIT` — every item after the first on its own line.
    pub const NEXT_PER_LINE: u32 = 80;
    /// `Integer.MAX_VALUE`, Eclipse's "never wrap here" sentinel.
    pub const NEVER: u32 = 0x7fff_ffff;

    /// The split mode bits alone.
    #[must_use]
    pub const fn split(self) -> u32 {
        self.0 & Self::SPLIT_MASK
    }

    /// Whether wrapping is forced rather than overflow-driven.
    #[must_use]
    pub const fn is_forced(self) -> bool {
        self.0 != Self::NEVER && (self.0 & Self::FORCE) != 0
    }

    /// Whether this is the never-wrap sentinel.
    #[must_use]
    pub const fn is_never(self) -> bool {
        self.0 == Self::NEVER
    }

    /// `deserialize_with` coercer from the decimal string form.
    pub(crate) fn opt_deserialize<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(serde_kv::opt_number(deserializer)?.map(Self))
    }
}
