//! `[blank-lines]` — how many empty lines survive, and how many are enforced.
//!
//! Two distinct concepts share this section, exactly as they do in every native formatter:
//!
//! - **`max-*`** clamps the blank lines *already present in the source* (Eclipse
//!   `number_of_empty_lines_to_preserve` / IntelliJ `KEEP_BLANK_LINES_*`). Whether two
//!   significant tokens have a blank line between them is the *only* fact the engine reads from
//!   the input's whitespace — google-java-format reads it too — and it never feeds a line-break
//!   decision (`DESIGN.md` §17).
//! - every other key **enforces** a count at a structural position, independent of the input
//!   (Eclipse `blank_lines_*` / IntelliJ `BLANK_LINES_*`).
//!
//! The two compose the way the vendors compose them: an enforced count is a *minimum*, a `max-*`
//! is a *cap* on a run the source already had. So `at-block-start = 0` with `max-in-code = 1`
//! emits no blank line of its own but keeps one the author wrote — which is exactly
//! google-java-format's behavior at the start of a block.
//!
//! See `jals-fmt/MAPPING.md` §5.2 for the per-vendor correspondence.

use core::fmt;

use serde::de::{self, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How a member that reads as its own unit is separated from its neighbours.
///
/// "Its own unit" is a member carrying a Javadoc comment, whatever its kind, and a field whose
/// annotations take lines of their own. The three references answer three different ways, and
/// what separates them is a **direction** rather than a count — which is why this is not a number:
///
/// - Eclipse and IntelliJ have no such notion at all. A documented member is separated by its
///   kind's own rule, exactly as an undocumented one is: [`Inherit`](Self::Inherit), and the
///   default, because a rule only google-java-format has must not reach a profile that lowered
///   nothing onto it.
/// - google-java-format separates it **more**: `thisOneGetsBlankLineBefore` gives such a member a
///   blank line whatever its kind says. [`AtLeast`](Self::AtLeast) — the wider of the two rules
///   and never the narrower, since the narrower would make *documenting* a member separate it by
///   less than the plain neighbour beside it.
/// - palantir-java-format separates it **less**: it enforces nothing there and leaves the source's
///   own blank lines to say, so two documented fields written adjacent stay adjacent where
///   google-java-format separates them. [`Preserve`](Self::Preserve).
///
/// Spelled in `jalsfmt.toml` as `"inherit"`, a count, or `"preserve"`.
///
/// A count of `0` is [`AtLeast(0)`](Self::AtLeast) — "at least none", which the kind rule wins
/// over — and **not** `Preserve`. The two were the same spelling while this key overrode the kind
/// rule outright, which is what made a documented member come out separated by *less* than a plain
/// neighbour; a config that meant "suppress" has to say `"preserve"` now, and one that meant
/// "nothing extra" can drop the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocumentedMember {
    /// The member's own `[blank-lines]` rule decides. Eclipse, IntelliJ, and the default.
    #[default]
    Inherit,
    /// At least this many lines, whatever the kind rule says. google-java-format.
    AtLeast(usize),
    /// None enforced: the source's own blank lines decide. palantir-java-format.
    Preserve,
}

impl DocumentedMember {
    /// The lines enforced around such a member, given what its kind alone would enforce.
    #[must_use]
    pub const fn resolve(self, kind: usize) -> usize {
        match self {
            Self::Inherit => kind,
            // `usize::max` is not `const`.
            Self::AtLeast(lines) => {
                if lines > kind {
                    lines
                } else {
                    kind
                }
            }
            Self::Preserve => 0,
        }
    }
}

impl Serialize for DocumentedMember {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Inherit => serializer.serialize_str("inherit"),
            Self::AtLeast(lines) => serializer.serialize_u64(*lines as u64),
            Self::Preserve => serializer.serialize_str("preserve"),
        }
    }
}

impl<'de> Deserialize<'de> for DocumentedMember {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// Accepts either of the two spellings, so the keyword and the count are one key rather
        /// than two that can disagree.
        struct Either;

        impl Visitor<'_> for Either {
            type Value = DocumentedMember;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a line count, \"inherit\", or \"preserve\"")
            }

            fn visit_u64<E: de::Error>(self, lines: u64) -> Result<Self::Value, E> {
                usize::try_from(lines)
                    .map_err(|_| E::invalid_value(Unexpected::Unsigned(lines), &self))
                    .map(DocumentedMember::AtLeast)
            }

            fn visit_i64<E: de::Error>(self, lines: i64) -> Result<Self::Value, E> {
                usize::try_from(lines)
                    .map_err(|_| E::invalid_value(Unexpected::Signed(lines), &self))
                    .map(DocumentedMember::AtLeast)
            }

            fn visit_str<E: de::Error>(self, name: &str) -> Result<Self::Value, E> {
                match name {
                    "inherit" => Ok(DocumentedMember::Inherit),
                    "preserve" => Ok(DocumentedMember::Preserve),
                    other => Err(E::invalid_value(Unexpected::Str(other), &self)),
                }
            }
        }

        deserializer.deserialize_any(Either)
    }
}

/// Blank-line counts, in lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct BlankLines {
    /// Longest run of source blank lines kept inside a method body. Eclipse
    /// `number_of_empty_lines_to_preserve` / IntelliJ `KEEP_BLANK_LINES_IN_CODE`.
    pub max_in_code: usize,
    /// Longest run of source blank lines kept between declarations. IntelliJ
    /// `KEEP_BLANK_LINES_IN_DECLARATIONS`.
    pub max_in_declarations: usize,
    /// Longest run of source blank lines kept immediately before a closing `}`. Eclipse
    /// `number_of_blank_lines_at_end_of_code_block` / IntelliJ `KEEP_BLANK_LINES_BEFORE_RBRACE`.
    pub max_before_closing_brace: usize,
    /// Longest run of source blank lines kept between a Javadoc comment and the declaration it
    /// documents.
    ///
    /// Zero is google-java-format's `allowBlankAfterLastComment`, which returns false for a doc
    /// comment: the comment documents what follows, so whatever the author left between the two
    /// is not a separation. Eclipse has no rule of its own here and simply preserves up to
    /// `number_of_empty_lines_to_preserve`.
    pub max_after_doc_comment: usize,
    /// Blank lines before the `package` declaration. Eclipse `blank_lines_before_package` /
    /// IntelliJ `BLANK_LINES_BEFORE_PACKAGE`.
    pub before_package: usize,
    /// Blank lines after the `package` declaration.
    pub after_package: usize,
    /// Blank lines before the first `import`.
    pub before_imports: usize,
    /// Blank lines after the last `import`.
    pub after_imports: usize,
    /// Blank lines between two import groups. Eclipse `blank_lines_between_import_groups`;
    /// IntelliJ spells it as an `<emptyLine/>` entry inside `IMPORT_LAYOUT_TABLE`.
    pub between_import_groups: usize,
    /// Blank lines around a type declaration. Eclipse `blank_lines_between_type_declarations` /
    /// IntelliJ `BLANK_LINES_AROUND_CLASS`.
    pub around_type: usize,
    /// Blank lines after a type header, before its first member. Eclipse
    /// `blank_lines_before_first_class_body_declaration` / IntelliJ `BLANK_LINES_AFTER_CLASS_HEADER`.
    pub at_type_body_start: usize,
    /// Blank lines before a type body's closing `}`. Eclipse
    /// `blank_lines_after_last_class_body_declaration` / IntelliJ `BLANK_LINES_BEFORE_CLASS_END`.
    pub at_type_body_end: usize,
    /// Blank lines around a member that reads as its own unit: one carrying a Javadoc comment,
    /// whatever its kind, and a field whose annotations take lines of their own. See
    /// [`DocumentedMember`], which is three values because the three references answer three
    /// ways — more, less, and not at all.
    pub around_documented_member: DocumentedMember,
    /// Blank lines around a field declaration. Eclipse `blank_lines_before_field` /
    /// IntelliJ `BLANK_LINES_AROUND_FIELD`.
    pub around_field: usize,
    /// Blank lines around a method or constructor declaration. Eclipse
    /// `blank_lines_before_method` / IntelliJ `BLANK_LINES_AROUND_METHOD`.
    pub around_method: usize,
    /// Blank lines around a field declaration in an interface. IntelliJ
    /// `BLANK_LINES_AROUND_FIELD_IN_INTERFACE`; Eclipse reuses its class-scoped setting.
    pub around_field_in_interface: usize,
    /// Blank lines around a method declaration in an interface. IntelliJ
    /// `BLANK_LINES_AROUND_METHOD_IN_INTERFACE`.
    pub around_method_in_interface: usize,
    /// Blank lines around an instance / static initializer block. Eclipse
    /// `blank_lines_before_new_chunk` / IntelliJ `BLANK_LINES_AROUND_INITIALIZER`.
    pub around_initializer: usize,
    /// Blank lines at the start of a method body. Eclipse
    /// `number_of_blank_lines_at_beginning_of_method_body` / IntelliJ `BLANK_LINES_BEFORE_METHOD_BODY`.
    pub before_method_body: usize,
    /// Blank lines at the start of a non-declaration block. Eclipse
    /// `number_of_blank_lines_at_beginning_of_code_block`.
    pub at_block_start: usize,
    /// Blank lines at the end of a non-declaration block. Eclipse
    /// `number_of_blank_lines_at_end_of_code_block`.
    pub at_block_end: usize,
    /// Blank lines between two `switch` statement groups. Eclipse
    /// `blank_lines_between_statement_group_in_switch` / IntelliJ `BLANK_LINES_BETWEEN_CASE_BLOCKS`.
    pub between_switch_groups: usize,
}

impl Default for BlankLines {
    fn default() -> Self {
        Self {
            max_in_code: 1,
            max_in_declarations: 1,
            max_before_closing_brace: 0,
            max_after_doc_comment: 0,
            before_package: 0,
            after_package: 1,
            before_imports: 0,
            after_imports: 1,
            between_import_groups: 1,
            around_type: 1,
            at_type_body_start: 0,
            at_type_body_end: 0,
            around_documented_member: DocumentedMember::Inherit,
            around_field: 0,
            around_method: 1,
            around_field_in_interface: 0,
            around_method_in_interface: 1,
            around_initializer: 1,
            before_method_body: 0,
            at_block_start: 0,
            at_block_end: 0,
            between_switch_groups: 0,
        }
    }
}
