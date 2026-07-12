//! Formatting configuration, deserialized from `jalsfmt.toml`.
//!
//! Every key is optional; omitted keys fall back to [`Config::default`]. Keys use
//! kebab-case (e.g. `indent-style`, `max-blank-lines`).
//!
//! The option enums are split out for navigability: the layout / style enums live in
//! [`options`], the numeric-literal case enums in [`literals`]. Both are re-exported here, so the
//! whole config surface is reachable as `fmt::*`. The load / parse error is the shared
//! [`ConfigError`](crate::ConfigError).

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use jals_fs::FileTree;
use serde::Deserialize;

mod literals;
mod options;

#[cfg(test)]
mod tests;

pub use crate::loader::ConfigError;
pub use literals::{FloatLiteralTrailingZero, HexLiteralCase, LiteralSuffixCase};
pub use options::{
    AnnotationPlacement, BinopLayout, BinopSeparator, BraceStyle, ClosingParen, ControlBraceStyle,
    FnParamsLayout, IndentStyle, LineEnding, SwitchCaseBody, TrailingComma, TypePunctuationDensity,
};

/// Formatter style settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
#[allow(clippy::struct_excessive_bools)]
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
    /// Preserve a blank line at the very start of a braced body (immediately after `{`, before
    /// the first item), instead of always dropping it. Off by default. Applies to every braced
    /// body (`lower_braced`): type / module bodies, method / constructor / initializer /
    /// control-flow / bare blocks, and `switch` blocks. The kept run is still clamped by
    /// `max-blank-lines`. Layout-only — the significant-token sequence is preserved exactly. Enum
    /// bodies and the blank line before a closing `}` are not affected.
    pub blank_line_at_block_start: bool,
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
    /// Rewrite block comments that look like a parameter-name label before an argument
    /// (`/*a=*/`, `/*xs...=*/`) into google-java-format's canonical spaced form `/* name= */`,
    /// collapsing interior whitespace (`/*  a  =  */` → `/* a= */`). A comment is rewritten
    /// only when its *entire* text matches `/* <java-identifier>(...)? = */`; Javadoc
    /// (`/** … */`), line comments, and any non-matching block comment are left exactly as
    /// written. Off by default. Operates only on comment trivia — the significant-token
    /// sequence is preserved exactly (a comment-whitespace toggle like
    /// [`wrap_comments`](Config::wrap_comments)). Mirrors google-java-format's
    /// `CommentsHelper.reformatParameterComment`.
    pub normalize_parameter_comments: bool,
    /// Keep a block / doc comment that is written immediately before a significant token *on the
    /// same line* hugging that token, instead of relocating it to the end of the line (e.g. the
    /// marker comment in `java.lang./* @A */ String`). By default such a comment is attached as a
    /// trailing comment of the preceding token and emitted as a line suffix, which flushes it past
    /// the rest of the line (`java.lang.String s; /* @A */`). When enabled it stays where it was
    /// written, matching google-java-format. Off by default. Operates only on comment trivia — the
    /// significant-token sequence is preserved exactly (a comment-position toggle like
    /// [`wrap_comments`](Config::wrap_comments)); a line comment runs to end of line, so it is never
    /// followed by a same-line token and is unaffected.
    pub inline_block_comments: bool,
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
    /// How a same-precedence binary-operator run lays out when it wraps:
    /// [`Tall`](BinopLayout::Tall) breaks at every operator (the default), while
    /// [`Compressed`](BinopLayout::Compressed) packs as many operands per line as fit
    /// [`max_width`](Config::max_width) (a *fill*, matching google-java-format). Orthogonal to
    /// [`binop_separator`](Config::binop_separator). Layout-only — the significant-token sequence
    /// is preserved exactly.
    pub binop_layout: BinopLayout,
    /// Let the last item of a call argument list or annotation argument list hang past the
    /// call line when it is a delimited expression — a block-bodied lambda, an
    /// anonymous-class / array-creating `new`, an array initializer, or a `name = {…}`
    /// annotation pair: the earlier arguments stay on the call line and only the trailing
    /// body breaks (`f(a, () -> {` … `});`). Off by default, keeping the all-or-nothing
    /// layout. Layout-only — the significant-token sequence is preserved exactly. Mirrors
    /// rustfmt's `overflow_delimited_expr`.
    pub overflow_delimited_expr: bool,
    /// Where the closing parenthesis of a wrapped paren-delimited list — a call / annotation
    /// argument list, a method / constructor parameter list, or a record header — is placed:
    /// [`OwnLine`](ClosingParen::OwnLine) (the default, dedented onto its own line, mirroring the
    /// array initializer's `}`) or [`Hug`](ClosingParen::Hug) (kept on the last item's line with
    /// no break before it, `f(` … `last);`, matching google-java-format). The brace-delimited
    /// array initializer (`{ … }`) is never affected. Layout-only — the significant-token sequence
    /// is preserved exactly (only the whitespace before the `)` changes); idempotent. A
    /// Java-specific option with no rustfmt equivalent.
    pub closing_paren: ClosingParen,
    /// Preserve the *tabular* (table-shaped) layout of an array initializer: when the source
    /// lays the elements out as a grid — at least two source rows, every row but the last with
    /// the same number of elements (the last with that many or fewer), and no interior
    /// comments — keep those source row breaks instead of reflowing against
    /// [`array_width`](Config::array_width) / [`max_width`](Config::max_width). Each source row
    /// goes on its own (block-indented) line, elements within a row separated by a single space.
    /// Any other multi-line array (an irregular row shape, or a single row) is unaffected and
    /// still wraps by width. Off by default. Layout-only — the significant-token sequence is
    /// preserved exactly (only inter-element whitespace changes); idempotent. A Java-specific
    /// option with no rustfmt equivalent; mirrors google-java-format's preservation of tabular
    /// array initializers (its `TabularMixedSignInitializer` behavior).
    pub tabular_array_initializers: bool,
    /// Put a `switch` *expression* that is the right-hand side of a `=` — a variable or field
    /// initializer (`int x = switch (v) {…}`) or an assignment (`x = switch (v) {…}`) — on its
    /// own continuation-indented line, breaking right after the `=`. Off by default, keeping the
    /// switch on the `=` line. Only the `=` assignment operator is affected; a `return switch …`
    /// (which has no `=`) stays inline, and a `switch` *statement* is never touched. Layout-only —
    /// the significant-token sequence is preserved exactly (only the whitespace after the `=`
    /// changes); idempotent. A Java-specific option with no rustfmt equivalent; mirrors
    /// google-java-format's layout of an assignment whose value is a switch expression.
    pub switch_expression_on_new_line: bool,
    /// How a *legacy* (colon-form) `switch` group — one or more `case X:` / `default:` labels
    /// followed by statements — lays out its body relative to the label's colon:
    /// [`Always`](SwitchCaseBody::Always) (the default; each label on its own line, every body
    /// statement broken onto its own line and indented one level, matching google-java-format),
    /// [`SingleLine`](SwitchCaseBody::SingleLine) (keep a single-label, single-statement,
    /// comment-free body inline on the colon line, break the rest), or
    /// [`SameLine`](SwitchCaseBody::SameLine) (keep the whole group inline). The arrow form
    /// (`case X -> …`) is never affected. Layout-only — the significant-token sequence is
    /// preserved exactly (only whitespace after the colon changes); idempotent. A Java-specific
    /// option with no rustfmt equivalent.
    pub switch_case_body: SwitchCaseBody,
    /// Wrap a `switch` `case` label's constant list — the comma-separated `case` constants — across
    /// multiple lines when the arm overflows [`max_width`](Config::max_width): the first constant
    /// stays on the `case` line, each subsequent constant hangs at one continuation indent, the
    /// comma stays attached to its constant (the break falls after the comma), and the `->` / `:`
    /// plus body ride on the last constant's line. A short list that fits stays on one line
    /// (all-or-nothing). A single constant and a bare `default` label are never affected. Applies to
    /// both the arrow form (`case A, B -> …`) and the legacy colon form (`case A, B: …`). Off by
    /// default. Layout-only — the significant-token sequence is preserved exactly (only inter-token
    /// whitespace changes); idempotent. A Java-specific option with no rustfmt equivalent; mirrors
    /// google-java-format's wrapping of a long `case` label list (its `ExpressionSwitch` behavior).
    pub wrap_case_labels: bool,
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
    /// Whether to emit a space *before* the colon of an *operator colon* — a `:` that separates
    /// two operands: an enhanced `for` (`for (T x : xs)`), a ternary (`a ? b : c`), and an
    /// `assert` message (`assert c : m`). Additive over
    /// [`space_before_colon`](Config::space_before_colon): the space is emitted when either option
    /// is on. The *label* colons (a labeled statement `label:` and a `switch` `case x:` /
    /// `default:`) are never affected by this option and keep following `space_before_colon`
    /// alone. Off by default. One exception keeps Google Java Format fidelity: an unnamed `_`
    /// for-each variable hugs its colon (`for (T _: xs)`), so no space is inserted there even with
    /// this option on. Layout-only — the significant-token sequence is preserved exactly.
    pub space_around_operator_colon: bool,
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
        Self {
            indent_style: IndentStyle::Space,
            indent_width: 4,
            continuation_indent: None,
            max_blank_lines: 1,
            blank_line_at_block_start: false,
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
            normalize_parameter_comments: false,
            inline_block_comments: false,
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
            binop_layout: BinopLayout::Tall,
            overflow_delimited_expr: false,
            closing_paren: ClosingParen::OwnLine,
            tabular_array_initializers: false,
            switch_expression_on_new_line: false,
            switch_case_body: SwitchCaseBody::Always,
            wrap_case_labels: false,
            space_before_colon: false,
            space_after_colon: true,
            space_around_operator_colon: false,
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
    ///
    /// A rendering helper for the formatter (`jals-fmt`); it is not a config key.
    pub fn indent_unit(&self) -> String {
        match self.indent_style {
            IndentStyle::Tab => "\t".to_string(),
            IndentStyle::Space => " ".repeat(self.indent_width),
        }
    }

    /// The number of display columns one indentation level occupies.
    ///
    /// A rendering helper for the formatter (`jals-fmt`); it is not a config key.
    pub fn indent_cols(&self) -> usize {
        self.indent_width.max(1)
    }

    /// The number of display columns one *continuation* indent occupies — the indent applied to
    /// the wrapped lines of an expression / statement (method chains, wrapped binary / ternary
    /// operators, and delimited lists). Falls back to `indent-width` when
    /// [`continuation_indent`](Config::continuation_indent) is unset, so default output is
    /// unchanged. In tab style it equals one indentation level (`indent_cols()`), keeping the
    /// emitted indentation a whole number of tabs.
    ///
    /// A rendering helper for the formatter (`jals-fmt`); it is not a config key.
    pub fn continuation_cols(&self) -> usize {
        match self.indent_style {
            IndentStyle::Tab => self.indent_cols(),
            IndentStyle::Space => self.continuation_indent.unwrap_or(self.indent_width).max(1),
        }
    }

    /// The resolved line terminator for input `src`, honoring `Auto`/`Native`.
    ///
    /// A rendering helper for the formatter (`jals-fmt`); it is not a config key.
    pub fn newline(&self, src: &str) -> &'static str {
        self.line_ending.resolve(src)
    }

    /// Load and parse the `jalsfmt.toml` at `path`, read through `fs`.
    ///
    /// `fs` is any [`FileTree`] — a [`jals_fs::OsFileTree`] on the host, or a
    /// [`jals_fs::InMemoryFileTree`] for wasm / tests; `path` is a `/`-separated virtual path.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the file cannot be read or contains invalid TOML.
    pub fn from_file(fs: &dyn FileTree, path: &str) -> Result<Self, ConfigError> {
        <Self as crate::DiscoverableConfig>::load(fs, path)
    }

    /// Search upward from `start_dir` (a `/`-separated virtual path) for `jalsfmt.toml`, read
    /// through `fs`.
    ///
    /// Returns the parsed config if a file is found, otherwise [`Config::default`].
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a discovered file cannot be read or parsed.
    pub fn discover(fs: &dyn FileTree, start_dir: &str) -> Result<Self, ConfigError> {
        <Self as crate::DiscoverableConfig>::discover(fs, start_dir)
    }
}

impl crate::DiscoverableConfig for Config {
    const FILE_NAME: &'static str = "jalsfmt.toml";
}
