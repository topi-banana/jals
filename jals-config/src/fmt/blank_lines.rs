//! `[blank-lines]` — how many empty lines survive, and how many are enforced.
//!
//! Two distinct concepts share this section, exactly as they do in every native formatter:
//!
//! - **`max-*`** clamps the blank lines *already present in the source* (Eclipse
//!   `number_of_empty_lines_to_preserve` / IntelliJ `KEEP_BLANK_LINES_*`). These read input
//!   whitespace, so they only have meaning in the whitespace-retaining mode (`DESIGN.md` §17).
//! - every other key **enforces** a count at a structural position, independent of the input
//!   (Eclipse `blank_lines_*` / IntelliJ `BLANK_LINES_*`).
//!
//! The two compose the way the vendors compose them: an enforced count is a *minimum*, a `max-*`
//! is a *cap* on a run the source already had. So `at-block-start = 0` with `max-in-code = 1`
//! emits no blank line of its own but keeps one the author wrote — which is exactly
//! google-java-format's behavior at the start of a block.
//!
//! See `jals-fmt/MAPPING.md` §5.2 for the per-vendor correspondence.

use serde::Deserialize;

/// Blank-line counts, in lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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
            before_package: 0,
            after_package: 1,
            before_imports: 0,
            after_imports: 1,
            between_import_groups: 1,
            around_type: 1,
            at_type_body_start: 0,
            at_type_body_end: 0,
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
