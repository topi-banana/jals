//! Formatting configuration, deserialized from `jalsfmt.toml`.
//!
//! Every key is optional; omitted keys fall back to [`Config::default`]. Keys use
//! kebab-case (e.g. `indent-style`, `max-blank-lines`).
//!
//! The option enums are split out for navigability: the layout / style enums live in
//! [`options`], the numeric-literal case enums in [`literals`], and the load / parse error
//! in [`error`]. All are re-exported here, so the whole config surface is reachable as
//! `config::*`.

use std::path::Path;

use serde::Deserialize;

mod error;
mod literals;
mod options;

#[cfg(test)]
mod tests;

pub use error::ConfigError;
pub use literals::{FloatLiteralTrailingZero, HexLiteralCase, LiteralSuffixCase};
pub use options::{
    AnnotationPlacement, BinopSeparator, BraceStyle, ControlBraceStyle, FnParamsLayout,
    IndentStyle, LineEnding, TrailingComma, TypePunctuationDensity,
};

/// Formatter style settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// Spaces vs. tab for indentation.
    pub indent_style: IndentStyle,
    /// Number of columns per indentation level (and spaces emitted when `indent_style` is `Space`).
    pub indent_width: usize,
    /// Columns to indent a *continuation* line — the extra lines produced when an expression or
    /// statement wraps (method chains, wrapped binary / ternary operators, and delimited lists:
    /// parameter / argument / array-initializer / annotation-arg / record-header). `None` (the
    /// default) falls back to [`indent_width`](Config::indent_width), so default output is
    /// unchanged. Block bodies (`{ … }`) always use `indent-width`. In tab style this is ignored
    /// (each continuation is one tab), keeping the output a whole number of tabs. Layout-only —
    /// the significant-token sequence is preserved exactly.
    pub continuation_indent: Option<usize>,
    /// Runs of blank lines are collapsed down to at most this many.
    pub max_blank_lines: usize,
    /// Line terminator to emit.
    pub line_ending: LineEnding,
    /// Ensure the output ends with exactly one newline.
    pub insert_final_newline: bool,
    /// Target line width for wrapping code.
    pub max_width: usize,
    /// Maximum width of a method chain (`a.b().c().d()`) kept on a single line. A chain with
    /// at least two calls whose flat width exceeds this is laid out one call per line (it also
    /// wraps when it would overflow [`max_width`](Config::max_width)). Mirrors rustfmt's
    /// `chain_width`.
    pub chain_width: usize,
    /// Maximum width of a function or method call's argument list (`f(a, b, c)`) kept on a
    /// single line. A call whose argument list's flat width exceeds this is laid out one
    /// argument per line (it also wraps when it would overflow [`max_width`](Config::max_width)).
    /// Mirrors rustfmt's `fn_call_width`.
    pub fn_call_width: usize,
    /// Maximum width of an array initializer (`{a, b, c}`) kept on a single line. An
    /// initializer whose flat width exceeds this is laid out one element per line (it also
    /// wraps when it would overflow [`max_width`](Config::max_width)). Mirrors rustfmt's
    /// `array_width`.
    pub array_width: usize,
    /// Maximum flat width of a ternary conditional (`a ? b : c`) kept on a single line. A
    /// ternary whose flat width exceeds this — or that would overflow
    /// [`max_width`](Config::max_width) — wraps, the `?` and `:` placed per
    /// [`binop_separator`](Config::binop_separator) (leading the continuation line under
    /// `front`, trailing the broken line under `back`). A value of `0` wraps every ternary.
    /// Layout-only — the significant-token sequence is preserved exactly. Mirrors rustfmt's
    /// `single_line_if_else_max_width` (whose Rust if-else expression maps to Java's ternary).
    pub single_line_if_else_max_width: usize,
    /// Placement of the opening brace of a declaration body (type, method, constructor, or
    /// initializer): same line (K&R) or next line (Allman). Control-flow blocks
    /// (`if`/`for`/`while`/`try`/…), `switch`, and lambda bodies are governed separately by
    /// [`control_brace_style`](Config::control_brace_style).
    pub brace_style: BraceStyle,
    /// Collapse an empty declaration body — a type body (`class` / `interface` / `@interface` /
    /// record) or the block of a method, constructor, or initializer — to `{}` on the header's
    /// line. On by default. When off, such a body expands to a two-line `{` … `}` (opening on
    /// its own line under [`brace_style`](Config::brace_style) `next-line`). Control-flow /
    /// `switch` / lambda / bare blocks are never affected and always keep `{}`; `enum` bodies
    /// (not block-formatted yet) are likewise unaffected. Layout-only — the significant-token
    /// sequence is preserved exactly. Mirrors rustfmt's `empty_item_single_line`.
    pub empty_item_single_line: bool,
    /// Keep a declaration body — the block of a method, constructor, or initializer — on the
    /// header's line when it holds exactly one statement and no comments, e.g.
    /// `int foo() { return 1; }`. Off by default. When on, such a body collapses onto one line
    /// if it fits [`max_width`](Config::max_width); a body with two or more statements, a
    /// comment, a nested block, or one that would overflow `max-width` stays multi-line. The
    /// one-liner is emitted regardless of [`brace_style`](Config::brace_style) (like
    /// [`empty_item_single_line`](Config::empty_item_single_line)); only when it does not fit
    /// does the brace open on its own line under `next-line`. Layout-only — the
    /// significant-token sequence is preserved exactly. Mirrors rustfmt's `fn_single_line`.
    pub fn_single_line: bool,
    /// Force every block to be laid out multi-line. When on, an empty block — of any kind
    /// (a type body, a method / constructor / initializer block, a control-flow / `switch` /
    /// lambda / bare block) — expands to a two-line `{` … `}` instead of collapsing to `{}`
    /// (overriding [`empty_item_single_line`](Config::empty_item_single_line) and extending past
    /// its declaration-only scope), and a single-statement declaration body is never collapsed
    /// onto the header's line (overriding [`fn_single_line`](Config::fn_single_line)). The opening
    /// brace still follows [`brace_style`](Config::brace_style) /
    /// [`control_brace_style`](Config::control_brace_style). Off by default. Layout-only — the
    /// significant-token sequence is preserved exactly. Reinterprets rustfmt's
    /// `force_multiline_blocks` (whose literal closure / match-arm brace-wrapping would add tokens
    /// and is not portable under jals's invariants).
    pub force_multiline_blocks: bool,
    /// Layout of control-flow brace styling: the opening brace of a control-flow / `switch` /
    /// lambda / bare block, and the `} else` / `} catch` / `} finally` / `} while`
    /// continuations. Same line (K&R) or next line (Allman).
    pub control_brace_style: ControlBraceStyle,
    /// Reflow comments so no line exceeds [`comment_width`](Config::comment_width).
    /// Off by default; [`comment_width`](Config::comment_width) has no effect unless this
    /// is enabled (mirrors rustfmt's `wrap_comments`).
    pub wrap_comments: bool,
    /// Target line width for reflowing comment / Javadoc prose, including indentation.
    /// Only consulted when [`wrap_comments`](Config::wrap_comments) is enabled.
    pub comment_width: usize,
    /// Sort `import` declarations: non-static imports first (alphabetical by qualified name),
    /// then static imports (alphabetical). Off by default; opt-in like
    /// [`wrap_comments`](Config::wrap_comments). When enabled the formatter's significant-token
    /// *sequence* may change (the multiset is preserved); mirrors rustfmt's `reorder_imports`.
    pub reorder_imports: bool,
    /// How to treat the trailing comma of an array initializer (`{1, 2, 3,}`). Defaults to
    /// [`Preserve`](TrailingComma::Preserve), which keeps the source comma exactly; the other
    /// modes may add or drop that single comma, weakening the strict significant-token invariant
    /// (a comma carrying a comment is never dropped). Mirrors rustfmt's `trailing_comma`.
    pub trailing_comma: TrailingComma,
    /// Group `import` declarations into prefix-defined blocks separated by a blank line, each
    /// block sorted alphabetically by qualified name. The groups and their order come from
    /// [`import_groups`](Config::import_groups). Off by default; opt-in like
    /// [`reorder_imports`](Config::reorder_imports), preserving the significant-token *multiset*
    /// while the *sequence* may change. Implies and overrides
    /// [`reorder_imports`](Config::reorder_imports) — grouping sorts within each block. Mirrors
    /// rustfmt's `group_imports`.
    pub group_imports: bool,
    /// The ordered import groups consulted when [`group_imports`](Config::group_imports) is on:
    /// a list of name prefixes. A non-static import joins the group of its *longest* matching
    /// prefix (ties broken by list order); `"*"` is the catch-all for non-static imports matching
    /// no prefix, and `"static"` is the group of every static import (regardless of its name).
    /// A missing `"*"` / `"static"` becomes an implicit trailing group (catch-all, then static).
    /// Consulted only when `group_imports` is enabled.
    pub import_groups: Vec<String>,
    /// Placement of a binary operator when a binary expression wraps across lines: at the
    /// start of the continuation line ([`Front`](BinopSeparator::Front), the default) or at
    /// the end of the broken line ([`Back`](BinopSeparator::Back)). The wrapping itself is
    /// driven by [`max_width`](Config::max_width) alone. Mirrors rustfmt's `binop_separator`.
    pub binop_separator: BinopSeparator,
    /// Let the last item of a call argument list or annotation argument list hang past the
    /// call line when it is a delimited expression — a block-bodied lambda, an
    /// anonymous-class / array-creating `new`, an array initializer, or a `name = {…}`
    /// annotation pair: the earlier arguments stay on the call line and only the trailing
    /// body breaks (`f(a, () -> {` … `});`). Off by default, keeping the all-or-nothing
    /// layout. Layout-only — the significant-token sequence is preserved exactly. Mirrors
    /// rustfmt's `overflow_delimited_expr`.
    pub overflow_delimited_expr: bool,
    /// Whether to emit a space *before* a colon (`:`). Applies uniformly to every Java colon
    /// context: a ternary (`a ? b : c`), an enhanced `for` (`for (T x : xs)`), a labeled
    /// statement (`label:`), an `assert` message (`assert c : m`), and a `switch` `case` /
    /// `default` label (`case x:`). Off by default (no space before), matching idiomatic
    /// label / `case` style. The `::` method-reference token is a distinct token and is never
    /// affected. Layout-only — the significant-token sequence is preserved exactly. Mirrors
    /// rustfmt's `space_before_colon`.
    pub space_before_colon: bool,
    /// Whether to emit a space *after* a colon (`:`), in the same contexts as
    /// [`space_before_colon`](Config::space_before_colon). On by default. The `::`
    /// method-reference token is never affected. Layout-only. Mirrors rustfmt's
    /// `space_after_colon`.
    pub space_after_colon: bool,
    /// Layout of a method / constructor parameter list (`PARAM_LIST`):
    /// [`Tall`](FnParamsLayout::Tall) (the default, all-or-nothing),
    /// [`Compressed`](FnParamsLayout::Compressed) (pack as many per line as fit), or
    /// [`Vertical`](FnParamsLayout::Vertical) (always one per line). Applies only to
    /// declaration parameter lists, never to call argument lists. Layout-only — the
    /// significant-token sequence is preserved exactly. Mirrors rustfmt's `fn_params_layout`;
    /// the deprecated key `fn-args-layout` is accepted as an alias.
    #[serde(alias = "fn-args-layout")]
    pub fn_params_layout: FnParamsLayout,
    /// Density of spacing around the `&` of a Java intersection type — a type-parameter bound
    /// (`<T extends A & B>`) or a cast intersection (`(A & B) x`): [`Wide`](TypePunctuationDensity::Wide)
    /// (the default, `A & B`) or [`Compressed`](TypePunctuationDensity::Compressed) (`A&B`). The
    /// bitwise-AND operator `&` (an expression) is never affected. Layout-only — the
    /// significant-token sequence is preserved exactly. Mirrors rustfmt's `type_punctuation_density`.
    pub type_punctuation_density: TypePunctuationDensity,
    /// Reorder the keyword modifiers of every declaration (`public`, `static`, `final`, …) into
    /// a fixed canonical order (JLS / Checkstyle: public, protected, private, abstract, default,
    /// static, sealed, non-sealed, final, transient, volatile, synchronized, native, strictfp),
    /// hoisting all annotations to the front (keeping their relative order). Off by default;
    /// opt-in like [`reorder_imports`](Config::reorder_imports). When enabled the formatter's
    /// significant-token *sequence* may change (the multiset is preserved, and each comment stays
    /// glued to its modifier). A Java-specific option with no rustfmt equivalent.
    pub reorder_modifiers: bool,
    /// Placement of a declaration's leading annotations (the `MODIFIERS` node of a type / method
    /// / constructor / field / initializer / local-variable declaration):
    /// [`Compact`](AnnotationPlacement::Compact) (the default, inline `@Override public void m()`)
    /// or [`Expanded`](AnnotationPlacement::Expanded) (each annotation on its own line above the
    /// declaration). Parameter annotations (a `PARAM`'s own `MODIFIERS`) and type-use /
    /// enum-constant / type-parameter annotations are never affected — they always stay inline.
    /// Layout-only — the significant-token sequence is preserved exactly. A Java-specific option
    /// with no rustfmt equivalent.
    pub annotation_placement: AnnotationPlacement,
    /// Case of the hexadecimal digit letters of an integer / floating-point literal (`0xFF` vs.
    /// `0xff`). Defaults to [`Preserve`](HexLiteralCase::Preserve), which keeps the source case
    /// exactly; the other modes rewrite the case of the hex *mantissa* digits, weakening the
    /// strict significant-token invariant (a literal token's text — but never its kind — may
    /// change). The `0x` prefix, `p` exponent, and any `l` / `f` / `d` suffix are left untouched.
    /// Mirrors rustfmt's `hex_literal_case`.
    pub hex_literal_case: HexLiteralCase,
    /// Whether a decimal float literal carries a trailing zero (`1.0` vs. `1.`). Defaults to
    /// [`Preserve`](FloatLiteralTrailingZero::Preserve), which keeps the source exactly;
    /// [`Always`](FloatLiteralTrailingZero::Always) adds the zero and
    /// [`Never`](FloatLiteralTrailingZero::Never) strips an all-zero fraction, both weakening the
    /// strict significant-token invariant (a literal token's text — but never its kind — may
    /// change). Only in-scope decimal floats are touched; the value, suffix, and exponent are
    /// preserved. Mirrors rustfmt's `float_literal_trailing_zero`.
    pub float_literal_trailing_zero: FloatLiteralTrailingZero,
    /// Case of a numeric literal's trailing type suffix (`123l` vs. `123L`, `1.5f` vs. `1.5F`).
    /// Defaults to [`Preserve`](LiteralSuffixCase::Preserve), which keeps the source exactly;
    /// [`Upper`](LiteralSuffixCase::Upper) / [`Lower`](LiteralSuffixCase::Lower) force the case of
    /// the single suffix letter (the `l` / `L` integer suffix or the `f` / `F` / `d` / `D` float
    /// suffix), weakening the strict significant-token invariant (a literal token's text — but
    /// never its kind — may change). The value, radix prefix, mantissa, and exponent are left
    /// untouched. A Java-specific option with no rustfmt equivalent.
    pub literal_suffix_case: LiteralSuffixCase,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            indent_style: IndentStyle::Space,
            indent_width: 4,
            continuation_indent: None,
            max_blank_lines: 1,
            line_ending: LineEnding::Lf,
            insert_final_newline: true,
            max_width: 100,
            chain_width: 60,
            fn_call_width: 60,
            array_width: 60,
            single_line_if_else_max_width: 50,
            brace_style: BraceStyle::SameLine,
            empty_item_single_line: true,
            fn_single_line: false,
            force_multiline_blocks: false,
            control_brace_style: ControlBraceStyle::SameLine,
            wrap_comments: false,
            comment_width: 80,
            reorder_imports: false,
            trailing_comma: TrailingComma::Preserve,
            group_imports: false,
            import_groups: vec![
                "java.".to_string(),
                "javax.".to_string(),
                "*".to_string(),
                "static".to_string(),
            ],
            binop_separator: BinopSeparator::Front,
            overflow_delimited_expr: false,
            space_before_colon: false,
            space_after_colon: true,
            fn_params_layout: FnParamsLayout::Tall,
            type_punctuation_density: TypePunctuationDensity::Wide,
            reorder_modifiers: false,
            annotation_placement: AnnotationPlacement::Compact,
            hex_literal_case: HexLiteralCase::Preserve,
            float_literal_trailing_zero: FloatLiteralTrailingZero::Preserve,
            literal_suffix_case: LiteralSuffixCase::Preserve,
        }
    }
}

impl Config {
    /// One indentation level rendered as a string.
    pub(crate) fn indent_unit(&self) -> String {
        match self.indent_style {
            IndentStyle::Tab => "\t".to_string(),
            IndentStyle::Space => " ".repeat(self.indent_width),
        }
    }

    /// The number of display columns one indentation level occupies.
    pub(crate) fn indent_cols(&self) -> usize {
        self.indent_width.max(1)
    }

    /// The number of display columns one *continuation* indent occupies — the indent applied to
    /// the wrapped lines of an expression / statement (method chains, wrapped binary / ternary
    /// operators, and delimited lists). Falls back to `indent-width` when
    /// [`continuation_indent`](Config::continuation_indent) is unset, so default output is
    /// unchanged. In tab style it equals one indentation level (`indent_cols()`), keeping the
    /// emitted indentation a whole number of tabs.
    pub(crate) fn continuation_cols(&self) -> usize {
        match self.indent_style {
            IndentStyle::Tab => self.indent_cols(),
            IndentStyle::Space => self.continuation_indent.unwrap_or(self.indent_width).max(1),
        }
    }

    /// The resolved line terminator for input `src`, honoring `Auto`/`Native`.
    pub(crate) fn newline(&self, src: &str) -> &'static str {
        self.line_ending.resolve(src)
    }

    /// Load and parse a specific `jalsfmt.toml` file.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the file cannot be read or contains invalid TOML.
    pub fn from_file(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Search upward from `start_dir` for `jalsfmt.toml`.
    ///
    /// Returns the parsed config if a file is found, otherwise [`Config::default`].
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a discovered file cannot be read or parsed.
    pub fn discover(start_dir: &Path) -> Result<Config, ConfigError> {
        let mut dir = Some(start_dir);
        while let Some(d) = dir {
            let candidate = d.join("jalsfmt.toml");
            if candidate.is_file() {
                return Config::from_file(&candidate);
            }
            dir = d.parent();
        }
        Ok(Config::default())
    }
}
