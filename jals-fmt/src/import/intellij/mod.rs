//! IntelliJ IDEA importer — the complete Java code-style surface.
//!
//! # Coverage
//!
//! All **297** settings: the 14 `<indentOptions>` fields, the 182 `CommonCodeStyleSettings`
//! fields, the 92 `JavaCodeStyleSettings` fields (including the synthetic `REPEAT_ANNOTATIONS`
//! list accessor), the 6 language-neutral `<code_scheme>` fields, and the 3 EditorConfig core
//! properties IntelliJ honors from outside the scheme. The list lives in `inventory.tsv` next to
//! this module, is machine-extracted from the three settings classes, and drives the coverage
//! test.
//!
//! # One model, two spellings
//!
//! IntelliJ writes every setting twice over: an `UPPER_SNAKE` option name with raw integer enums
//! in a scheme XML, and a lowercase `ij_java_*` key with named tokens in `.editorconfig`. The
//! model is keyed by the **XML name**, because that spelling is total — eight `<indentOptions>`
//! settings have no editorconfig key at all — and [`IntellijEditorConfig`] translates through
//! the generated [`keys`] table. Values accept either spelling: see [`values`], where each of
//! the three *different* int→token tables lives with its own type.
//!
//! # Projection
//!
//! [`From<IntellijConfig> for Config`] carries the subset with a jals equivalent
//! (`jals-fmt/MAPPING.md` §5). What stays here, typed but unprojected, is listed in §7 of that
//! document: the 18 `ALIGN_MULTILINE_*` column-alignment settings, the naming and code-generation
//! preferences, the classpath-dependent import-on-demand thresholds, and the editor-behavior
//! knobs.

use alloc::collections::BTreeMap;
use alloc::string::String;

use jals_config::fmt::{
    BraceStyle, Config, ForceBraces, ImportOrder, IndentStyle, KeepOnOneLine, LineEnding,
    ParenPositions, WrapPolicy,
};
use serde::{Deserialize, Deserializer};

use super::serde_kv::Kv;
use super::{ConfigImporter, ImportError, ImportGroups};

mod blank_lines;
mod codegen;
mod common;
mod general;
mod imports;
mod indent;
mod javadoc;
pub(crate) mod keys;
mod naming;
mod spacing;
mod values;
mod wrapping;

#[cfg(test)]
mod tests;

pub use blank_lines::IntellijBlankLines;
pub use codegen::IntellijCodegen;
pub use common::IntellijCommon;
pub use general::IntellijGeneral;
pub use imports::IntellijImports;
pub use indent::IntellijIndent;
pub use javadoc::IntellijJavadoc;
pub use naming::IntellijNaming;
pub use spacing::IntellijSpacing;
pub use values::{IjBraceStyle, IjForceBraces, IjWrap, PackageEntry, PackageEntryTable};
pub use wrapping::IntellijWrapping;

/// A parsed IntelliJ Java code style, in full.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntellijConfig {
    /// `<indentOptions>`.
    pub indent: IntellijIndent,
    /// The `SPACE_*` / `SPACES_*` settings.
    pub spacing: IntellijSpacing,
    /// The `BLANK_LINES_*` / `KEEP_BLANK_LINES_*` counts.
    pub blank_lines: IntellijBlankLines,
    /// Wrapping, alignment, brace style, brace forcing, and one-line keeping.
    pub wrapping: IntellijWrapping,
    /// The language-common settings belonging to no larger family.
    pub common: IntellijCommon,
    /// The import-layout settings.
    pub imports: IntellijImports,
    /// The Javadoc settings.
    pub javadoc: IntellijJavadoc,
    /// The naming conventions — modeled for completeness, not formatter rules.
    pub naming: IntellijNaming,
    /// The code-generation preferences — modeled for completeness, not formatter rules.
    pub codegen: IntellijCodegen,
    /// The language-neutral scheme settings and EditorConfig core properties.
    pub general: IntellijGeneral,
}

impl IntellijConfig {
    /// Deserialize every family from one flat `setting name → value` map.
    pub fn from_pairs(pairs: BTreeMap<String, String>) -> Result<Self, ImportError> {
        let object = Kv::object(pairs);
        Ok(Self {
            indent: Kv::from_object(&object)?,
            spacing: Kv::from_object(&object)?,
            blank_lines: Kv::from_object(&object)?,
            wrapping: Kv::from_object(&object)?,
            common: Kv::from_object(&object)?,
            imports: Kv::from_object(&object)?,
            javadoc: Kv::from_object(&object)?,
            naming: Kv::from_object(&object)?,
            codegen: Kv::from_object(&object)?,
            general: Kv::from_object(&object)?,
        })
    }

    /// The XML option name an `.editorconfig` key denotes, if any.
    ///
    /// Unknown keys (another language's `ij_kotlin_*`, an EditorConfig property IntelliJ does
    /// not map, a key from a newer IDE) return `None` and are dropped rather than failing the
    /// import.
    pub(crate) fn setting_name(editorconfig_key: &str) -> Option<&'static str> {
        keys::EDITORCONFIG_KEYS
            .binary_search_by(|(key, _)| (*key).cmp(editorconfig_key))
            .ok()
            .map(|index| keys::EDITORCONFIG_KEYS[index].1)
    }
}

/// Reads the same flat `setting name → value` map both file forms lower to.
impl<'de> Deserialize<'de> for IntellijConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let pairs = BTreeMap::<String, String>::deserialize(deserializer)?;
        Self::from_pairs(pairs).map_err(serde::de::Error::custom)
    }
}

impl IjBraceStyle {
    /// The jals brace style this value denotes (`MAPPING.md` §5.3).
    const fn to_jals(self) -> BraceStyle {
        match self {
            Self::EndOfLine => BraceStyle::SameLine,
            Self::NextLine => BraceStyle::NextLine,
            Self::Whitesmiths => BraceStyle::NextLineShifted,
            Self::Gnu => BraceStyle::NextLineShiftedBraces,
            Self::NextLineIfWrapped => BraceStyle::NextLineOnWrap,
        }
    }
}

impl IjForceBraces {
    /// The jals brace-forcing policy this value denotes.
    const fn to_jals(self) -> ForceBraces {
        match self {
            Self::Never => ForceBraces::Never,
            Self::IfMultiline => ForceBraces::IfMultiline,
            Self::Always => ForceBraces::Always,
        }
    }
}

impl IjWrap {
    /// The jals wrap policy this value denotes (`MAPPING.md` §5.4).
    const fn to_jals(self) -> WrapPolicy {
        match self {
            Self::Off => WrapPolicy::Never,
            Self::Normal => WrapPolicy::IfLong,
            Self::OnEveryItem => WrapPolicy::IfLongPerItem,
            Self::SplitIntoLines => WrapPolicy::AlwaysPerItem,
        }
    }
}

impl PackageEntryTable {
    /// Lower an `IMPORT_LAYOUT_TABLE` to jals import-group prefixes.
    ///
    /// Blank-line markers are dropped — jals separates every group by
    /// `blank-lines.between-import-groups` — and every static entry collapses into the single
    /// `"static"` group jals models. Prefixes go through [`ImportGroups::prefix`] so this
    /// importer's encoding matches the Spotless one.
    ///
    /// The "all module imports" row is skipped: it selects `import module M;` declarations by
    /// project structure rather than by name prefix, so jals has nothing to map it to
    /// (`MAPPING.md` §7). Its name is empty, so projecting it would otherwise emit a second
    /// catch-all group.
    ///
    /// `with_subpackages` is likewise **not** carried: `java.*` (that package only) and `java.**`
    /// (it and everything under it) both become `"java."`, because jals matches a group by raw
    /// string prefix and has no non-recursive form. IntelliJ is the only vendor with the concept,
    /// so §7 records the collapse rather than jals growing a rule no other target can produce.
    fn to_jals_groups(&self) -> alloc::vec::Vec<String> {
        let mut groups = alloc::vec::Vec::new();
        for entry in &self.0 {
            match entry {
                PackageEntry::BlankLine
                | PackageEntry::Package {
                    is_module: true, ..
                } => {}
                PackageEntry::Package {
                    name, is_static, ..
                } => {
                    if *is_static {
                        ImportGroups::push_static(&mut groups);
                    } else {
                        groups.push(ImportGroups::prefix(name));
                    }
                }
            }
        }
        groups
    }
}

/// The projection's shapes, grouped so each call site reads as the pair being mapped.
struct Lower;

impl Lower {
    /// Set `target` from `value` when the scheme declared it, applying `map`.
    fn set<T, U>(target: &mut U, value: Option<T>, map: impl FnOnce(T) -> U) {
        if let Some(value) = value {
            *target = map(value);
        }
    }

    /// Lower one `*_WRAP` onto a jals wrap policy.
    fn wrap(target: &mut WrapPolicy, value: Option<IjWrap>) {
        Self::set(target, value, IjWrap::to_jals);
    }

    /// Lower a `(lparen_on_next_line, rparen_on_next_line)` pair onto a delimiter placement.
    ///
    /// IntelliJ's two booleans have four states; jals's vocabulary has no asymmetric value, so
    /// the two mixed states fold onto [`ParenPositions::SeparateLines`]. The original pair stays
    /// visible in the native model.
    fn parens(target: &mut ParenPositions, lparen: Option<bool>, rparen: Option<bool>) {
        if lparen.is_none() && rparen.is_none() {
            return;
        }
        *target = if lparen.unwrap_or(false) || rparen.unwrap_or(false) {
            ParenPositions::SeparateLines
        } else {
            ParenPositions::CommonLines
        };
    }

    /// Lower a `KEEP_SIMPLE_*_IN_ONE_LINE` boolean onto the five-valued jals policy.
    ///
    /// `true` means "leave it on one line if the author did", which is `preserve` — the one
    /// value that reads the input's line breaks, so the engine later rounds it to
    /// `if-single-item` (`DESIGN.md` §17); `false` means "always expand".
    const fn keep(value: bool) -> KeepOnOneLine {
        if value {
            KeepOnOneLine::Preserve
        } else {
            KeepOnOneLine::Never
        }
    }

    /// Clamp a raw IntelliJ integer to a count, mapping a negative value to zero.
    fn unsigned(value: i64) -> usize {
        usize::try_from(value).unwrap_or(0)
    }

    /// Read a width setting, discarding IntelliJ's `-1` "inherit the general setting" sentinel
    /// (and a nonsensical `0`) rather than lowering it to a zero-column width.
    fn width(value: Option<i64>) -> Option<usize> {
        value.filter(|columns| *columns > 0).map(Self::unsigned)
    }
}

impl From<IntellijConfig> for Config {
    fn from(native: IntellijConfig) -> Self {
        let mut config = Self::default();
        let IntellijConfig {
            indent,
            spacing,
            blank_lines,
            wrapping,
            common,
            imports,
            javadoc,
            general,
            // Naming and code-generation preferences are part of the `ij_java_*` surface but are
            // not formatter rules; they stay in the native model (`MAPPING.md` §7).
            naming: _,
            codegen: _,
        } = native;

        // --- [layout] -------------------------------------------------------------------
        let layout = &mut config.layout;
        Lower::set(&mut layout.indent_style, indent.use_tab_character, |tabs| {
            if tabs {
                // `SMART_TABS` is IntelliJ's name for Eclipse's `mixed`.
                if indent.smart_tabs == Some(true) {
                    IndentStyle::Mixed
                } else {
                    IndentStyle::Tab
                }
            } else {
                IndentStyle::Space
            }
        });
        // Every width below can carry IntelliJ's `-1` "inherit the general setting" sentinel,
        // which is not a width at all: only a positive value moves the jals key off its default.
        Lower::set(
            &mut layout.indent_width,
            Lower::width(indent.indent_size),
            |columns| columns,
        );
        Lower::set(
            &mut layout.tab_width,
            Lower::width(indent.tab_size),
            |columns| columns,
        );
        Lower::set(
            &mut layout.continuation_indent,
            Lower::width(indent.continuation_indent_size),
            Some,
        );
        // A negative `LABEL_INDENT_SIZE` (or the absolute flag) puts the label at column 0,
        // which jals spells as no indent — so this one clamps rather than filters.
        Lower::set(
            &mut layout.label_indent,
            indent.label_indent_size,
            Lower::unsigned,
        );
        Lower::set(
            &mut layout.indent_empty_lines,
            indent.keep_indents_on_empty_lines,
            |b| b,
        );
        Lower::set(
            &mut layout.max_width,
            Lower::width(common.right_margin),
            |columns| columns,
        );
        Lower::set(&mut layout.line_ending, general.line_separator, |sep| {
            match sep.as_str() {
                "crlf" | "\r\n" => LineEnding::Crlf,
                // IntelliJ's bare-CR terminator has no jals equivalent and falls back to LF.
                _ => LineEnding::Lf,
            }
        });
        Lower::set(
            &mut layout.insert_final_newline,
            general.insert_final_newline,
            |b| b,
        );
        Lower::set(
            &mut layout.trim_trailing_whitespace,
            general.trim_trailing_whitespace,
            |b| b,
        );
        Lower::set(
            &mut layout.indent_switch_labels,
            wrapping.indent_case_from_switch,
            |b| b,
        );
        Lower::set(
            &mut layout.indent_switch_case_body,
            wrapping.indent_break_from_case,
            |b| b,
        );
        Lower::set(
            &mut layout.indent_type_members,
            wrapping.do_not_indent_top_level_class_members,
            |skip| !skip,
        );
        Lower::set(
            &mut layout.formatter_tags,
            general.formatter_tags_enabled,
            |b| b,
        );
        Lower::set(
            &mut layout.formatter_off_tag,
            general.formatter_off_tag,
            |t| t,
        );
        Lower::set(
            &mut layout.formatter_on_tag,
            general.formatter_on_tag,
            |t| t,
        );

        // --- [blank-lines] --------------------------------------------------------------
        let blanks = &mut config.blank_lines;
        let count =
            |target: &mut usize, value: Option<i64>| Lower::set(target, value, Lower::unsigned);
        count(
            &mut blanks.max_in_code,
            blank_lines.keep_blank_lines_in_code,
        );
        count(
            &mut blanks.max_in_declarations,
            blank_lines.keep_blank_lines_in_declarations,
        );
        count(
            &mut blanks.max_before_closing_brace,
            blank_lines.keep_blank_lines_before_rbrace,
        );
        count(
            &mut blanks.before_package,
            blank_lines.blank_lines_before_package,
        );
        count(
            &mut blanks.after_package,
            blank_lines.blank_lines_after_package,
        );
        count(
            &mut blanks.before_imports,
            blank_lines.blank_lines_before_imports,
        );
        count(
            &mut blanks.after_imports,
            blank_lines.blank_lines_after_imports,
        );
        count(
            &mut blanks.around_type,
            blank_lines.blank_lines_around_class,
        );
        count(
            &mut blanks.at_type_body_start,
            blank_lines.blank_lines_after_class_header,
        );
        count(
            &mut blanks.at_type_body_end,
            blank_lines.blank_lines_before_class_end,
        );
        count(
            &mut blanks.around_field,
            blank_lines.blank_lines_around_field,
        );
        count(
            &mut blanks.around_method,
            blank_lines.blank_lines_around_method,
        );
        count(
            &mut blanks.around_field_in_interface,
            blank_lines.blank_lines_around_field_in_interface,
        );
        count(
            &mut blanks.around_method_in_interface,
            blank_lines.blank_lines_around_method_in_interface,
        );
        count(
            &mut blanks.around_initializer,
            blank_lines.blank_lines_around_initializer,
        );
        count(
            &mut blanks.before_method_body,
            blank_lines.blank_lines_before_method_body,
        );
        count(
            &mut blanks.between_switch_groups,
            blank_lines.blank_lines_between_case_blocks,
        );

        // --- [braces] -------------------------------------------------------------------
        let braces = &mut config.braces;
        Lower::set(
            &mut braces.type_declaration,
            wrapping.class_brace_style,
            IjBraceStyle::to_jals,
        );
        Lower::set(
            &mut braces.method_declaration,
            wrapping.method_brace_style,
            IjBraceStyle::to_jals,
        );
        Lower::set(
            &mut braces.block,
            wrapping.brace_style,
            IjBraceStyle::to_jals,
        );
        Lower::set(
            &mut braces.lambda_body,
            wrapping.lambda_brace_style,
            IjBraceStyle::to_jals,
        );
        // IntelliJ has no separate switch / array-initializer brace style; both follow the
        // block one.
        Lower::set(
            &mut braces.switch,
            wrapping.brace_style,
            IjBraceStyle::to_jals,
        );
        Lower::set(
            &mut braces.else_on_new_line,
            wrapping.else_on_new_line,
            |b| b,
        );
        Lower::set(
            &mut braces.while_on_new_line,
            wrapping.while_on_new_line,
            |b| b,
        );
        Lower::set(
            &mut braces.catch_on_new_line,
            wrapping.catch_on_new_line,
            |b| b,
        );
        Lower::set(
            &mut braces.finally_on_new_line,
            wrapping.finally_on_new_line,
            |b| b,
        );
        Lower::set(
            &mut braces.compact_else_if,
            wrapping.special_else_if_treatment,
            |b| b,
        );
        Lower::set(
            &mut braces.force_if,
            wrapping.if_brace_force,
            IjForceBraces::to_jals,
        );
        Lower::set(
            &mut braces.force_for,
            wrapping.for_brace_force,
            IjForceBraces::to_jals,
        );
        Lower::set(
            &mut braces.force_while,
            wrapping.while_brace_force,
            IjForceBraces::to_jals,
        );
        Lower::set(
            &mut braces.force_do_while,
            wrapping.dowhile_brace_force,
            IjForceBraces::to_jals,
        );
        Lower::set(
            &mut braces.keep_type_body_on_one_line,
            wrapping.keep_simple_classes_in_one_line,
            Lower::keep,
        );
        Lower::set(
            &mut braces.keep_method_body_on_one_line,
            wrapping.keep_simple_methods_in_one_line,
            Lower::keep,
        );
        Lower::set(
            &mut braces.keep_block_on_one_line,
            wrapping.keep_simple_blocks_in_one_line,
            Lower::keep,
        );
        Lower::set(
            &mut braces.keep_lambda_body_on_one_line,
            wrapping.keep_simple_lambdas_in_one_line,
            Lower::keep,
        );
        Lower::set(
            &mut braces.keep_control_statement_on_one_line,
            wrapping.keep_control_statement_in_one_line,
            |b| b,
        );

        // --- [wrapping] -----------------------------------------------------------------
        let wrap_cfg = &mut config.wrapping;
        Lower::wrap(&mut wrap_cfg.call_arguments, wrapping.call_parameters_wrap);
        Lower::wrap(
            &mut wrap_cfg.method_parameters,
            wrapping.method_parameters_wrap,
        );
        Lower::wrap(
            &mut wrap_cfg.record_components,
            wrapping.record_components_wrap,
        );
        Lower::wrap(&mut wrap_cfg.resource_list, wrapping.resource_list_wrap);
        Lower::wrap(&mut wrap_cfg.throws_list, wrapping.throws_list_wrap);
        Lower::wrap(&mut wrap_cfg.extends_list, wrapping.extends_list_wrap);
        Lower::wrap(&mut wrap_cfg.enum_constants, wrapping.enum_constants_wrap);
        Lower::wrap(
            &mut wrap_cfg.array_initializer,
            wrapping.array_initializer_wrap,
        );
        Lower::wrap(
            &mut wrap_cfg.annotation_arguments,
            wrapping.annotation_parameter_wrap,
        );
        Lower::wrap(
            &mut wrap_cfg.multi_catch_types,
            wrapping.multi_catch_types_wrap,
        );
        Lower::wrap(
            &mut wrap_cfg.deconstruction_list,
            wrapping.deconstruction_list_wrap,
        );
        Lower::wrap(&mut wrap_cfg.method_chain, wrapping.method_call_chain_wrap);
        Lower::wrap(
            &mut wrap_cfg.binary_operation,
            wrapping.binary_operation_wrap,
        );
        Lower::wrap(&mut wrap_cfg.ternary, wrapping.ternary_operation_wrap);
        Lower::wrap(&mut wrap_cfg.assignment, wrapping.assignment_wrap);
        Lower::wrap(&mut wrap_cfg.for_statement, wrapping.for_statement_wrap);
        Lower::wrap(
            &mut wrap_cfg.assert_statement,
            wrapping.assert_statement_wrap,
        );
        Lower::wrap(
            &mut wrap_cfg.switch_expression,
            wrapping.switch_expressions_wrap,
        );
        Lower::wrap(
            &mut wrap_cfg.type_annotations,
            wrapping.class_annotation_wrap,
        );
        Lower::wrap(
            &mut wrap_cfg.method_annotations,
            wrapping.method_annotation_wrap,
        );
        Lower::wrap(
            &mut wrap_cfg.field_annotations,
            wrapping.field_annotation_wrap,
        );
        Lower::wrap(
            &mut wrap_cfg.parameter_annotations,
            wrapping.parameter_annotation_wrap,
        );
        Lower::wrap(
            &mut wrap_cfg.variable_annotations,
            wrapping.variable_annotation_wrap,
        );
        Lower::set(
            &mut wrap_cfg.before_binary_operator,
            wrapping.binary_operation_sign_on_next_line,
            |b| b,
        );
        Lower::set(
            &mut wrap_cfg.before_ternary_operator,
            wrapping.ternary_operation_signs_on_next_line,
            |b| b,
        );
        Lower::set(
            &mut wrap_cfg.before_assignment_operator,
            wrapping.place_assignment_sign_on_next_line,
            |b| b,
        );
        Lower::set(
            &mut wrap_cfg.before_assert_colon,
            wrapping.assert_statement_colon_on_next_line,
            |b| b,
        );
        Lower::set(
            &mut wrap_cfg.wrap_first_method_in_chain,
            wrapping.wrap_first_method_in_call_chain,
            |b| b,
        );
        Lower::parens(
            &mut wrap_cfg.paren_method_declaration,
            wrapping.method_parameters_lparen_on_next_line,
            wrapping.method_parameters_rparen_on_next_line,
        );
        Lower::parens(
            &mut wrap_cfg.paren_method_invocation,
            wrapping.call_parameters_lparen_on_next_line,
            wrapping.call_parameters_rparen_on_next_line,
        );
        Lower::parens(
            &mut wrap_cfg.paren_control,
            wrapping.for_statement_lparen_on_next_line,
            wrapping.for_statement_rparen_on_next_line,
        );
        Lower::parens(
            &mut wrap_cfg.paren_annotation,
            wrapping.new_line_after_lparen_in_annotation,
            wrapping.rparen_on_new_line_in_annotation,
        );
        Lower::parens(
            &mut wrap_cfg.paren_record,
            wrapping.new_line_after_lparen_in_record_header,
            wrapping.rparen_on_new_line_in_record_header,
        );
        // IntelliJ's `KEEP_LINE_BREAKS` is the inverse of "rejoin what the source broke".
        Lower::set(
            &mut wrap_cfg.join_wrapped_lines,
            wrapping.keep_line_breaks,
            |keep_breaks| !keep_breaks,
        );
        Lower::set(
            &mut wrap_cfg.wrap_long_lines,
            wrapping.wrap_long_lines,
            |b| b,
        );

        // --- [spacing] ------------------------------------------------------------------
        let space = &mut config.spacing;
        let flag = |target: &mut bool, value: Option<bool>| Lower::set(target, value, |b| b);
        flag(
            &mut space.around_assignment_operators,
            spacing.space_around_assignment_operators,
        );
        flag(
            &mut space.around_logical_operators,
            spacing.space_around_logical_operators,
        );
        flag(
            &mut space.around_equality_operators,
            spacing.space_around_equality_operators,
        );
        flag(
            &mut space.around_relational_operators,
            spacing.space_around_relational_operators,
        );
        flag(
            &mut space.around_bitwise_operators,
            spacing.space_around_bitwise_operators,
        );
        flag(
            &mut space.around_additive_operators,
            spacing.space_around_additive_operators,
        );
        flag(
            &mut space.around_multiplicative_operators,
            spacing.space_around_multiplicative_operators,
        );
        flag(
            &mut space.around_shift_operators,
            spacing.space_around_shift_operators,
        );
        flag(
            &mut space.around_unary_operator,
            spacing.space_around_unary_operator,
        );
        flag(
            &mut space.around_lambda_arrow,
            spacing.space_around_lambda_arrow,
        );
        flag(
            &mut space.around_method_ref_double_colon,
            spacing.space_around_method_ref_dbl_colon,
        );
        flag(
            &mut space.around_type_bounds,
            spacing.space_around_type_bounds_in_type_parameters,
        );
        flag(
            &mut space.around_annotation_eq,
            spacing.space_around_annotation_eq,
        );
        flag(&mut space.before_comma, spacing.space_before_comma);
        flag(&mut space.after_comma, spacing.space_after_comma);
        flag(&mut space.before_semicolon, spacing.space_before_semicolon);
        flag(&mut space.after_semicolon, spacing.space_after_semicolon);
        flag(
            &mut space.before_method_call_parentheses,
            spacing.space_before_method_call_parentheses,
        );
        flag(
            &mut space.before_method_parentheses,
            spacing.space_before_method_parentheses,
        );
        flag(
            &mut space.before_keyword_parentheses,
            spacing.space_before_if_parentheses,
        );
        flag(
            &mut space.before_annotation_parentheses,
            spacing.space_before_anotation_parameter_list,
        );
        flag(
            &mut space.within_method_call_parentheses,
            spacing.space_within_method_call_parentheses,
        );
        flag(
            &mut space.within_method_parentheses,
            spacing.space_within_method_parentheses,
        );
        flag(
            &mut space.within_keyword_parentheses,
            spacing.space_within_if_parentheses,
        );
        flag(
            &mut space.within_cast_parentheses,
            spacing.space_within_cast_parentheses,
        );
        flag(
            &mut space.within_annotation_parentheses,
            spacing.space_within_annotation_parentheses,
        );
        flag(&mut space.within_brackets, spacing.space_within_brackets);
        flag(
            &mut space.within_array_initializer_braces,
            spacing.space_within_array_initializer_braces,
        );
        flag(
            &mut space.within_angle_brackets,
            spacing.spaces_within_angle_brackets,
        );
        flag(
            &mut space.within_record_header,
            spacing.space_within_record_header,
        );
        flag(
            &mut space.within_empty_parentheses,
            spacing.space_within_empty_method_parentheses,
        );
        flag(
            &mut space.within_empty_braces,
            spacing.space_within_empty_array_initializer_braces,
        );
        flag(
            &mut space.before_left_brace,
            spacing.space_before_class_lbrace,
        );
        flag(
            &mut space.before_array_initializer_left_brace,
            spacing.space_before_array_initializer_lbrace,
        );
        flag(
            &mut space.before_continuation_keyword,
            spacing.space_before_else_keyword,
        );
        flag(&mut space.after_type_cast, spacing.space_after_type_cast);
        flag(
            &mut space.before_type_parameter_list,
            spacing.space_before_type_parameter_list,
        );
        flag(
            &mut space.before_ternary_question,
            spacing.space_before_quest,
        );
        flag(&mut space.after_ternary_question, spacing.space_after_quest);
        flag(&mut space.before_ternary_colon, spacing.space_before_colon);
        flag(&mut space.after_ternary_colon, spacing.space_after_colon);
        flag(
            &mut space.before_foreach_colon,
            spacing.space_before_colon_in_foreach,
        );

        // --- [comments] -----------------------------------------------------------------
        let comments = &mut config.comments;
        Lower::set(
            &mut comments.format_javadoc,
            javadoc.enable_javadoc_formatting,
            |b| b,
        );
        Lower::set(&mut comments.format_line, wrapping.wrap_comments, |b| b);
        Lower::set(&mut comments.format_block, wrapping.wrap_comments, |b| b);
        // IntelliJ has no comment-specific width: it reflows against the shared right margin.
        // Only a scheme that actually declared one moves the jals key off its own default.
        if Lower::width(common.right_margin).is_some() {
            comments.width = config.layout.max_width;
        }
        Lower::set(
            &mut comments.preserve_blank_lines,
            javadoc.jd_keep_empty_lines,
            |b| b,
        );
        Lower::set(
            &mut comments.preserve_line_breaks,
            javadoc.jd_preserve_line_feeds,
            |b| b,
        );
        Lower::set(
            &mut comments.blank_line_before_tags,
            javadoc.jd_add_blank_after_description,
            |b| b,
        );
        Lower::set(
            &mut comments.align_tag_descriptions,
            javadoc.jd_align_param_comments,
            |b| b,
        );
        Lower::set(
            &mut comments.indent_tag_description,
            javadoc.jd_indent_on_continuation,
            |b| b,
        );
        Lower::set(
            &mut comments.leading_asterisks,
            javadoc.jd_leading_asterisks_are_enabled,
            |b| b,
        );

        // --- [imports] ------------------------------------------------------------------
        if let Some(table) = imports.import_layout_table.as_ref() {
            let groups = table.to_jals_groups();
            if !groups.is_empty() {
                config.imports.order = ImportOrder::Group;
                config.imports.groups = groups;
            }
        }
        Lower::set(
            &mut config.imports.static_first,
            imports.layout_static_imports_separately,
            |b| b,
        );

        config
    }
}

/// Importer for the `.editorconfig` (`ij_java_*`) form (portable).
#[derive(Debug, Clone, Copy, Default)]
pub struct IntellijEditorConfig;

impl ConfigImporter for IntellijEditorConfig {
    type Native = IntellijConfig;

    fn parse(src: &str) -> Result<Self::Native, ImportError> {
        let pairs = super::text::EditorConfig::parse(src)
            .into_iter()
            .filter_map(|(key, value)| {
                IntellijConfig::setting_name(&key)
                    .map(|name| (alloc::string::ToString::to_string(name), value))
            })
            .collect();
        IntellijConfig::from_pairs(pairs)
    }
}

/// Importer for the `.idea/codeStyles/Project.xml` / exported scheme form. Needs the XML
/// reader, so it is gated behind the `std` feature.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, Default)]
pub struct IntellijXmlScheme;

#[cfg(feature = "std")]
impl ConfigImporter for IntellijXmlScheme {
    type Native = IntellijConfig;

    fn parse(src: &str) -> Result<Self::Native, ImportError> {
        IntellijConfig::from_pairs(super::xml::IntellijSchemeReader::parse(src)?)
    }
}
