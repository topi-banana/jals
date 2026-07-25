//! Eclipse JDT importer — the complete `org.eclipse.jdt.core.formatter.*` surface.
//!
//! # Coverage
//!
//! All **416** ids: the 401 `DefaultCodeFormatterOptions.getMap()` writes back, plus the 15
//! deprecated ones JDT still honors on read (`wrap_before_binary_operator`,
//! `insert_new_line_in_empty_block`, …) and fans out into finer settings. The list lives in
//! `inventory.tsv` next to this module, is machine-extracted from
//! `DefaultCodeFormatterConstants.java`, and drives the coverage test — an id that is not
//! modeled fails the build.
//!
//! Both file forms lower to the same `key → value` map, so one model serves both: the
//! `.settings/org.eclipse.jdt.core.prefs` properties file ([`EclipsePrefs`], portable) and the
//! exported XML profile ([`EclipseXmlProfile`], `std`-gated).
//!
//! # Shape
//!
//! [`EclipseConfig`] is nine family structs rather than one 416-field struct, matching how
//! Eclipse itself groups the surface. Each family deserializes from the same flat map — no
//! `#[serde(flatten)]`, just nine passes over one `serde_json::Map`, so the behavior is
//! explicit. Every field is `Option`: absent means "the profile did not say", which leaves the
//! corresponding jals option at its default rather than at Eclipse's.
//!
//! # Projection
//!
//! [`From<EclipseConfig> for Config`] carries the subset with a jals equivalent
//! (`jals-fmt/MAPPING.md` §5). The rest — the 53 `alignment_for_*` in full, the 219
//! `insert_space_*` in full, the column-alignment settings, the Javadoc minutiae — stay here,
//! typed and named, as the option surface a future Eclipse-compatible layout engine reads.

use alloc::collections::BTreeMap;
use alloc::string::String;

use jals_config::fmt::{
    BraceStyle, Config, IndentStyle, KeepOnOneLine, ParenPositions, WrapPolicy,
};

use serde::{Deserialize, Deserializer};

use super::serde_kv::Kv;
use super::{ConfigImporter, ImportError};

mod alignment;
mod blank_lines;
mod braces;
mod comments;
mod indentation;
mod new_lines;
mod one_line;
mod spacing;
mod values;
mod wrapping;

#[cfg(test)]
mod tests;

pub use alignment::Alignments;
pub use blank_lines::BlankLines;
pub use braces::Braces;
pub use comments::Comments;
pub use indentation::Indentation;
pub use new_lines::NewLines;
pub use one_line::OneLineBodies;
pub use spacing::Spacing;
pub use values::{
    Alignment, BracePosition, Insert, OneLine, ParenthesisPositions, TabChar, TextBlockIndentation,
};
pub use wrapping::Wrapping;

/// A parsed Eclipse JDT formatter profile, in full.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EclipseConfig {
    /// Indentation, width, on/off tags, and the settings belonging to no larger family.
    pub indentation: Indentation,
    /// The 15 `brace_position_for_*` settings.
    pub braces: Braces,
    /// The 14 `keep_*_on_one_line` settings (plus the `keep_*_on_same_line` statement toggles).
    pub one_line: OneLineBodies,
    /// The `blank_lines_*` / `number_of_*` counts.
    pub blank_lines: BlankLines,
    /// The 53 `alignment_for_*` wrap bitmasks.
    pub alignment: Alignments,
    /// `wrap_before_*` break placement and `parentheses_positions_in_*`.
    pub wrapping: Wrapping,
    /// The `insert_space_*` family.
    pub spacing: Spacing,
    /// The `insert_new_line_*` family.
    pub new_lines: NewLines,
    /// The `comment.*` family.
    pub comments: Comments,
}

impl EclipseConfig {
    /// Deserialize every family from one flat `key → value` map.
    ///
    /// Nine independent passes over the same object: each family model takes the ids it names
    /// and ignores the rest, so no `#[serde(flatten)]` buffering is involved.
    pub fn from_pairs(pairs: BTreeMap<String, String>) -> Result<Self, ImportError> {
        let object = Kv::object(pairs);
        Ok(Self {
            indentation: Kv::from_object(&object)?,
            braces: Kv::from_object(&object)?,
            one_line: Kv::from_object(&object)?,
            blank_lines: Kv::from_object(&object)?,
            alignment: Kv::from_object(&object)?,
            wrapping: Kv::from_object(&object)?,
            spacing: Kv::from_object(&object)?,
            new_lines: Kv::from_object(&object)?,
            comments: Kv::from_object(&object)?,
        })
    }
}

/// Reads the same flat `id → value` map the two file forms lower to.
///
/// A profile is *not* a nested document: an embedding such as Spotless's
/// `eclipse().configFile(...)` hands over the stringified settings verbatim, so the natural
/// serde shape is a string map, assembled into the nine families by
/// [`EclipseConfig::from_pairs`].
impl<'de> Deserialize<'de> for EclipseConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let pairs = BTreeMap::<String, String>::deserialize(deserializer)?;
        Self::from_pairs(pairs).map_err(serde::de::Error::custom)
    }
}

impl BracePosition {
    /// The jals brace style this position denotes.
    const fn to_jals(self) -> BraceStyle {
        match self {
            Self::EndOfLine => BraceStyle::SameLine,
            Self::NextLine => BraceStyle::NextLine,
            Self::NextLineShifted => BraceStyle::NextLineShifted,
            Self::NextLineOnWrap => BraceStyle::NextLineOnWrap,
        }
    }
}

impl OneLine {
    /// The jals one-line policy this value denotes. Eclipse's vocabulary is the one jals
    /// adopted, so this is an exact renaming.
    const fn to_jals(self) -> KeepOnOneLine {
        match self {
            Self::OneLineNever => KeepOnOneLine::Never,
            Self::OneLineIfEmpty => KeepOnOneLine::IfEmpty,
            Self::OneLineIfSingleItem => KeepOnOneLine::IfSingleItem,
            Self::OneLineAlways => KeepOnOneLine::Always,
            Self::OneLinePreserve => KeepOnOneLine::Preserve,
        }
    }
}

impl ParenthesisPositions {
    /// The jals delimiter placement this value denotes — again an exact renaming, since jals
    /// took Eclipse's vocabulary for `ParenPositions`.
    const fn to_jals(self) -> ParenPositions {
        match self {
            Self::CommonLines => ParenPositions::CommonLines,
            Self::SeparateLinesIfWrapped => ParenPositions::SeparateLinesIfWrapped,
            Self::SeparateLinesIfNotEmpty => ParenPositions::SeparateLinesIfNotEmpty,
            Self::SeparateLines => ParenPositions::SeparateLines,
            Self::PreservePositions => ParenPositions::Preserve,
        }
    }
}

impl Alignment {
    /// The jals wrap policy these bits denote (`MAPPING.md` §5.4).
    ///
    /// The never-wrap sentinel and a value with no split bits both mean "do not break here".
    /// Otherwise the split mode picks fill vs. one-per-line, and `M_FORCE` promotes
    /// overflow-driven wrapping to unconditional wrapping.
    const fn to_jals(self) -> WrapPolicy {
        if self.is_never() {
            return WrapPolicy::Never;
        }
        let per_item = match self.split() {
            Self::COMPACT | Self::COMPACT_FIRST_BREAK => false,
            Self::ONE_PER_LINE | Self::NEXT_SHIFTED | Self::NEXT_PER_LINE => true,
            // `M_NO_ALIGNMENT`: the position exists but never breaks.
            _ => return WrapPolicy::Never,
        };
        match (per_item, self.is_forced()) {
            (false, false) => WrapPolicy::IfLong,
            (true, false) => WrapPolicy::IfLongPerItem,
            // A forced fill still means "always break", which jals spells one-per-line.
            (_, true) => WrapPolicy::AlwaysPerItem,
        }
    }
}

/// The projection's three shapes, grouped so each call site reads as the pair being mapped.
struct Lower;

impl Lower {
    /// Set `target` from `value` when the profile declared it, applying `map`.
    ///
    /// The projection is one long sequence of "if the native setting is present, lower it";
    /// this keeps each line to the pair being mapped instead of an `if let` block.
    fn set<T, U>(target: &mut U, value: Option<T>, map: impl FnOnce(T) -> U) {
        if let Some(value) = value {
            *target = map(value);
        }
    }

    /// Lower one `alignment_for_*` bitmask onto a jals wrap policy.
    fn align(target: &mut WrapPolicy, value: Option<Alignment>) {
        Self::set(target, value, Alignment::to_jals);
    }

    /// Fold Eclipse's before/after pair for one token role onto jals's single key.
    ///
    /// Eclipse states spacing twice per role; jals has one key. A role counts as spaced when
    /// either side inserts, and the jals default survives when the profile declared neither.
    fn around(target: &mut bool, before: Option<Insert>, after: Option<Insert>) {
        if before.is_some() || after.is_some() {
            *target = before.is_some_and(Insert::is_insert) || after.is_some_and(Insert::is_insert);
        }
    }
}

impl From<EclipseConfig> for Config {
    fn from(native: EclipseConfig) -> Self {
        let mut config = Self::default();
        let EclipseConfig {
            indentation,
            braces,
            one_line,
            blank_lines,
            alignment,
            wrapping,
            spacing,
            new_lines,
            comments,
        } = native;

        // --- [layout] -------------------------------------------------------------------
        let layout = &mut config.layout;
        Lower::set(
            &mut layout.indent_style,
            indentation.tabulation_char,
            |tab| match tab {
                TabChar::Space => IndentStyle::Space,
                TabChar::Tab => IndentStyle::Tab,
                TabChar::Mixed => IndentStyle::Mixed,
            },
        );
        Lower::set(&mut layout.tab_width, indentation.tabulation_size, |n| n);
        // Under `mixed`, `indentation.size` is the logical level width and `tabulation.size` is
        // only the tab stop; otherwise the two are the same knob.
        let indent_width = if indentation.tabulation_char == Some(TabChar::Mixed) {
            indentation.indentation_size.or(indentation.tabulation_size)
        } else {
            indentation.tabulation_size
        };
        Lower::set(&mut layout.indent_width, indent_width, |n| n);
        // Eclipse counts the continuation indent in indentation *levels*; jals wants columns.
        // `saturating_mul` guards a pathological level count (usize is 32-bit on wasm).
        let level_cols = layout.indent_width;
        Lower::set(
            &mut layout.continuation_indent,
            indentation.continuation_indentation,
            |levels| Some(levels.saturating_mul(level_cols)),
        );
        Lower::set(&mut layout.max_width, indentation.line_split, |n| n);
        Lower::set(
            &mut layout.indent_empty_lines,
            indentation.indent_empty_lines,
            |b| b,
        );
        Lower::set(
            &mut layout.indent_switch_labels,
            indentation.indent_switchstatements_compare_to_switch,
            |b| b,
        );
        Lower::set(
            &mut layout.indent_switch_case_body,
            indentation.indent_switchstatements_compare_to_cases,
            |b| b,
        );
        Lower::set(
            &mut layout.indent_type_members,
            indentation.indent_body_declarations_compare_to_type_header,
            |b| b,
        );
        Lower::set(
            &mut layout.formatter_tags,
            indentation.use_on_off_tags,
            |b| b,
        );
        Lower::set(
            &mut layout.formatter_off_tag,
            indentation.disabling_tag,
            |tag| tag,
        );
        Lower::set(
            &mut layout.formatter_on_tag,
            indentation.enabling_tag,
            |tag| tag,
        );
        Lower::set(
            &mut layout.insert_final_newline,
            new_lines.insert_new_line_at_end_of_file_if_missing,
            Insert::is_insert,
        );

        // --- [blank-lines] --------------------------------------------------------------
        let blanks = &mut config.blank_lines;
        // Eclipse has one preserve count for both contexts.
        Lower::set(
            &mut blanks.max_in_code,
            blank_lines.number_of_empty_lines_to_preserve,
            |n| n,
        );
        Lower::set(
            &mut blanks.max_in_declarations,
            blank_lines.number_of_empty_lines_to_preserve,
            |n| n,
        );
        Lower::set(
            &mut blanks.before_package,
            blank_lines.blank_lines_before_package,
            |n| n,
        );
        Lower::set(
            &mut blanks.after_package,
            blank_lines.blank_lines_after_package,
            |n| n,
        );
        Lower::set(
            &mut blanks.before_imports,
            blank_lines.blank_lines_before_imports,
            |n| n,
        );
        Lower::set(
            &mut blanks.after_imports,
            blank_lines.blank_lines_after_imports,
            |n| n,
        );
        Lower::set(
            &mut blanks.between_import_groups,
            blank_lines.blank_lines_between_import_groups,
            |n| n,
        );
        Lower::set(
            &mut blanks.around_type,
            blank_lines.blank_lines_between_type_declarations,
            |n| n,
        );
        Lower::set(
            &mut blanks.at_type_body_start,
            blank_lines.blank_lines_before_first_class_body_declaration,
            |n| n,
        );
        Lower::set(
            &mut blanks.at_type_body_end,
            blank_lines.blank_lines_after_last_class_body_declaration,
            |n| n,
        );
        Lower::set(
            &mut blanks.around_field,
            blank_lines.blank_lines_before_field,
            |n| n,
        );
        Lower::set(
            &mut blanks.around_method,
            blank_lines.blank_lines_before_method,
            |n| n,
        );
        Lower::set(
            &mut blanks.around_initializer,
            blank_lines.blank_lines_before_new_chunk,
            |n| n,
        );
        Lower::set(
            &mut blanks.before_method_body,
            blank_lines.number_of_blank_lines_at_beginning_of_method_body,
            |n| n,
        );
        Lower::set(
            &mut blanks.at_block_start,
            blank_lines.number_of_blank_lines_at_beginning_of_code_block,
            |n| n,
        );
        Lower::set(
            &mut blanks.at_block_end,
            blank_lines.number_of_blank_lines_at_end_of_code_block,
            |n| n,
        );
        Lower::set(
            &mut blanks.between_switch_groups,
            blank_lines.blank_lines_between_statement_group_in_switch,
            |n| n,
        );

        // --- [braces] -------------------------------------------------------------------
        let jbraces = &mut config.braces;
        Lower::set(
            &mut jbraces.type_declaration,
            braces.brace_position_for_type_declaration,
            BracePosition::to_jals,
        );
        Lower::set(
            &mut jbraces.method_declaration,
            braces.brace_position_for_method_declaration,
            BracePosition::to_jals,
        );
        Lower::set(
            &mut jbraces.block,
            braces.brace_position_for_block,
            BracePosition::to_jals,
        );
        Lower::set(
            &mut jbraces.lambda_body,
            braces.brace_position_for_lambda_body,
            BracePosition::to_jals,
        );
        Lower::set(
            &mut jbraces.switch,
            braces.brace_position_for_switch,
            BracePosition::to_jals,
        );
        Lower::set(
            &mut jbraces.array_initializer,
            braces.brace_position_for_array_initializer,
            BracePosition::to_jals,
        );
        Lower::set(
            &mut jbraces.else_on_new_line,
            new_lines.insert_new_line_before_else_in_if_statement,
            Insert::is_insert,
        );
        Lower::set(
            &mut jbraces.while_on_new_line,
            new_lines.insert_new_line_before_while_in_do_statement,
            Insert::is_insert,
        );
        Lower::set(
            &mut jbraces.catch_on_new_line,
            new_lines.insert_new_line_before_catch_in_try_statement,
            Insert::is_insert,
        );
        Lower::set(
            &mut jbraces.finally_on_new_line,
            new_lines.insert_new_line_before_finally_in_try_statement,
            Insert::is_insert,
        );
        Lower::set(
            &mut jbraces.compact_else_if,
            indentation.compact_else_if,
            |b| b,
        );
        Lower::set(
            &mut jbraces.keep_type_body_on_one_line,
            one_line.keep_type_declaration_on_one_line,
            OneLine::to_jals,
        );
        Lower::set(
            &mut jbraces.keep_method_body_on_one_line,
            one_line.keep_method_body_on_one_line,
            OneLine::to_jals,
        );
        Lower::set(
            &mut jbraces.keep_block_on_one_line,
            one_line.keep_code_block_on_one_line,
            OneLine::to_jals,
        );
        Lower::set(
            &mut jbraces.keep_lambda_body_on_one_line,
            one_line.keep_lambda_body_block_on_one_line,
            OneLine::to_jals,
        );
        Lower::set(
            &mut jbraces.keep_switch_body_on_one_line,
            one_line.keep_switch_body_block_on_one_line,
            OneLine::to_jals,
        );
        Lower::set(
            &mut jbraces.keep_enum_declaration_on_one_line,
            one_line.keep_enum_declaration_on_one_line,
            OneLine::to_jals,
        );
        Lower::set(
            &mut jbraces.keep_record_declaration_on_one_line,
            one_line.keep_record_declaration_on_one_line,
            OneLine::to_jals,
        );
        Lower::set(
            &mut jbraces.keep_annotation_declaration_on_one_line,
            one_line.keep_annotation_declaration_on_one_line,
            OneLine::to_jals,
        );
        // Note the id's typo: Eclipse ships `keep_imple_if_on_one_line`.
        Lower::set(
            &mut jbraces.keep_control_statement_on_one_line,
            one_line.keep_imple_if_on_one_line,
            |b| b,
        );

        // --- [wrapping] -----------------------------------------------------------------
        let wrap = &mut config.wrapping;
        Lower::align(
            &mut wrap.call_arguments,
            alignment.alignment_for_arguments_in_method_invocation,
        );
        Lower::align(
            &mut wrap.method_parameters,
            alignment.alignment_for_parameters_in_method_declaration,
        );
        Lower::align(
            &mut wrap.record_components,
            alignment.alignment_for_record_components,
        );
        Lower::align(
            &mut wrap.resource_list,
            alignment.alignment_for_resources_in_try,
        );
        Lower::align(
            &mut wrap.throws_list,
            alignment.alignment_for_throws_clause_in_method_declaration,
        );
        Lower::align(
            &mut wrap.extends_list,
            alignment.alignment_for_superinterfaces_in_type_declaration,
        );
        Lower::align(
            &mut wrap.enum_constants,
            alignment.alignment_for_enum_constants,
        );
        Lower::align(
            &mut wrap.array_initializer,
            alignment.alignment_for_expressions_in_array_initializer,
        );
        Lower::align(
            &mut wrap.annotation_arguments,
            alignment.alignment_for_arguments_in_annotation,
        );
        Lower::align(
            &mut wrap.type_arguments,
            alignment.alignment_for_type_arguments,
        );
        Lower::align(
            &mut wrap.type_parameters,
            alignment.alignment_for_type_parameters,
        );
        Lower::align(
            &mut wrap.multi_catch_types,
            alignment.alignment_for_union_type_in_multicatch,
        );
        Lower::align(
            &mut wrap.case_labels,
            alignment.alignment_for_expressions_in_switch_case_with_colon,
        );
        Lower::align(
            &mut wrap.method_chain,
            alignment.alignment_for_selector_in_method_invocation,
        );
        // Eclipse splits binary wrapping across seven operator classes; the additive one is
        // taken as the representative and the other six stay in the native model.
        Lower::align(
            &mut wrap.binary_operation,
            alignment
                .alignment_for_additive_operator
                .or(alignment.alignment_for_binary_expression),
        );
        Lower::align(
            &mut wrap.ternary,
            alignment.alignment_for_conditional_expression,
        );
        Lower::align(&mut wrap.assignment, alignment.alignment_for_assignment);
        Lower::align(
            &mut wrap.for_statement,
            alignment.alignment_for_expressions_in_for_loop_header,
        );
        Lower::align(
            &mut wrap.assert_statement,
            alignment.alignment_for_assertion_message,
        );
        Lower::align(
            &mut wrap.switch_expression,
            alignment.alignment_for_expressions_in_switch_case_with_arrow,
        );
        // Eclipse decides annotation placement with an `insert_new_line_*` toggle rather than a
        // wrap policy: a newline after every annotation is jals's `always-per-item`.
        let annotation_wrap = |insert: Insert| {
            if insert.is_insert() {
                WrapPolicy::AlwaysPerItem
            } else {
                WrapPolicy::Never
            }
        };
        Lower::set(
            &mut wrap.type_annotations,
            new_lines
                .insert_new_line_after_annotation_on_type
                .or(new_lines.insert_new_line_after_annotation),
            annotation_wrap,
        );
        Lower::set(
            &mut wrap.method_annotations,
            new_lines
                .insert_new_line_after_annotation_on_method
                .or(new_lines.insert_new_line_after_annotation_on_member),
            annotation_wrap,
        );
        Lower::set(
            &mut wrap.field_annotations,
            new_lines
                .insert_new_line_after_annotation_on_field
                .or(new_lines.insert_new_line_after_annotation_on_member),
            annotation_wrap,
        );
        Lower::set(
            &mut wrap.parameter_annotations,
            new_lines.insert_new_line_after_annotation_on_parameter,
            annotation_wrap,
        );
        Lower::set(
            &mut wrap.variable_annotations,
            new_lines.insert_new_line_after_annotation_on_local_variable,
            annotation_wrap,
        );
        Lower::set(
            &mut wrap.before_binary_operator,
            wrapping
                .wrap_before_additive_operator
                .or(wrapping.wrap_before_binary_operator),
            |b| b,
        );
        Lower::set(
            &mut wrap.before_ternary_operator,
            wrapping.wrap_before_conditional_operator,
            |b| b,
        );
        Lower::set(
            &mut wrap.before_assignment_operator,
            wrapping.wrap_before_assignment_operator,
            |b| b,
        );
        Lower::set(
            &mut wrap.before_assert_colon,
            wrapping.wrap_before_assertion_message_operator,
            |b| b,
        );
        Lower::set(
            &mut wrap.paren_method_declaration,
            // Eclipse ships this id with a typo: `..._method_delcaration`.
            wrapping.parentheses_positions_in_method_delcaration,
            ParenthesisPositions::to_jals,
        );
        Lower::set(
            &mut wrap.paren_method_invocation,
            wrapping.parentheses_positions_in_method_invocation,
            ParenthesisPositions::to_jals,
        );
        Lower::set(
            &mut wrap.paren_control,
            wrapping.parentheses_positions_in_if_while_statement,
            ParenthesisPositions::to_jals,
        );
        Lower::set(
            &mut wrap.paren_annotation,
            wrapping.parentheses_positions_in_annotation,
            ParenthesisPositions::to_jals,
        );
        Lower::set(
            &mut wrap.paren_lambda,
            wrapping.parentheses_positions_in_lambda_declaration,
            ParenthesisPositions::to_jals,
        );
        Lower::set(
            &mut wrap.paren_record,
            wrapping.parentheses_positions_in_record_declaration,
            ParenthesisPositions::to_jals,
        );
        Lower::set(
            &mut wrap.join_wrapped_lines,
            indentation.join_wrapped_lines,
            |b| b,
        );

        // --- [spacing] ------------------------------------------------------------------
        // Eclipse states each of these twice (before / after the token); jals has one key per
        // token role, so the pair is folded with `||` — a space on either side means the role
        // is spaced.
        let space = &mut config.spacing;
        Lower::around(
            &mut space.around_assignment_operators,
            spacing.insert_space_before_assignment_operator,
            spacing.insert_space_after_assignment_operator,
        );
        Lower::around(
            &mut space.around_logical_operators,
            spacing.insert_space_before_logical_operator,
            spacing.insert_space_after_logical_operator,
        );
        Lower::around(
            &mut space.around_equality_operators,
            spacing.insert_space_before_relational_operator,
            spacing.insert_space_after_relational_operator,
        );
        Lower::around(
            &mut space.around_relational_operators,
            spacing.insert_space_before_relational_operator,
            spacing.insert_space_after_relational_operator,
        );
        Lower::around(
            &mut space.around_bitwise_operators,
            spacing.insert_space_before_bitwise_operator,
            spacing.insert_space_after_bitwise_operator,
        );
        Lower::around(
            &mut space.around_additive_operators,
            spacing.insert_space_before_additive_operator,
            spacing.insert_space_after_additive_operator,
        );
        Lower::around(
            &mut space.around_multiplicative_operators,
            spacing.insert_space_before_multiplicative_operator,
            spacing.insert_space_after_multiplicative_operator,
        );
        Lower::around(
            &mut space.around_shift_operators,
            spacing.insert_space_before_shift_operator,
            spacing.insert_space_after_shift_operator,
        );
        Lower::around(
            &mut space.around_unary_operator,
            spacing.insert_space_before_unary_operator,
            spacing.insert_space_after_unary_operator,
        );
        Lower::around(
            &mut space.around_lambda_arrow,
            spacing.insert_space_before_lambda_arrow,
            spacing.insert_space_after_lambda_arrow,
        );
        Lower::around(
            &mut space.around_type_bounds,
            spacing.insert_space_before_and_in_type_parameter,
            spacing.insert_space_after_and_in_type_parameter,
        );
        Lower::set(
            &mut space.before_comma,
            spacing.insert_space_before_comma_in_method_invocation_arguments,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.after_comma,
            spacing.insert_space_after_comma_in_method_invocation_arguments,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_semicolon,
            spacing.insert_space_before_semicolon_in_for,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.after_semicolon,
            spacing.insert_space_after_semicolon_in_for,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_method_call_parentheses,
            spacing.insert_space_before_opening_paren_in_method_invocation,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_method_parentheses,
            spacing.insert_space_before_opening_paren_in_method_declaration,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_keyword_parentheses,
            spacing.insert_space_before_opening_paren_in_if,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_annotation_parentheses,
            spacing.insert_space_before_opening_paren_in_annotation,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.within_method_call_parentheses,
            spacing.insert_space_after_opening_paren_in_method_invocation,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.within_method_parentheses,
            spacing.insert_space_after_opening_paren_in_method_declaration,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.within_keyword_parentheses,
            spacing.insert_space_after_opening_paren_in_if,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.within_cast_parentheses,
            spacing.insert_space_after_opening_paren_in_cast,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.within_annotation_parentheses,
            spacing.insert_space_after_opening_paren_in_annotation,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.within_brackets,
            spacing.insert_space_after_opening_bracket_in_array_reference,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.within_array_initializer_braces,
            spacing.insert_space_after_opening_brace_in_array_initializer,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.within_angle_brackets,
            spacing.insert_space_after_opening_angle_bracket_in_type_arguments,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.within_record_header,
            spacing.insert_space_after_opening_paren_in_record_declaration,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.within_empty_parentheses,
            spacing.insert_space_between_empty_parens_in_method_declaration,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.within_empty_braces,
            spacing.insert_space_between_empty_braces_in_array_initializer,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_left_brace,
            spacing.insert_space_before_opening_brace_in_block,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_array_initializer_left_brace,
            spacing.insert_space_before_opening_brace_in_array_initializer,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.after_type_cast,
            spacing.insert_space_after_closing_paren_in_cast,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_type_parameter_list,
            spacing.insert_space_before_opening_angle_bracket_in_type_parameters,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_ternary_question,
            spacing.insert_space_before_question_in_conditional,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.after_ternary_question,
            spacing.insert_space_after_question_in_conditional,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_ternary_colon,
            spacing.insert_space_before_colon_in_conditional,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.after_ternary_colon,
            spacing.insert_space_after_colon_in_conditional,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_foreach_colon,
            spacing.insert_space_before_colon_in_for,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.after_foreach_colon,
            spacing.insert_space_after_colon_in_for,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_label_colon,
            spacing.insert_space_before_colon_in_labeled_statement,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.after_label_colon,
            spacing.insert_space_after_colon_in_labeled_statement,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_case_colon,
            spacing.insert_space_before_colon_in_case,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.after_case_colon,
            spacing.insert_space_after_colon_in_case,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.before_assert_colon,
            spacing.insert_space_before_colon_in_assert,
            Insert::is_insert,
        );
        Lower::set(
            &mut space.after_assert_colon,
            spacing.insert_space_after_colon_in_assert,
            Insert::is_insert,
        );

        // --- [comments] -----------------------------------------------------------------
        let jcomments = &mut config.comments;
        Lower::set(
            &mut jcomments.format_line,
            comments
                .comment_format_line_comments
                .or(comments.comment_format_comments),
            |b| b,
        );
        Lower::set(
            &mut jcomments.format_block,
            comments
                .comment_format_block_comments
                .or(comments.comment_format_comments),
            |b| b,
        );
        Lower::set(
            &mut jcomments.format_javadoc,
            comments
                .comment_format_javadoc_comments
                .or(comments.comment_format_comments),
            |b| b,
        );
        Lower::set(
            &mut jcomments.format_header,
            comments.comment_format_header,
            |b| b,
        );
        Lower::set(
            &mut jcomments.format_html,
            comments.comment_format_html,
            |b| b,
        );
        Lower::set(
            &mut jcomments.format_source_in_comments,
            comments.comment_format_source_code,
            |b| b,
        );
        Lower::set(&mut jcomments.width, comments.comment_line_length, |n| n);
        Lower::set(
            &mut jcomments.count_width_from_start,
            comments.comment_count_line_length_from_starting_position,
            |b| b,
        );
        Lower::set(
            &mut jcomments.preserve_blank_lines,
            comments
                .comment_clear_blank_lines_in_javadoc_comment
                .or(comments.comment_clear_blank_lines),
            |clear| !clear,
        );
        Lower::set(
            &mut jcomments.blank_line_before_tags,
            comments.comment_insert_new_line_before_root_tags,
            Insert::is_insert,
        );
        Lower::set(
            &mut jcomments.align_tag_descriptions,
            comments.comment_align_tags_names_descriptions,
            |b| b,
        );
        Lower::set(
            &mut jcomments.indent_tag_description,
            comments.comment_indent_tag_description,
            |b| b,
        );

        config
    }
}

/// Importer for the `.settings/org.eclipse.jdt.core.prefs` properties file (portable).
#[derive(Debug, Clone, Copy, Default)]
pub struct EclipsePrefs;

impl ConfigImporter for EclipsePrefs {
    type Native = EclipseConfig;

    fn parse(src: &str) -> Result<Self::Native, ImportError> {
        EclipseConfig::from_pairs(super::text::Properties::parse(src))
    }
}

/// Importer for an exported Eclipse XML formatter profile. Needs the XML reader, so it is gated
/// behind the `std` feature.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, Default)]
pub struct EclipseXmlProfile;

#[cfg(feature = "std")]
impl ConfigImporter for EclipseXmlProfile {
    type Native = EclipseConfig;

    fn parse(src: &str) -> Result<Self::Native, ImportError> {
        EclipseConfig::from_pairs(super::xml::EclipseProfileReader::parse(src)?)
    }
}
