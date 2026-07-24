//! google-java-format: the (deliberately empty) configuration surface and its jals mapping.
//!
//! GJF has **no config file** — non-configurability is an explicit design goal (DESIGN §0,
//! §A.7), so there is nothing to detect on disk and nothing to parse. The whole surface is the
//! style variant chosen on the command line (`--aosp`) or through a build-tool option, which is
//! exactly what [`GoogleJavaFormatConfig`] models: a minimal struct with a single [`GjfStyle`]
//! field. It still derives `Deserialize` so a profile embedding (e.g. a future `[compat.gjf]`
//! table, or a Spotless `googleJavaFormat(...)` lowering) can construct it through serde like
//! every other importer.

use alloc::borrow::ToOwned;
use alloc::vec;

use jals_config::fmt::{AnnotationPlacement, BinopLayout, ClosingParen, Config, FnParamsLayout};
use serde::Deserialize;

/// The two published google-java-format style variants.
///
/// The *only* difference is the indent multiplier (`JavaFormatterOptions`: `GOOGLE(1)` /
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

/// google-java-format's whole configuration surface — a minimal struct, because GJF is
/// deliberately non-configurable ("no configurability as to the formatter's algorithm").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct GoogleJavaFormatConfig {
    /// The selected style variant.
    pub style: GjfStyle,
}

impl From<GoogleJavaFormatConfig> for Config {
    fn from(native: GoogleJavaFormatConfig) -> Self {
        let (indent_width, continuation_indent) = match native.style {
            GjfStyle::Google => (2, 4),
            GjfStyle::Aosp => (4, 8),
        };
        // GJF formats Javadoc by default (`--skip-javadoc-formatting` opts out).
        GoogleJavaFormatConfig::family(indent_width, continuation_indent, 100, true)
    }
}

impl GoogleJavaFormatConfig {
    /// The jals options shared by the whole GJF family — palantir-java-format inherits GJF's
    /// token-level passes and canonical layout conventions verbatim, so both `From` impls funnel
    /// here and differ only in indents, column limit, and Javadoc reflow.
    ///
    /// Non-default choices, each anchored to a documented GJF behavior (the jals option docs name
    /// their google-java-format equivalent explicitly):
    ///
    /// - widths: GJF has a single 100-column driver and no rustfmt-style sub-widths, so every
    ///   width-scoped option is bound to the column limit;
    /// - `binop-layout` `compressed`: GJF's binary-expression fill;
    /// - `closing-paren` `hug`: GJF never dangles a `)`;
    /// - `fn-params-layout` `compressed`: argument/parameter fill (`INDEPENDENT` breaks);
    /// - `tabular-array-initializers`, `switch-expression-on-new-line`, `wrap-case-labels`,
    ///   `normalize-parameter-comments`, `inline-block-comments`: the jals options that exist to
    ///   mirror GJF, switched on;
    /// - `space-around-operator-colon`: GJSG spaces the for-each / ternary colon;
    /// - imports: static block first, then everything else, one blank line between (GJSG §3.3.3);
    /// - `reorder-modifiers`: `ModifierOrderer` runs by default;
    /// - `annotation-placement` `expanded`: declaration annotations each take a line (field
    ///   annotations may share one in GJF — jals's nearest value is still `expanded`);
    /// - literal rewrites stay `preserve`: GJF never rewrites a literal (DESIGN §4).
    pub(crate) fn family(
        indent_width: usize,
        continuation_indent: usize,
        max_width: usize,
        wrap_comments: bool,
    ) -> Config {
        Config {
            indent_width,
            continuation_indent: Some(continuation_indent),
            max_width,
            chain_width: max_width,
            fn_call_width: max_width,
            array_width: max_width,
            single_line_if_else_max_width: max_width,
            comment_width: max_width,
            wrap_comments,
            normalize_parameter_comments: true,
            inline_block_comments: true,
            reorder_imports: true,
            group_imports: true,
            import_groups: vec!["static".to_owned(), "*".to_owned()],
            binop_layout: BinopLayout::Compressed,
            closing_paren: ClosingParen::Hug,
            tabular_array_initializers: true,
            switch_expression_on_new_line: true,
            wrap_case_labels: true,
            space_around_operator_colon: true,
            fn_params_layout: FnParamsLayout::Compressed,
            reorder_modifiers: true,
            annotation_placement: AnnotationPlacement::Expanded,
            ..Config::default()
        }
    }
}
