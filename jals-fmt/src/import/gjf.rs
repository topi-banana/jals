//! google-java-format — the whole (deliberately tiny) configuration surface, and the profile
//! the GJF family shares.
//!
//! # Coverage
//!
//! GJF has **no config file**; non-configurability is an explicit design goal. Its entire
//! surface is `JavaFormatterOptions` — `style`, `formatJavadoc`, `reorderModifiers`,
//! `reflowLongStrings` — plus the two `CommandLineOptions` toggles that decide whether the
//! import passes run. [`GoogleJavaFormatConfig`] models all six, so nothing is missing.
//!
//! What is deliberately *not* modeled is `CommandLineOptions`' range selection (`--lines`,
//! `--offset`, `--length`, `--assume-filename`) and its process flags (`--dry-run`,
//! `--set-exit-if-changed`): those pick *what* to format and how to report, not how the output
//! looks, so they have no place in a style config.
//!
//! # The family profile
//!
//! palantir-java-format inherits GJF's token-level passes and canonical conventions verbatim and
//! differs only in its break engine (which no config can express) plus the style-derived indents
//! and column limit. Both `From` impls therefore funnel through [`GoogleJavaFormatConfig::family`].

use alloc::borrow::ToOwned;
use alloc::vec;

use jals_config::fmt::{
    Comments, Config, ImportOrder, Imports, IndentStyle, InlineAnnotations, KeepOnOneLine, Layout,
    ParenPositions, Spacing, WrapPolicy, Wrapping,
};
use serde::Deserialize;

/// The two published google-java-format style variants.
///
/// The only difference is the indent multiplier (`JavaFormatterOptions`: `GOOGLE(1)` /
/// `AOSP(2)`); the 100-column limit is shared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GjfStyle {
    /// Google Java Style: block indent 2, continuation indent 4.
    #[default]
    Google,
    /// The AOSP variant: doubled indents (block 4, continuation 8).
    Aosp,
}

impl GjfStyle {
    /// `(block indent, continuation indent)` in columns.
    const fn indents(self) -> (usize, usize) {
        match self {
            Self::Google => (2, 4),
            Self::Aosp => (4, 8),
        }
    }
}

/// google-java-format's whole configuration surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
// `JavaFormatterOptions` really is mostly booleans; grouping them would misrepresent it.
#[allow(clippy::struct_excessive_bools)]
pub struct GoogleJavaFormatConfig {
    /// `JavaFormatterOptions.style` — the CLI's `--aosp`.
    pub style: GjfStyle,
    /// `JavaFormatterOptions.formatJavadoc` — the CLI's `--skip-javadoc-formatting` inverted.
    pub format_javadoc: bool,
    /// `JavaFormatterOptions.reorderModifiers` — runs `ModifierOrderer`.
    pub reorder_modifiers: bool,
    /// `JavaFormatterOptions.reflowLongStrings` — runs `StringWrapper`. Projects onto
    /// `wrapping.reflow-long-strings`.
    pub reflow_long_strings: bool,
    /// `--skip-sorting-imports` inverted — runs `ImportOrderer`.
    pub sort_imports: bool,
    /// `--skip-removing-unused-imports` inverted — runs `RemoveUnusedImports`. Projects onto
    /// `imports.remove-unused`, whose name test is syntactic — no classpath is consulted, so
    /// it stays inside the portable crate.
    pub remove_unused_imports: bool,
}

impl Default for GoogleJavaFormatConfig {
    fn default() -> Self {
        Self {
            style: GjfStyle::Google,
            format_javadoc: true,
            reorder_modifiers: true,
            reflow_long_strings: true,
            sort_imports: true,
            remove_unused_imports: true,
        }
    }
}

impl From<GoogleJavaFormatConfig> for Config {
    fn from(native: GoogleJavaFormatConfig) -> Self {
        let (indent_width, continuation_indent) = native.style.indents();
        GoogleJavaFormatConfig::family(indent_width, continuation_indent, 100, native)
    }
}

impl GoogleJavaFormatConfig {
    /// The jals config shared by the whole GJF family, parameterized by the style-derived
    /// indents and column limit.
    ///
    /// Each non-default choice is anchored to a documented GJF behavior:
    ///
    /// - one column limit drives every wrap, so no construct gets its own threshold; the
    ///   argument / parameter / binary fills are `if-long`, and a method chain that does not fit
    ///   goes one call per line (`if-long-per-item`);
    /// - `paren-*` are all `common-lines`: GJF never puts a `)` on its own line;
    /// - `tabular-array-initializers`: GJF keeps a grid-shaped initializer's source rows;
    /// - `case-labels` wrap: GJF breaks a long `case` label list;
    /// - comments are always reflowed (Javadoc only when `formatJavadoc`), and both
    ///   GJF-specific comment rewrites are on;
    /// - imports: a static block first, then everything else, one blank line between
    ///   (Google Java Style §3.3.3), and an unused one deleted;
    /// - `reflow-long-strings`: GJF's `StringWrapper` second pass;
    /// - literal rewrites stay `preserve`: GJF never rewrites a literal.
    pub(crate) fn family(
        indent_width: usize,
        continuation_indent: usize,
        max_width: usize,
        native: Self,
    ) -> Config {
        Config {
            layout: Layout {
                indent_style: IndentStyle::Space,
                indent_width,
                tab_width: indent_width,
                continuation_indent: Some(continuation_indent),
                max_width,
                ..Layout::default()
            },
            // GJF collapses an empty body to `{}` and never joins a non-empty one, which is
            // the jals default; every brace is K&R, also the default.
            braces: jals_config::fmt::Braces {
                keep_type_body_on_one_line: KeepOnOneLine::IfEmpty,
                keep_method_body_on_one_line: KeepOnOneLine::IfEmpty,
                // `visitStatement` separates a braceless body from its header with a break whose
                // flat form is a space, so `if (a) return;` stays on one line when it fits.
                keep_control_statement_on_one_line: true,
                ..jals_config::fmt::Braces::default()
            },
            wrapping: Wrapping {
                method_chain: WrapPolicy::IfLongPerItem,
                // `visitConditionalExpression` separates `?` and `:` with `breakOp(" ")`, which
                // is UNIFIED: a ternary that does not fit breaks at both or at neither.
                ternary: WrapPolicy::IfLongPerItem,
                // `visitFormals` separates parameters with `breakOp(" ")`, which is UNIFIED: a
                // parameter list that does not fit goes one parameter per line rather than
                // packing. An *argument* list is the fill, and `fill-item-width` decides it.
                method_parameters: WrapPolicy::IfLongPerItem,
                // `visitCase` separates a rule's labels with a UNIFIED break.
                case_labels: WrapPolicy::IfLongPerItem,
                // `classDeclarationTypeList` and `visitThrowsClause` separate their types with
                // `breakOp(" ")`, which is UNIFIED: a clause that does not fit goes one type per
                // line rather than packing.
                // `visitParameterizedType` separates type arguments with UNIFIED breaks too, and
                // `visitAnnotation` its member-value pairs.
                type_arguments: WrapPolicy::IfLongPerItem,
                type_parameters: WrapPolicy::IfLongPerItem,
                deconstruction_list: WrapPolicy::IfLongPerItem,
                // `visitUnionType` separates a multi-catch's alternatives the same way.
                multi_catch_types: WrapPolicy::IfLongPerItem,
                // `visitForLoop` separates the header's three clauses the same way.
                for_statement: WrapPolicy::IfLongPerItem,
                annotation_arguments: WrapPolicy::IfLongPerItem,
                extends_list: WrapPolicy::IfLongPerItem,
                throws_list: WrapPolicy::IfLongPerItem,
                // `visitEnumDeclaration` forces a break between constants, and `visitTry`
                // between resources.
                enum_constants: WrapPolicy::AlwaysPerItem,
                resource_list: WrapPolicy::AlwaysPerItem,
                paren_method_declaration: ParenPositions::CommonLines,
                paren_method_invocation: ParenPositions::CommonLines,
                paren_control: ParenPositions::CommonLines,
                paren_annotation: ParenPositions::CommonLines,
                paren_lambda: ParenPositions::CommonLines,
                paren_record: ParenPositions::CommonLines,
                tabular_array_initializers: true,
                // `hasOnlyShortItems` / `MAX_ITEM_LENGTH_FOR_FILLING`: an argument list fills
                // only while every argument is under 10 source columns.
                fill_item_width: 10,
                // `isFormatMethod`: a leading format string takes the first continuation line and
                // the values it interpolates pack onto the next.
                format_string_arguments: true,
                // `visitLabeledStatement` forces a break after the label's `:`.
                labeled_statement: WrapPolicy::AlwaysPerItem,
                // `fieldAnnotationDirection`: every *variable* — field, local, parameter,
                // record component, resource, `catch` parameter — puts its annotations on their
                // own lines, unless none of them takes arguments.
                parameter_annotations: WrapPolicy::AlwaysPerItem,
                variable_annotations: WrapPolicy::AlwaysPerItem,
                inline_argumentless_annotations: InlineAnnotations::Declarations,
                reflow_long_strings: native.reflow_long_strings,
                ..Wrapping::default()
            },
            // `new String[] {…}` and `{{1}, {2}}` both take a space before the initializer's
            // brace; every other spacing decision is already the jals default.
            spacing: Spacing {
                before_array_initializer_left_brace: true,
                ..Spacing::default()
            },
            comments: Comments {
                format_line: true,
                // `JavaCommentsHelper.rewrite` sends only a *Javadoc* tok through
                // `JavadocFormatter`. A `/* … */` comment is trimmed of trailing whitespace and
                // re-indented (`indentJavadoc` / `preserveIndentation`) — never refilled. Turning
                // this on would rewrap every license header in the file.
                format_block: false,
                format_javadoc: native.format_javadoc,
                // The file's leading comment gets no special treatment: whatever its kind says.
                format_header: true,
                format_html: true,
                width: max_width,
                blank_line_before_tags: true,
                normalize_parameter_comments: true,
                inline_block_comments: true,
                ..Comments::default()
            },
            imports: Imports {
                order: if native.sort_imports {
                    ImportOrder::Group
                } else {
                    ImportOrder::Preserve
                },
                groups: vec!["static".to_owned(), "*".to_owned()],
                static_first: true,
                reorder_modifiers: native.reorder_modifiers,
                remove_unused: native.remove_unused_imports,
            },
            ..Config::default()
        }
    }
}
