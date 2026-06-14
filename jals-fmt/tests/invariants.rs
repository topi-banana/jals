//! Property tests for the formatter's correctness invariants.

use jals_fmt::{
    BinopSeparator, Config, FnParamsLayout, TrailingComma, TypePunctuationDensity, format_source,
};
use jals_syntax::{SyntaxKind, parse};
use proptest::prelude::*;

fn fmt(src: &str) -> String {
    format_source(src, &Config::default()).formatted
}

fn fmt_with(src: &str, config: &Config) -> String {
    format_source(src, config).formatted
}

/// Config with comment reflow on at a narrow width, so the property tests exercise wrapping.
fn wrap_config() -> Config {
    Config {
        wrap_comments: true,
        comment_width: 12,
        ..Config::default()
    }
}

/// The prose "skeleton" of all comments: every character that is not whitespace and not a
/// comment marker (`/` or `*`), in document order. Reflow only changes whitespace and
/// markers and never splits or reorders a word, so this sequence is invariant under it.
fn comment_skeleton(src: &str) -> String {
    parse(src)
        .syntax()
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| {
            matches!(
                t.kind(),
                SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT
            )
        })
        .flat_map(|t| t.text().chars().collect::<Vec<_>>())
        .filter(|c| !c.is_whitespace() && *c != '/' && *c != '*')
        .collect()
}

/// The sequence of non-trivia tokens (kind + text) of `src`.
fn sig_tokens(src: &str) -> Vec<(SyntaxKind, String)> {
    parse(src)
        .syntax()
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| (t.kind(), t.text().to_string()))
        .collect()
}

/// The comment contents of `src`, with interior whitespace normalized.
fn comment_contents(src: &str) -> Vec<String> {
    parse(src)
        .syntax()
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| {
            matches!(
                t.kind(),
                SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT
            )
        })
        .map(|t| t.text().split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// A generator of Java-ish source: random concatenations of real tokens, whitespace, and
/// comments. Mirrors the lossless generator in `jals-syntax`.
fn javaish() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("class"),
            Just("interface"),
            Just("void"),
            Just("int"),
            Just("public"),
            Just("static"),
            Just("final"),
            Just("return"),
            Just("if"),
            Just("else"),
            Just("for"),
            Just("while"),
            Just("new"),
            Just("this"),
            Just("{"),
            Just("}"),
            Just("("),
            Just(")"),
            Just("["),
            Just("]"),
            Just(";"),
            Just(","),
            Just("."),
            Just("="),
            Just("=="),
            Just("+"),
            Just("-"),
            Just("*"),
            Just("<"),
            Just(">"),
            Just(">>"),
            Just(">="),
            Just("&&"),
            Just("||"),
            Just("%"),
            Just("instanceof"),
            Just("->"),
            Just("::"),
            Just("@"),
            Just("x"),
            Just("Foo"),
            Just("y"),
            Just("0"),
            Just("1"),
            Just("\"s\""),
            Just("// c\n"),
            Just("/* b */"),
            // Multi-word comments so reflow (`wrap-comments`) actually wraps.
            Just("// alpha beta gamma delta epsilon\n"),
            Just("/** one two three four five six seven */"),
            Just("/*\n * alpha beta gamma\n * delta epsilon\n */"),
            Just("\n"),
            Just(" "),
            Just("\t"),
        ],
        0..40,
    )
    .prop_map(|parts| parts.concat())
}

/// Config with import reordering on.
fn reorder_config() -> Config {
    Config {
        reorder_imports: true,
        ..Config::default()
    }
}

/// Config with import grouping on (default `import-groups`).
fn group_config() -> Config {
    Config {
        group_imports: true,
        ..Config::default()
    }
}

/// Config with modifier reordering on.
fn reorder_mods_config() -> Config {
    Config {
        reorder_modifiers: true,
        ..Config::default()
    }
}

/// The canonical rank of a keyword modifier, or `None` for an annotation (or any non-modifier
/// element). Mirrors the formatter's internal `modifiers::rank_of` so a canonical layout can be
/// asserted on formatted output.
fn modifier_rank(kind: SyntaxKind) -> Option<usize> {
    Some(match kind {
        SyntaxKind::PUBLIC_KW => 0,
        SyntaxKind::PROTECTED_KW => 1,
        SyntaxKind::PRIVATE_KW => 2,
        SyntaxKind::ABSTRACT_KW => 3,
        SyntaxKind::DEFAULT_KW => 4,
        SyntaxKind::STATIC_KW => 5,
        SyntaxKind::SEALED_KW => 6,
        SyntaxKind::NON_SEALED_KW => 7,
        SyntaxKind::FINAL_KW => 8,
        SyntaxKind::TRANSIENT_KW => 9,
        SyntaxKind::VOLATILE_KW => 10,
        SyntaxKind::SYNCHRONIZED_KW => 11,
        SyntaxKind::NATIVE_KW => 12,
        SyntaxKind::STRICTFP_KW => 13,
        _ => return None,
    })
}

/// Config with a given trailing-comma policy and a narrow `array-width`, so array initializers
/// are pushed into the vertical (broken) layout where `vertical` adds its comma.
fn trailing_config(trailing_comma: TrailingComma) -> Config {
    Config {
        trailing_comma,
        array_width: 8,
        ..Config::default()
    }
}

/// Config with a given binop separator and a narrow `max-width`, so binary expressions are
/// pushed into the wrapped layout.
fn binop_config(binop_separator: BinopSeparator) -> Config {
    Config {
        binop_separator,
        max_width: 24,
        ..Config::default()
    }
}

/// Config with a given intersection-type `&` density.
fn type_punct_config(type_punctuation_density: TypePunctuationDensity) -> Config {
    Config {
        type_punctuation_density,
        ..Config::default()
    }
}

/// Config with a given `empty-item-single-line` setting.
fn empty_single_line_config(empty_item_single_line: bool) -> Config {
    Config {
        empty_item_single_line,
        ..Config::default()
    }
}

/// Config with a given `fn-single-line` setting.
fn fn_single_line_config(fn_single_line: bool) -> Config {
    Config {
        fn_single_line,
        ..Config::default()
    }
}

/// Config with `overflow-delimited-expr` on and narrow widths, so the overflow layout and
/// each of its fallbacks are exercised by the generator's lambdas, `new`s, and braces.
fn overflow_config() -> Config {
    Config {
        overflow_delimited_expr: true,
        max_width: 24,
        fn_call_width: 16,
        array_width: 8,
        ..Config::default()
    }
}

/// Config with a given parameter layout and a narrow `max-width`, so parameter lists are pushed
/// into the wrapped (`Tall`) / packed (`Compressed`) / vertical layout the option selects.
fn params_config(layout: FnParamsLayout) -> Config {
    Config {
        fn_params_layout: layout,
        max_width: 24,
        ..Config::default()
    }
}

/// A generator over the three parameter layouts.
fn params_layout() -> impl Strategy<Value = FnParamsLayout> {
    prop_oneof![
        Just(FnParamsLayout::Tall),
        Just(FnParamsLayout::Compressed),
        Just(FnParamsLayout::Vertical),
    ]
}

/// The non-trivia tokens of `src` with every `,` removed. The trailing-comma modes may add or
/// drop array-initializer commas, but must leave every other token (and their order) intact.
fn sig_tokens_without_commas(src: &str) -> Vec<(SyntaxKind, String)> {
    sig_tokens(src)
        .into_iter()
        .filter(|(k, _)| *k != SyntaxKind::COMMA)
        .collect()
}

/// A compilation unit with a random, possibly-unsorted import block: a package decl, then
/// random imports (static / non-static / wildcard, some with leading or trailing comments or
/// trailing blank lines), then a trivial class. Newline-joined so a trailing `//` comment
/// never swallows the next import.
fn java_with_imports() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("import a.b.C;"),
            Just("import a.b.A;"),
            Just("import a.b.*;"),
            Just("import x.Y;"),
            Just("import static a.b.Z.z;"),
            Just("import static a.A.a;"),
            Just("// lead\nimport a.b.D;"),
            Just("import a.b.E; // trail"),
            Just("import a.b.F;\n"),
            Just("import java.util.List;"),
            Just("import javax.swing.JButton;"),
        ],
        0..8,
    )
    .prop_map(|imps| format!("package p.q;\n{}\nclass C {{}}\n", imps.join("\n")))
}

/// The `(is_static, dotted-name)` key of each import in `src`, in document order. Mirrors the
/// formatter's internal `import_sort_key` so a sorted output can be asserted.
fn import_keys(src: &str) -> Vec<(bool, String)> {
    use jals_syntax::ast::{AstNode, SourceFile};
    let file = SourceFile::cast(parse(src).syntax()).expect("parse yields a source file");
    file.imports()
        .map(|imp| {
            let name = imp.name().map(|n| n.text()).unwrap_or_default();
            (imp.is_static(), name)
        })
        .collect()
}

/// The group rank of an import under the default `import-groups` (`["java.", "javax.", "*",
/// "static"]`). Mirrors the formatter's `import_group_rank` for those groups: java. = 0,
/// javax. = 1, catch-all = 2, static = 3.
fn default_group_rank(is_static: bool, name: &str) -> usize {
    if is_static {
        3
    } else if name.starts_with("java.") {
        0
    } else if name.starts_with("javax.") {
        1
    } else {
        2
    }
}

/// The `(group_rank, dotted-name)` key of each import in `src`, in document order — the ordering
/// the formatter's `group-imports` should produce under the default `import-groups`.
fn import_group_keys(src: &str) -> Vec<(usize, String)> {
    use jals_syntax::ast::{AstNode, SourceFile};
    let file = SourceFile::cast(parse(src).syntax()).expect("parse yields a source file");
    file.imports()
        .map(|imp| {
            let name = imp.name().map(|n| n.text()).unwrap_or_default();
            (default_group_rank(imp.is_static(), &name), name)
        })
        .collect()
}

/// A class whose member carries a random, possibly-unsorted run of modifiers and annotations,
/// so `reorder-modifiers` has multi-modifier `MODIFIERS` nodes to reorder. Semantically these
/// combinations need not be legal Java — the parser is error-resilient and still builds the
/// `MODIFIERS` node, which is all the invariants exercise.
fn java_with_modifiers() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("public"),
            Just("protected"),
            Just("private"),
            Just("abstract"),
            Just("static"),
            Just("final"),
            Just("synchronized"),
            Just("volatile"),
            Just("transient"),
            Just("@A"),
            Just("@B(1)"),
            Just("/* c */ public"),
            Just("// lead\nstatic"),
        ],
        0..7,
    )
    .prop_map(|mods| format!("class C {{\n{} int x = 0;\n}}\n", mods.join(" ")))
}

proptest! {
    /// Formatting is idempotent.
    #[test]
    fn idempotent(src in javaish()) {
        let once = fmt(&src);
        let twice = fmt(&once);
        prop_assert_eq!(once, twice);
    }

    /// The significant-token sequence is preserved.
    #[test]
    fn preserves_significant_tokens(src in javaish()) {
        let out = fmt(&src);
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// Comment contents are preserved (modulo whitespace).
    #[test]
    fn preserves_comments(src in javaish()) {
        let out = fmt(&src);
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// Never panics on arbitrary Unicode input.
    #[test]
    fn never_panics(src in ".*") {
        let _ = fmt(&src);
    }

    /// Formatting stays idempotent under any blank-line upper bound.
    #[test]
    fn idempotent_under_blank_bound(src in javaish(), bound in 0usize..4) {
        let cfg = Config { max_blank_lines: bound, ..Config::default() };
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// The significant-token sequence is preserved under any blank-line upper bound.
    #[test]
    fn preserves_significant_tokens_under_blank_bound(src in javaish(), bound in 0usize..4) {
        let cfg = Config { max_blank_lines: bound, ..Config::default() };
        let out = fmt_with(&src, &cfg);
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// Reflow keeps formatting idempotent.
    #[test]
    fn wrap_idempotent(src in javaish()) {
        let once = fmt_with(&src, &wrap_config());
        let twice = fmt_with(&once, &wrap_config());
        prop_assert_eq!(once, twice);
    }

    /// Reflow never touches significant tokens (comments are trivia).
    #[test]
    fn wrap_preserves_significant_tokens(src in javaish()) {
        let out = fmt_with(&src, &wrap_config());
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// Reflow preserves comment prose exactly (no word added, dropped, split, or reordered).
    #[test]
    fn wrap_preserves_comment_prose(src in javaish()) {
        let out = fmt_with(&src, &wrap_config());
        prop_assert_eq!(comment_skeleton(&src), comment_skeleton(&out));
    }

    /// Reflow never panics on arbitrary Unicode input.
    #[test]
    fn wrap_never_panics(src in ".*") {
        let _ = fmt_with(&src, &wrap_config());
    }

    /// A CRLF line ending keeps formatting idempotent. (Line endings apply to the breaks the
    /// renderer emits; the interiors of multi-line tokens — string literals, text blocks,
    /// block comments — are preserved verbatim, so they may legitimately keep a bare LF.)
    #[test]
    fn crlf_idempotent(src in javaish()) {
        let cfg = Config { line_ending: jals_fmt::LineEnding::Crlf, ..Config::default() };
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(&once, &twice);
    }

    /// Auto line-ending detection keeps formatting idempotent.
    #[test]
    fn auto_idempotent(src in javaish()) {
        let cfg = Config { line_ending: jals_fmt::LineEnding::Auto, ..Config::default() };
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(&once, &twice);
    }

    /// Next-line brace style keeps formatting idempotent.
    #[test]
    fn next_line_brace_idempotent(src in javaish()) {
        let cfg = Config { brace_style: jals_fmt::BraceStyle::NextLine, ..Config::default() };
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Next-line brace style preserves the significant-token sequence (it only moves the
    /// whitespace around a brace).
    #[test]
    fn next_line_brace_preserves_significant_tokens(src in javaish()) {
        let cfg = Config { brace_style: jals_fmt::BraceStyle::NextLine, ..Config::default() };
        let out = fmt_with(&src, &cfg);
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// Full Allman (both `brace-style` and `control-brace-style` on `next-line`) stays
    /// idempotent.
    #[test]
    fn full_allman_idempotent(src in javaish()) {
        let cfg = Config {
            brace_style: jals_fmt::BraceStyle::NextLine,
            control_brace_style: jals_fmt::ControlBraceStyle::NextLine,
            ..Config::default()
        };
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Full Allman preserves the significant-token sequence.
    #[test]
    fn full_allman_preserves_significant_tokens(src in javaish()) {
        let cfg = Config {
            brace_style: jals_fmt::BraceStyle::NextLine,
            control_brace_style: jals_fmt::ControlBraceStyle::NextLine,
            ..Config::default()
        };
        let out = fmt_with(&src, &cfg);
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// Reorder preserves the *multiset* of significant tokens (the sequence may change, but no
    /// token is added, dropped, or altered). This is the relaxed form of the strict-sequence
    /// invariant that applies when `reorder-imports` is on.
    #[test]
    fn reorder_preserves_significant_token_multiset(src in java_with_imports()) {
        let out = fmt_with(&src, &reorder_config());
        let mut before = sig_tokens(&src);
        let mut after = sig_tokens(&out);
        before.sort();
        after.sort();
        prop_assert_eq!(before, after);
    }

    /// Reorder keeps formatting idempotent.
    #[test]
    fn reorder_idempotent(src in java_with_imports()) {
        let once = fmt_with(&src, &reorder_config());
        let twice = fmt_with(&once, &reorder_config());
        prop_assert_eq!(once, twice);
    }

    /// Reorder preserves comment *contents* as a multiset (none dropped or mangled). Their
    /// order may change, since each comment moves with the import it is glued to.
    #[test]
    fn reorder_preserves_comments(src in java_with_imports()) {
        let out = fmt_with(&src, &reorder_config());
        let mut before = comment_contents(&src);
        let mut after = comment_contents(&out);
        before.sort();
        after.sort();
        prop_assert_eq!(before, after);
    }

    /// Reorder never panics on arbitrary Unicode input.
    #[test]
    fn reorder_never_panics(src in ".*") {
        let _ = fmt_with(&src, &reorder_config());
    }

    /// The emitted import block is actually sorted: non-static first, then static, each
    /// alphabetical by qualified name.
    #[test]
    fn reorder_actually_sorts(src in java_with_imports()) {
        let out = fmt_with(&src, &reorder_config());
        let keys = import_keys(&out);
        let mut sorted = keys.clone();
        sorted.sort();
        prop_assert_eq!(keys, sorted);
    }

    /// `group-imports` keeps formatting idempotent.
    #[test]
    fn group_idempotent(src in java_with_imports()) {
        let once = fmt_with(&src, &group_config());
        let twice = fmt_with(&once, &group_config());
        prop_assert_eq!(once, twice);
    }

    /// `group-imports` preserves the multiset of significant tokens (only their order may change).
    #[test]
    fn group_preserves_significant_token_multiset(src in java_with_imports()) {
        let out = fmt_with(&src, &group_config());
        let mut before = sig_tokens(&src);
        let mut after = sig_tokens(&out);
        before.sort();
        after.sort();
        prop_assert_eq!(before, after);
    }

    /// `group-imports` never drops a comment (they move with their anchoring import, so document
    /// order may change). Compare the multiset.
    #[test]
    fn group_preserves_comments(src in java_with_imports()) {
        let out = fmt_with(&src, &group_config());
        let mut before = comment_contents(&src);
        let mut after = comment_contents(&out);
        before.sort();
        after.sort();
        prop_assert_eq!(before, after);
    }

    /// `group-imports` never panics on arbitrary input.
    #[test]
    fn group_never_panics(src in ".*") {
        let _ = fmt_with(&src, &group_config());
    }

    /// `group-imports` stays idempotent under any blank-line upper bound (the inter-group blank
    /// is clamped like any other run, and collapses to none at bound 0).
    #[test]
    fn group_idempotent_under_blank_bound(src in java_with_imports(), bound in 0usize..4) {
        let cfg = Config { group_imports: true, max_blank_lines: bound, ..Config::default() };
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// With `group-imports`, the emitted imports are grouped: group ranks never decrease across
    /// the block, and within one group the names are sorted.
    #[test]
    fn group_actually_groups(src in java_with_imports()) {
        let out = fmt_with(&src, &group_config());
        let keys = import_group_keys(&out);
        for w in keys.windows(2) {
            prop_assert!(w[0].0 <= w[1].0, "group ranks must be non-decreasing: {:?}", keys);
            if w[0].0 == w[1].0 {
                prop_assert!(w[0].1 <= w[1].1, "names within a group must be sorted: {:?}", keys);
            }
        }
    }

    /// Each trailing-comma mode keeps formatting idempotent.
    #[test]
    fn trailing_comma_idempotent(
        src in javaish(),
        mode in prop_oneof![
            Just(TrailingComma::Always),
            Just(TrailingComma::Never),
            Just(TrailingComma::Vertical),
        ],
    ) {
        let cfg = trailing_config(mode);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Trailing-comma modes preserve every non-comma significant token, in order. Only `,`
    /// tokens (and only those of array initializers) may be added or dropped.
    #[test]
    fn trailing_comma_preserves_non_comma_tokens(
        src in javaish(),
        mode in prop_oneof![
            Just(TrailingComma::Always),
            Just(TrailingComma::Never),
            Just(TrailingComma::Vertical),
        ],
    ) {
        let out = fmt_with(&src, &trailing_config(mode));
        prop_assert_eq!(sig_tokens_without_commas(&src), sig_tokens_without_commas(&out));
    }

    /// Trailing-comma modes never drop or mangle a comment (a comma carrying one is kept).
    #[test]
    fn trailing_comma_preserves_comments(
        src in javaish(),
        mode in prop_oneof![
            Just(TrailingComma::Always),
            Just(TrailingComma::Never),
            Just(TrailingComma::Vertical),
        ],
    ) {
        let out = fmt_with(&src, &trailing_config(mode));
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// Trailing-comma modes never panic on arbitrary Unicode input.
    #[test]
    fn trailing_comma_never_panics(src in ".*") {
        let _ = fmt_with(&src, &trailing_config(TrailingComma::Always));
        let _ = fmt_with(&src, &trailing_config(TrailingComma::Never));
        let _ = fmt_with(&src, &trailing_config(TrailingComma::Vertical));
    }

    /// Binary-expression wrapping stays idempotent under both operator placements.
    #[test]
    fn binop_separator_idempotent(
        src in javaish(),
        sep in prop_oneof![Just(BinopSeparator::Front), Just(BinopSeparator::Back)],
    ) {
        let cfg = binop_config(sep);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Binary-expression wrapping preserves the significant-token sequence exactly — it only
    /// moves whitespace around operators.
    #[test]
    fn binop_separator_preserves_significant_tokens(
        src in javaish(),
        sep in prop_oneof![Just(BinopSeparator::Front), Just(BinopSeparator::Back)],
    ) {
        let out = fmt_with(&src, &binop_config(sep));
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// Binary-expression wrapping never drops or mangles a comment.
    #[test]
    fn binop_separator_preserves_comments(
        src in javaish(),
        sep in prop_oneof![Just(BinopSeparator::Front), Just(BinopSeparator::Back)],
    ) {
        let out = fmt_with(&src, &binop_config(sep));
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// Binary-expression wrapping never panics on arbitrary Unicode input.
    #[test]
    fn binop_separator_never_panics(src in ".*") {
        let _ = fmt_with(&src, &binop_config(BinopSeparator::Front));
        let _ = fmt_with(&src, &binop_config(BinopSeparator::Back));
    }

    /// Intersection-type `&` density stays idempotent under both densities.
    #[test]
    fn type_punctuation_density_idempotent(
        src in javaish(),
        density in prop_oneof![
            Just(TypePunctuationDensity::Wide),
            Just(TypePunctuationDensity::Compressed),
        ],
    ) {
        let cfg = type_punct_config(density);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Intersection-type `&` density is layout-only: it only moves whitespace around `&`, so the
    /// significant-token sequence is preserved exactly.
    #[test]
    fn type_punctuation_density_preserves_significant_tokens(
        src in javaish(),
        density in prop_oneof![
            Just(TypePunctuationDensity::Wide),
            Just(TypePunctuationDensity::Compressed),
        ],
    ) {
        let out = fmt_with(&src, &type_punct_config(density));
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// Intersection-type `&` density never drops or mangles a comment.
    #[test]
    fn type_punctuation_density_preserves_comments(
        src in javaish(),
        density in prop_oneof![
            Just(TypePunctuationDensity::Wide),
            Just(TypePunctuationDensity::Compressed),
        ],
    ) {
        let out = fmt_with(&src, &type_punct_config(density));
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// Intersection-type `&` density never panics on arbitrary Unicode input.
    #[test]
    fn type_punctuation_density_never_panics(src in ".*") {
        let _ = fmt_with(&src, &type_punct_config(TypePunctuationDensity::Wide));
        let _ = fmt_with(&src, &type_punct_config(TypePunctuationDensity::Compressed));
    }

    /// Expanding empty declaration bodies stays idempotent under both settings (re-formatting
    /// an expanded `{` … `}` reproduces it; collapsing a `{}` likewise stays put).
    #[test]
    fn empty_item_single_line_idempotent(
        src in javaish(),
        single_line in prop_oneof![Just(true), Just(false)],
    ) {
        let cfg = empty_single_line_config(single_line);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// `empty-item-single-line` is layout-only: it only moves whitespace inside an empty body,
    /// so the significant-token sequence is preserved exactly under both settings.
    #[test]
    fn empty_item_single_line_preserves_significant_tokens(
        src in javaish(),
        single_line in prop_oneof![Just(true), Just(false)],
    ) {
        let out = fmt_with(&src, &empty_single_line_config(single_line));
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// `empty-item-single-line` never drops or mangles a comment.
    #[test]
    fn empty_item_single_line_preserves_comments(
        src in javaish(),
        single_line in prop_oneof![Just(true), Just(false)],
    ) {
        let out = fmt_with(&src, &empty_single_line_config(single_line));
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// `empty-item-single-line` never panics on arbitrary Unicode input.
    #[test]
    fn empty_item_single_line_never_panics(src in ".*") {
        let _ = fmt_with(&src, &empty_single_line_config(true));
        let _ = fmt_with(&src, &empty_single_line_config(false));
    }

    /// `fn-single-line` is idempotent: re-formatting a collapsed (or un-collapsed) body
    /// reproduces it under both settings.
    #[test]
    fn fn_single_line_idempotent(
        src in javaish(),
        single_line in prop_oneof![Just(true), Just(false)],
    ) {
        let cfg = fn_single_line_config(single_line);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// `fn-single-line` is layout-only: it only moves whitespace inside a single-statement
    /// body, so the significant-token sequence is preserved exactly under both settings.
    #[test]
    fn fn_single_line_preserves_significant_tokens(
        src in javaish(),
        single_line in prop_oneof![Just(true), Just(false)],
    ) {
        let out = fmt_with(&src, &fn_single_line_config(single_line));
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// `fn-single-line` never drops or mangles a comment (a commented body is never collapsed).
    #[test]
    fn fn_single_line_preserves_comments(
        src in javaish(),
        single_line in prop_oneof![Just(true), Just(false)],
    ) {
        let out = fmt_with(&src, &fn_single_line_config(single_line));
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// `fn-single-line` never panics on arbitrary Unicode input.
    #[test]
    fn fn_single_line_never_panics(src in ".*") {
        let _ = fmt_with(&src, &fn_single_line_config(true));
        let _ = fmt_with(&src, &fn_single_line_config(false));
    }

    /// Last-argument overflow stays idempotent: re-formatting the hung layout reproduces it.
    #[test]
    fn overflow_delimited_expr_idempotent(src in javaish()) {
        let cfg = overflow_config();
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Last-argument overflow is layout-only: the significant-token sequence is preserved
    /// exactly (`trailing-comma` stays `preserve`, so the strict invariant is in full force).
    #[test]
    fn overflow_delimited_expr_preserves_significant_tokens(src in javaish()) {
        let out = fmt_with(&src, &overflow_config());
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// Last-argument overflow never drops or mangles a comment.
    #[test]
    fn overflow_delimited_expr_preserves_comments(src in javaish()) {
        let out = fmt_with(&src, &overflow_config());
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// Last-argument overflow never panics on arbitrary Unicode input.
    #[test]
    fn overflow_delimited_expr_never_panics(src in ".*") {
        let _ = fmt_with(&src, &overflow_config());
    }

    /// Each parameter layout keeps formatting idempotent (the `Compressed` packing in
    /// particular: re-formatting the packed lines reproduces the same wrapping).
    #[test]
    fn fn_params_layout_idempotent(src in javaish(), layout in params_layout()) {
        let cfg = params_config(layout);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Parameter layout is layout-only: the significant-token sequence is preserved exactly
    /// (no comma is ever added or dropped — Java forbids a trailing comma in a parameter list).
    #[test]
    fn fn_params_layout_preserves_significant_tokens(src in javaish(), layout in params_layout()) {
        let out = fmt_with(&src, &params_config(layout));
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// Parameter layout never drops or mangles a comment.
    #[test]
    fn fn_params_layout_preserves_comments(src in javaish(), layout in params_layout()) {
        let out = fmt_with(&src, &params_config(layout));
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// Parameter layout never panics on arbitrary Unicode input.
    #[test]
    fn fn_params_layout_never_panics(src in ".*") {
        let _ = fmt_with(&src, &params_config(FnParamsLayout::Tall));
        let _ = fmt_with(&src, &params_config(FnParamsLayout::Compressed));
        let _ = fmt_with(&src, &params_config(FnParamsLayout::Vertical));
    }

    /// Modifier reordering keeps formatting idempotent.
    #[test]
    fn reorder_mods_idempotent(src in javaish()) {
        let once = fmt_with(&src, &reorder_mods_config());
        let twice = fmt_with(&once, &reorder_mods_config());
        prop_assert_eq!(once, twice);
    }

    /// Modifier reordering preserves the *multiset* of significant tokens (the sequence may
    /// change, but no token is added, dropped, or altered) — the relaxed invariant that applies
    /// when `reorder-modifiers` is on.
    #[test]
    fn reorder_mods_preserves_significant_token_multiset(src in javaish()) {
        let out = fmt_with(&src, &reorder_mods_config());
        let mut before = sig_tokens(&src);
        let mut after = sig_tokens(&out);
        before.sort();
        after.sort();
        prop_assert_eq!(before, after);
    }

    /// Modifier reordering never drops or mangles a comment (each moves with the modifier it is
    /// glued to, so document order may change). Compare the multiset.
    #[test]
    fn reorder_mods_preserves_comments(src in javaish()) {
        let out = fmt_with(&src, &reorder_mods_config());
        let mut before = comment_contents(&src);
        let mut after = comment_contents(&out);
        before.sort();
        after.sort();
        prop_assert_eq!(before, after);
    }

    /// Modifier reordering never panics on arbitrary Unicode input.
    #[test]
    fn reorder_mods_never_panics(src in ".*") {
        let _ = fmt_with(&src, &reorder_mods_config());
    }

    /// On a targeted generator of scrambled modifier runs, reordering still preserves the
    /// significant-token multiset and stays idempotent.
    #[test]
    fn reorder_mods_targeted_multiset_and_idempotent(src in java_with_modifiers()) {
        let once = fmt_with(&src, &reorder_mods_config());
        let twice = fmt_with(&once, &reorder_mods_config());
        prop_assert_eq!(&once, &twice);
        let mut before = sig_tokens(&src);
        let mut after = sig_tokens(&once);
        before.sort();
        after.sort();
        prop_assert_eq!(before, after);
    }

    /// Every emitted `MODIFIERS` node is canonical: all annotations precede every keyword
    /// modifier, and the keyword modifiers are in non-decreasing canonical rank.
    #[test]
    fn reorder_mods_emits_canonical_order(src in java_with_modifiers()) {
        let out = fmt_with(&src, &reorder_mods_config());
        for m in parse(&out)
            .syntax()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::MODIFIERS)
        {
            let mut seen_keyword = false;
            let mut last_rank = 0usize;
            for e in m
                .children_with_tokens()
                .filter(|e| !e.kind().is_trivia())
            {
                match modifier_rank(e.kind()) {
                    Some(rank) => {
                        prop_assert!(rank >= last_rank, "keyword ranks must be non-decreasing");
                        last_rank = rank;
                        seen_keyword = true;
                    }
                    // An annotation appearing after a keyword would mean it was not hoisted.
                    None => prop_assert!(!seen_keyword, "annotations must precede keyword modifiers"),
                }
            }
        }
    }
}
