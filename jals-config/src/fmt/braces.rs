//! `[braces]` — brace placement, brace forcing, and one-line collapsing.
//!
//! Three coupled decisions live here because every native formatter couples them: where a `{`
//! goes, whether a braceless body gets braces added, and whether a short body is allowed to stay
//! on one line. See `jals-fmt/MAPPING.md` §5.3.

use serde::{Deserialize, Serialize};

/// Where the opening brace of a construct is placed.
///
/// The union of both vendors' vocabularies — Eclipse's four `brace_position_for_*` values and
/// IntelliJ's five `*_BRACE_STYLE` values — so neither collapses on import
/// (`MAPPING.md` §5.3 has the conversion table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BraceStyle {
    /// K&R: the brace stays on the header's line. Eclipse `end_of_line` / IntelliJ `end_of_line`.
    SameLine,
    /// Allman: the brace goes on its own line, aligned with the header. Eclipse / IntelliJ
    /// `next_line`.
    NextLine,
    /// Whitesmiths: brace *and* body indented one extra level. Eclipse `next_line_shifted` /
    /// IntelliJ `whitesmiths`.
    NextLineShifted,
    /// GNU: only the braces are indented one extra level, the body is not. IntelliJ `gnu`;
    /// Eclipse has no equivalent.
    NextLineShiftedBraces,
    /// On its own line only when the header itself wrapped. Eclipse `next_line_on_wrap` /
    /// IntelliJ `next_line_if_wrapped`.
    NextLineOnWrap,
}

/// Whether a control-flow statement's braceless body gains braces.
///
/// The only rule in this crate that *adds* significant tokens, so it defaults to
/// [`Never`](Self::Never) and the strict token-sequence invariant holds unless opted into.
/// IntelliJ `IF_BRACE_FORCE` and friends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForceBraces {
    /// Leave a braceless body exactly as written. The default.
    Never,
    /// Add braces only when the statement spans more than one line. The only rule whose
    /// condition consumes the engine's own line-breaking result, so its idempotency is a tested
    /// property rather than a constructive one (`DESIGN.md` §8.1 / §17).
    IfMultiline,
    /// Always add braces.
    Always,
}

/// Whether a body may occupy a single line.
///
/// Eclipse's five-valued `keep_*_on_one_line` vocabulary, which is a strict superset of
/// IntelliJ's `KEEP_SIMPLE_*_IN_ONE_LINE` booleans (`false` ⇒ [`Never`](Self::Never),
/// `true` ⇒ [`Preserve`](Self::Preserve)). Replaces the former `empty-item-single-line` /
/// `fn-single-line` / `force-multiline-blocks` trio, whose interactions could only be expressed
/// in prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeepOnOneLine {
    /// Always lay the body out across lines, even when empty. Eclipse `one_line_never`.
    Never,
    /// Collapse to `{}` only when the body is empty. Eclipse `one_line_if_empty`.
    IfEmpty,
    /// Collapse when the body is empty or holds exactly one item. Eclipse
    /// `one_line_if_single_item`.
    IfSingleItem,
    /// Collapse whenever the body fits the column limit. Eclipse `one_line_always`.
    Always,
    /// Keep the body on one line iff the source had it there. Eclipse `one_line_preserve` /
    /// IntelliJ `KEEP_SIMPLE_*_IN_ONE_LINE = true`. Reads input whitespace, which the single
    /// engine does not do: it rounds this to [`IfSingleItem`](Self::IfSingleItem) — the closest
    /// structural approximation of the same intent — and warns (`DESIGN.md` §17).
    Preserve,
}

/// Brace placement, forcing, and one-line collapsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
#[allow(clippy::struct_excessive_bools)]
pub struct Braces {
    /// Opening brace of a type body (`class` / `interface` / `enum` / `record` /
    /// `@interface` / anonymous class). Eclipse `brace_position_for_type_declaration` /
    /// IntelliJ `CLASS_BRACE_STYLE`.
    pub type_declaration: BraceStyle,
    /// Opening brace of a method, constructor, or initializer body. Eclipse
    /// `brace_position_for_method_declaration` / IntelliJ `METHOD_BRACE_STYLE`.
    pub method_declaration: BraceStyle,
    /// Opening brace of a control-flow or bare block. Eclipse `brace_position_for_block` /
    /// IntelliJ `BRACE_STYLE` (spelled `block_brace_style` in `.editorconfig`).
    pub block: BraceStyle,
    /// Opening brace of a block-bodied lambda. Eclipse `brace_position_for_lambda_body` /
    /// IntelliJ `LAMBDA_BRACE_STYLE`.
    pub lambda_body: BraceStyle,
    /// Opening brace of a `switch` block. Eclipse `brace_position_for_switch`.
    pub switch: BraceStyle,
    /// Opening brace of an array initializer. Eclipse `brace_position_for_array_initializer`.
    pub array_initializer: BraceStyle,
    /// Put `else` on the line after the closing `}`. Eclipse
    /// `insert_new_line_before_else_in_if_statement` (inverted) / IntelliJ `ELSE_ON_NEW_LINE`.
    pub else_on_new_line: bool,
    /// Put a `do`-`while`'s `while` on the line after the closing `}`. IntelliJ `WHILE_ON_NEW_LINE`.
    pub while_on_new_line: bool,
    /// Put `catch` on the line after the closing `}`. Eclipse
    /// `insert_new_line_before_catch_in_try_statement` / IntelliJ `CATCH_ON_NEW_LINE`.
    pub catch_on_new_line: bool,
    /// Put `finally` on the line after the closing `}`. Eclipse
    /// `insert_new_line_before_finally_in_try_statement` / IntelliJ `FINALLY_ON_NEW_LINE`.
    pub finally_on_new_line: bool,
    /// Keep `else if` on one line instead of nesting the inner `if` a level deeper. Eclipse
    /// `compact_else_if` / IntelliJ `SPECIAL_ELSE_IF_TREATMENT`.
    pub compact_else_if: bool,
    /// Force braces on an `if` / `else` body. IntelliJ `IF_BRACE_FORCE`.
    pub force_if: ForceBraces,
    /// Force braces on a `for` / for-each body. IntelliJ `FOR_BRACE_FORCE`.
    pub force_for: ForceBraces,
    /// Force braces on a `while` body. IntelliJ `WHILE_BRACE_FORCE`.
    pub force_while: ForceBraces,
    /// Force braces on a `do`-`while` body. IntelliJ `DOWHILE_BRACE_FORCE`.
    pub force_do_while: ForceBraces,
    /// Wrap an arrow `case`'s body in a block — `case A -> run();` to `case A -> { run(); }`.
    ///
    /// Mirrors rustfmt's `match_arm_blocks`, but defaults to [`ForceBraces::Never`] where
    /// rustfmt's default is `true`: no Java formatter adds braces unasked, and the four
    /// `force-*` rules above are all off for the same reason. IntelliJ's `*_BRACE_FORCE` is the
    /// vendor behind those four; an arrow `case` has no counterpart, so this key is jals-native.
    ///
    /// **Statement switches only.** A `case` of a switch *expression* has to produce a value, so
    /// braces alone do not do it — `case A -> f();` would have to become `case A -> { yield f(); }`,
    /// which inserts a keyword and changes what the arm *means*. That is a rewrite rather than a
    /// layout decision, so a switch expression's arms are left exactly as written whatever this is
    /// set to. An arm whose body is already a block, or a `throw`, is likewise untouched.
    pub force_switch_arm: ForceBraces,
    /// One-line collapsing of a type body. Eclipse `keep_type_declaration_on_one_line` /
    /// IntelliJ `KEEP_SIMPLE_CLASSES_IN_ONE_LINE`.
    pub keep_type_body_on_one_line: KeepOnOneLine,
    /// One-line collapsing of a method, constructor, or initializer body. Eclipse
    /// `keep_method_body_on_one_line` / IntelliJ `KEEP_SIMPLE_METHODS_IN_ONE_LINE`.
    pub keep_method_body_on_one_line: KeepOnOneLine,
    /// One-line collapsing of a control-flow or bare block. Eclipse
    /// `keep_code_block_on_one_line` / IntelliJ `KEEP_SIMPLE_BLOCKS_IN_ONE_LINE`.
    pub keep_block_on_one_line: KeepOnOneLine,
    /// One-line collapsing of a block-bodied lambda. Eclipse
    /// `keep_lambda_body_block_on_one_line` / IntelliJ `KEEP_SIMPLE_LAMBDAS_IN_ONE_LINE`.
    pub keep_lambda_body_on_one_line: KeepOnOneLine,
    /// One-line collapsing of a `switch` block. Eclipse `keep_switch_body_block_on_one_line`.
    pub keep_switch_body_on_one_line: KeepOnOneLine,
    /// One-line collapsing of an `enum` body. Eclipse `keep_enum_declaration_on_one_line`.
    pub keep_enum_declaration_on_one_line: KeepOnOneLine,
    /// One-line collapsing of a `record` body. Eclipse `keep_record_declaration_on_one_line`.
    pub keep_record_declaration_on_one_line: KeepOnOneLine,
    /// One-line collapsing of an `@interface` body. Eclipse
    /// `keep_annotation_declaration_on_one_line`.
    pub keep_annotation_declaration_on_one_line: KeepOnOneLine,
    /// Keep a braceless control-flow statement and its body on one line (`if (x) return;`).
    /// Eclipse `keep_simple_if_on_one_line` / IntelliJ `KEEP_CONTROL_STATEMENT_IN_ONE_LINE`.
    pub keep_control_statement_on_one_line: bool,
}

impl Default for Braces {
    fn default() -> Self {
        Self {
            type_declaration: BraceStyle::SameLine,
            method_declaration: BraceStyle::SameLine,
            block: BraceStyle::SameLine,
            lambda_body: BraceStyle::SameLine,
            switch: BraceStyle::SameLine,
            array_initializer: BraceStyle::SameLine,
            else_on_new_line: false,
            while_on_new_line: false,
            catch_on_new_line: false,
            finally_on_new_line: false,
            compact_else_if: true,
            force_if: ForceBraces::Never,
            force_for: ForceBraces::Never,
            force_while: ForceBraces::Never,
            force_do_while: ForceBraces::Never,
            force_switch_arm: ForceBraces::Never,
            keep_type_body_on_one_line: KeepOnOneLine::IfEmpty,
            keep_method_body_on_one_line: KeepOnOneLine::IfEmpty,
            keep_block_on_one_line: KeepOnOneLine::IfEmpty,
            keep_lambda_body_on_one_line: KeepOnOneLine::IfEmpty,
            keep_switch_body_on_one_line: KeepOnOneLine::IfEmpty,
            keep_enum_declaration_on_one_line: KeepOnOneLine::IfEmpty,
            keep_record_declaration_on_one_line: KeepOnOneLine::IfEmpty,
            keep_annotation_declaration_on_one_line: KeepOnOneLine::IfEmpty,
            keep_control_statement_on_one_line: false,
        }
    }
}
