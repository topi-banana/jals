//! Property tests for the formatter's correctness invariants.

use jals_fmt::{
    AnnotationPlacement, BinopSeparator, Config, FloatLiteralTrailingZero, FnParamsLayout,
    HexLiteralCase, LiteralSuffixCase, TrailingComma, TypePunctuationDensity, format_source,
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

/// Config with a given annotation placement.
fn annotation_placement_config(annotation_placement: AnnotationPlacement) -> Config {
    Config {
        annotation_placement,
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

/// Config with a given `force-multiline-blocks` setting.
fn force_multiline_blocks_config(force_multiline_blocks: bool) -> Config {
    Config {
        force_multiline_blocks,
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

/// Config with a given hex-literal-case policy.
fn hex_config(hex_literal_case: HexLiteralCase) -> Config {
    Config {
        hex_literal_case,
        ..Config::default()
    }
}

/// A generator over the two non-default hex-literal-case policies.
fn hex_case() -> impl Strategy<Value = HexLiteralCase> {
    prop_oneof![Just(HexLiteralCase::Upper), Just(HexLiteralCase::Lower)]
}

/// A class whose fields are initialized with a random mix of hex integers / floats (mixed-case,
/// with `_` separators and `l`/`L`/`f`/`d` suffixes) and non-hex literals, so `hex-literal-case`
/// has real material to normalize. As with the other targeted generators, these need not be
/// semantically legal Java — the parser is error-resilient and still produces the literal tokens.
fn java_with_hex_literals() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("0xFF"),
            Just("0Xff"),
            Just("0xCafeBabe"),
            Just("0xDEAD_beefL"),
            Just("0xAl"),
            Just("0x1.8p3"),
            Just("0xA.Bp1f"),
            Just("0X1p-2d"),
            Just("0xabcDEF"),
            Just("255"),
            Just("0777"),
            Just("0b1010"),
            Just("3.14f"),
        ],
        0..8,
    )
    .prop_map(|literals| {
        let fields = literals
            .iter()
            .enumerate()
            .map(|(i, lit)| format!("    int x{i} = {lit};"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("class C {{\n{fields}\n}}\n")
    })
}

/// Lowercase only a hex literal's *mantissa* digits, mirroring the exact scope of
/// `hex-literal-case` (the `0x`/`0X` prefix, the `p`/`P` exponent and its decimal digits, and any
/// `l`/`f`/`d` suffix are left as-is). Used to compare token streams modulo the only thing the
/// option may change.
fn canon_hex(kind: SyntaxKind, text: &str) -> String {
    if !matches!(kind, SyntaxKind::INT_LITERAL | SyntaxKind::FLOAT_LITERAL) {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'0' || !matches!(bytes[1], b'x' | b'X') {
        return text.to_string();
    }
    let mantissa_end = match bytes[2..].iter().position(|b| matches!(b, b'p' | b'P')) {
        Some(i) => i + 2,
        None if matches!(bytes.last(), Some(b'l' | b'L')) => text.len() - 1,
        None => text.len(),
    };
    format!(
        "{}{}{}",
        &text[..2],
        text[2..mantissa_end].to_ascii_lowercase(),
        &text[mantissa_end..]
    )
}

/// The significant tokens of `src`, with every hex literal's mantissa case canonicalized. Equal
/// before and after formatting iff the token *kinds* are preserved exactly and every token's text
/// is preserved except (possibly) the case of a hex literal's mantissa digits.
fn sig_tokens_canon_hex(src: &str) -> Vec<(SyntaxKind, String)> {
    sig_tokens(src)
        .into_iter()
        .map(|(k, t)| (k, canon_hex(k, &t)))
        .collect()
}

/// Config with a given float-literal-trailing-zero policy.
fn float_config(float_literal_trailing_zero: FloatLiteralTrailingZero) -> Config {
    Config {
        float_literal_trailing_zero,
        ..Config::default()
    }
}

/// A generator over the two non-default float-literal-trailing-zero policies.
fn float_zero_mode() -> impl Strategy<Value = FloatLiteralTrailingZero> {
    prop_oneof![
        Just(FloatLiteralTrailingZero::Always),
        Just(FloatLiteralTrailingZero::Never),
    ]
}

/// A class whose fields are initialized with a random mix of in-scope decimal floats (empty,
/// single-zero, and multi-zero fractions, with `f` suffixes and `e` exponents) and out-of-scope
/// literals (non-zero fractions, leading-dot, dotless, hex floats, integers), so
/// `float-literal-trailing-zero` has real material to normalize and real material to leave alone.
/// As with the other targeted generators these need not be semantically legal Java — the parser is
/// error-resilient and still produces the literal tokens.
fn java_with_float_literals() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("1.0"),
            Just("1."),
            Just("1.00"),
            Just("0.0"),
            Just("1.0f"),
            Just("1.f"),
            Just("1.0e10"),
            Just("1.e10"),
            Just("1.5"),
            Just("1.50"),
            Just(".5"),
            Just(".0"),
            Just("1.0_0"),
            Just("1e10"),
            Just("100f"),
            Just("0x1.0p3"),
            Just("255"),
        ],
        0..8,
    )
    .prop_map(|literals| {
        let fields = literals
            .iter()
            .enumerate()
            .map(|(i, lit)| format!("    double x{i} = {lit};"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("class C {{\n{fields}\n}}\n")
    })
}

/// Collapse the trailing zero of an in-scope decimal float so `1.`, `1.0`, and `1.00` all map to
/// the same string (the bare-dot form), mirroring the exact scope of `float-literal-trailing-zero`
/// (a non-zero fraction, a leading-dot float, a dotless float, a hex float, and any integer are
/// left as-is). Used to compare token streams modulo the only thing the option may change.
fn canon_float_zero(kind: SyntaxKind, text: &str) -> String {
    if kind != SyntaxKind::FLOAT_LITERAL {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    // Hex floats are out of scope.
    if bytes.len() >= 2 && bytes[0] == b'0' && matches!(bytes[1], b'x' | b'X') {
        return text.to_string();
    }
    let Some(dot) = bytes.iter().position(|&b| b == b'.') else {
        return text.to_string();
    };
    let mut frac_end = dot + 1;
    while frac_end < bytes.len() && (bytes[frac_end].is_ascii_digit() || bytes[frac_end] == b'_') {
        frac_end += 1;
    }
    // Strip an all-zero fraction (with a non-empty integer part) to the bare dot: `1.0` / `1.00`
    // canonicalize to `1.`, exactly the form the empty fraction already has.
    if dot > 0 && frac_end > dot + 1 && bytes[dot + 1..frac_end].iter().all(|&b| b == b'0') {
        format!("{}{}", &text[..dot + 1], &text[frac_end..])
    } else {
        text.to_string()
    }
}

/// The significant tokens of `src`, with every in-scope float literal's trailing zero canonicalized.
/// Equal before and after formatting iff the token *kinds* are preserved exactly and every token's
/// text is preserved except (possibly) an in-scope decimal float's trailing zero.
fn sig_tokens_canon_float(src: &str) -> Vec<(SyntaxKind, String)> {
    sig_tokens(src)
        .into_iter()
        .map(|(k, t)| (k, canon_float_zero(k, &t)))
        .collect()
}

/// Config with a given literal-suffix-case policy.
fn suffix_config(literal_suffix_case: LiteralSuffixCase) -> Config {
    Config {
        literal_suffix_case,
        ..Config::default()
    }
}

/// A generator over the two non-default literal-suffix-case policies.
fn suffix_case() -> impl Strategy<Value = LiteralSuffixCase> {
    prop_oneof![
        Just(LiteralSuffixCase::Upper),
        Just(LiteralSuffixCase::Lower)
    ]
}

/// A class whose fields are initialized with a random mix of suffixed literals — the integer
/// `l`/`L` (decimal and hex) and the floating-point `f`/`F`/`d`/`D` (decimal and hex) — and
/// literals out of scope for `literal-suffix-case`: an unsuffixed integer / float, and a hex
/// integer whose trailing `f`/`d`/`F`/`D` is a *digit* not a suffix (`0xabcdef`, `0xFD`). As with
/// the other targeted generators these need not be semantically legal Java — the parser is
/// error-resilient and still produces the literal tokens.
fn java_with_suffix_literals() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("123l"),
            Just("456L"),
            Just("0xCAFEl"),
            Just("0xBEEFL"),
            Just("1.5f"),
            Just("2.5F"),
            Just("3.5d"),
            Just("4.5D"),
            Just("0x1p3f"),
            Just("0x1.8p3D"),
            Just("0xabcdef"),
            Just("0xFD"),
            Just("0xff"),
            Just("255"),
            Just("1.5"),
            Just("1e10"),
        ],
        0..8,
    )
    .prop_map(|literals| {
        let fields = literals
            .iter()
            .enumerate()
            .map(|(i, lit)| format!("    int x{i} = {lit};"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("class C {{\n{fields}\n}}\n")
    })
}

/// Lowercase only a numeric literal's trailing type-suffix letter (the integer `l`/`L`, or the
/// floating-point `f`/`F`/`d`/`D`, disambiguated by the token kind so an integer's trailing hex
/// digit is never touched), mirroring the exact scope of `literal-suffix-case`. Used to compare
/// token streams modulo the only thing the option may change.
fn canon_suffix(kind: SyntaxKind, text: &str) -> String {
    let Some(&last) = text.as_bytes().last() else {
        return text.to_string();
    };
    let is_suffix = match kind {
        SyntaxKind::INT_LITERAL => matches!(last, b'l' | b'L'),
        SyntaxKind::FLOAT_LITERAL => matches!(last, b'f' | b'F' | b'd' | b'D'),
        _ => false,
    };
    if !is_suffix {
        return text.to_string();
    }
    format!(
        "{}{}",
        &text[..text.len() - 1],
        last.to_ascii_lowercase() as char
    )
}

/// The significant tokens of `src`, with every numeric literal's trailing suffix letter
/// canonicalized. Equal before and after formatting iff the token *kinds* are preserved exactly
/// and every token's text is preserved except (possibly) the case of a literal's suffix letter.
fn sig_tokens_canon_suffix(src: &str) -> Vec<(SyntaxKind, String)> {
    sig_tokens(src)
        .into_iter()
        .map(|(k, t)| (k, canon_suffix(k, &t)))
        .collect()
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

    /// Formatting stays idempotent under any continuation-indent (a narrow width forces wrapping
    /// so the continuation indent is actually exercised).
    #[test]
    fn idempotent_under_continuation_indent(
        src in javaish(),
        cont in prop::option::of(0usize..8),
        max_width in 20usize..60,
    ) {
        let cfg = Config { continuation_indent: cont, max_width, ..Config::default() };
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Continuation-indent is layout-only: the significant-token sequence is preserved.
    #[test]
    fn preserves_significant_tokens_under_continuation_indent(
        src in javaish(),
        cont in prop::option::of(0usize..8),
        max_width in 20usize..60,
    ) {
        let cfg = Config { continuation_indent: cont, max_width, ..Config::default() };
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

    /// Annotation placement stays idempotent under both modes.
    #[test]
    fn annotation_placement_idempotent(
        src in javaish(),
        placement in prop_oneof![
            Just(AnnotationPlacement::Compact),
            Just(AnnotationPlacement::Expanded),
        ],
    ) {
        let cfg = annotation_placement_config(placement);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// On a targeted generator of scrambled modifier runs (including annotations and comments),
    /// annotation placement stays idempotent.
    #[test]
    fn annotation_placement_targeted_idempotent(
        src in java_with_modifiers(),
        placement in prop_oneof![
            Just(AnnotationPlacement::Compact),
            Just(AnnotationPlacement::Expanded),
        ],
    ) {
        let cfg = annotation_placement_config(placement);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Annotation placement is layout-only: it only moves line breaks between an annotation and
    /// the following modifier/declaration, so the significant-token *sequence* is preserved
    /// exactly (the strict invariant, composing with the default).
    #[test]
    fn annotation_placement_preserves_significant_tokens(
        src in javaish(),
        placement in prop_oneof![
            Just(AnnotationPlacement::Compact),
            Just(AnnotationPlacement::Expanded),
        ],
    ) {
        let out = fmt_with(&src, &annotation_placement_config(placement));
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// Annotation placement never drops or mangles a comment.
    #[test]
    fn annotation_placement_preserves_comments(
        src in javaish(),
        placement in prop_oneof![
            Just(AnnotationPlacement::Compact),
            Just(AnnotationPlacement::Expanded),
        ],
    ) {
        let out = fmt_with(&src, &annotation_placement_config(placement));
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// Annotation placement never panics on arbitrary Unicode input.
    #[test]
    fn annotation_placement_never_panics(src in ".*") {
        let _ = fmt_with(&src, &annotation_placement_config(AnnotationPlacement::Compact));
        let _ = fmt_with(&src, &annotation_placement_config(AnnotationPlacement::Expanded));
    }

    /// Composed with `reorder-modifiers`, expanding annotations still preserves the
    /// significant-token *multiset* (the sequence may change, since reordering permutes
    /// modifiers) and stays idempotent.
    #[test]
    fn annotation_placement_expanded_with_reorder_preserves_multiset(src in java_with_modifiers()) {
        let cfg = Config {
            reorder_modifiers: true,
            annotation_placement: AnnotationPlacement::Expanded,
            ..Config::default()
        };
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(&once, &twice);
        let mut before = sig_tokens(&src);
        let mut after = sig_tokens(&once);
        before.sort();
        after.sort();
        prop_assert_eq!(before, after);
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

    /// `force-multiline-blocks` is idempotent: re-formatting the expanded blocks reproduces
    /// them under both settings.
    #[test]
    fn force_multiline_blocks_idempotent(
        src in javaish(),
        force in prop_oneof![Just(true), Just(false)],
    ) {
        let cfg = force_multiline_blocks_config(force);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// `force-multiline-blocks` is layout-only: it only expands blocks onto more lines, so the
    /// significant-token sequence is preserved exactly under both settings.
    #[test]
    fn force_multiline_blocks_preserves_significant_tokens(
        src in javaish(),
        force in prop_oneof![Just(true), Just(false)],
    ) {
        let out = fmt_with(&src, &force_multiline_blocks_config(force));
        prop_assert_eq!(sig_tokens(&src), sig_tokens(&out));
    }

    /// `force-multiline-blocks` never drops or mangles a comment.
    #[test]
    fn force_multiline_blocks_preserves_comments(
        src in javaish(),
        force in prop_oneof![Just(true), Just(false)],
    ) {
        let out = fmt_with(&src, &force_multiline_blocks_config(force));
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// `force-multiline-blocks` never panics on arbitrary Unicode input.
    #[test]
    fn force_multiline_blocks_never_panics(src in ".*") {
        let _ = fmt_with(&src, &force_multiline_blocks_config(true));
        let _ = fmt_with(&src, &force_multiline_blocks_config(false));
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

    /// Each hex-literal-case mode keeps formatting idempotent (the literals are already normalized
    /// after the first pass, so the second reproduces them).
    #[test]
    fn hex_literal_case_idempotent(src in javaish(), case in hex_case()) {
        let cfg = hex_config(case);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Hex-literal-case preserves the token *kind* sequence exactly, and every token's text except
    /// (possibly) the case of a hex literal's mantissa digits. No token is added, dropped, or
    /// otherwise altered — the relaxed invariant that applies when `hex-literal-case` is on.
    #[test]
    fn hex_literal_case_preserves_tokens_modulo_hex_case(
        src in java_with_hex_literals(),
        case in hex_case(),
    ) {
        let out = fmt_with(&src, &hex_config(case));
        prop_assert_eq!(sig_tokens_canon_hex(&src), sig_tokens_canon_hex(&out));
    }

    /// The emitted hex literals are actually in the requested case: no `a`–`f` letter survives
    /// under `Upper`, and no `A`–`F` under `Lower`, anywhere in a hex literal's mantissa.
    #[test]
    fn hex_literal_case_actually_normalizes(
        src in java_with_hex_literals(),
        case in hex_case(),
    ) {
        let out = fmt_with(&src, &hex_config(case));
        for (kind, t) in sig_tokens(&out) {
            if !matches!(kind, SyntaxKind::INT_LITERAL | SyntaxKind::FLOAT_LITERAL) {
                continue;
            }
            let b = t.as_bytes();
            if b.len() < 2 || b[0] != b'0' || !matches!(b[1], b'x' | b'X') {
                continue;
            }
            // Inspect only the mantissa (before a `p`/`P` exponent, before an `l`/`L` suffix).
            let end = match b[2..].iter().position(|x| matches!(x, b'p' | b'P')) {
                Some(i) => i + 2,
                None if matches!(b.last(), Some(b'l' | b'L')) => t.len() - 1,
                None => t.len(),
            };
            for &c in &b[2..end] {
                match case {
                    HexLiteralCase::Upper => prop_assert!(
                        !c.is_ascii_lowercase(),
                        "found lower-case digit in {t:?} under Upper"
                    ),
                    HexLiteralCase::Lower => prop_assert!(
                        !c.is_ascii_uppercase(),
                        "found upper-case digit in {t:?} under Lower"
                    ),
                    HexLiteralCase::Preserve => {}
                }
            }
        }
    }

    /// Hex-literal-case never drops or mangles a comment.
    #[test]
    fn hex_literal_case_preserves_comments(src in javaish(), case in hex_case()) {
        let out = fmt_with(&src, &hex_config(case));
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// Hex-literal-case never panics on arbitrary Unicode input.
    #[test]
    fn hex_literal_case_never_panics(src in ".*") {
        let _ = fmt_with(&src, &hex_config(HexLiteralCase::Upper));
        let _ = fmt_with(&src, &hex_config(HexLiteralCase::Lower));
    }

    /// Each float-literal-trailing-zero mode keeps formatting idempotent (the literals are already
    /// normalized after the first pass, so the second reproduces them).
    #[test]
    fn float_literal_trailing_zero_idempotent(src in javaish(), mode in float_zero_mode()) {
        let cfg = float_config(mode);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Float-literal-trailing-zero preserves the token *kind* sequence exactly, and every token's
    /// text except (possibly) an in-scope decimal float's trailing zero. No token is added, dropped,
    /// or otherwise altered — the relaxed invariant that applies when the option is on.
    #[test]
    fn float_literal_trailing_zero_preserves_tokens_modulo_zero(
        src in java_with_float_literals(),
        mode in float_zero_mode(),
    ) {
        let out = fmt_with(&src, &float_config(mode));
        prop_assert_eq!(sig_tokens_canon_float(&src), sig_tokens_canon_float(&out));
    }

    /// The emitted float literals are actually normalized: under `Always` no in-scope decimal float
    /// has an empty fraction (a digit always follows the `.`), and under `Never` no decimal float
    /// with a non-empty integer part has an all-zero fraction.
    #[test]
    fn float_literal_trailing_zero_actually_normalizes(
        src in java_with_float_literals(),
        mode in float_zero_mode(),
    ) {
        let out = fmt_with(&src, &float_config(mode));
        for (kind, t) in sig_tokens(&out) {
            if kind != SyntaxKind::FLOAT_LITERAL {
                continue;
            }
            let b = t.as_bytes();
            // Skip hex floats (out of scope) and dotless floats (no fraction to normalize).
            if b.len() >= 2 && b[0] == b'0' && matches!(b[1], b'x' | b'X') {
                continue;
            }
            let Some(dot) = b.iter().position(|&c| c == b'.') else {
                continue;
            };
            let mut frac_end = dot + 1;
            while frac_end < b.len() && (b[frac_end].is_ascii_digit() || b[frac_end] == b'_') {
                frac_end += 1;
            }
            match mode {
                FloatLiteralTrailingZero::Always => prop_assert!(
                    frac_end > dot + 1,
                    "found empty fraction in {t:?} under Always"
                ),
                FloatLiteralTrailingZero::Never => prop_assert!(
                    !(dot > 0
                        && frac_end > dot + 1
                        && b[dot + 1..frac_end].iter().all(|&c| c == b'0')),
                    "found all-zero fraction in {t:?} under Never"
                ),
                FloatLiteralTrailingZero::Preserve => {}
            }
        }
    }

    /// Float-literal-trailing-zero never drops or mangles a comment.
    #[test]
    fn float_literal_trailing_zero_preserves_comments(src in javaish(), mode in float_zero_mode()) {
        let out = fmt_with(&src, &float_config(mode));
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// Float-literal-trailing-zero never panics on arbitrary Unicode input.
    #[test]
    fn float_literal_trailing_zero_never_panics(src in ".*") {
        let _ = fmt_with(&src, &float_config(FloatLiteralTrailingZero::Always));
        let _ = fmt_with(&src, &float_config(FloatLiteralTrailingZero::Never));
    }

    /// Each literal-suffix-case mode keeps formatting idempotent (the suffixes are already in the
    /// requested case after the first pass, so the second reproduces them).
    #[test]
    fn literal_suffix_case_idempotent(src in javaish(), case in suffix_case()) {
        let cfg = suffix_config(case);
        let once = fmt_with(&src, &cfg);
        let twice = fmt_with(&once, &cfg);
        prop_assert_eq!(once, twice);
    }

    /// Literal-suffix-case preserves the token *kind* sequence exactly, and every token's text
    /// except (possibly) the case of a literal's trailing suffix letter. No token is added,
    /// dropped, or otherwise altered — the relaxed invariant that applies when the option is on.
    #[test]
    fn literal_suffix_case_preserves_tokens_modulo_suffix_case(
        src in java_with_suffix_literals(),
        case in suffix_case(),
    ) {
        let out = fmt_with(&src, &suffix_config(case));
        prop_assert_eq!(sig_tokens_canon_suffix(&src), sig_tokens_canon_suffix(&out));
    }

    /// The emitted literals' suffixes are actually in the requested case: every in-scope trailing
    /// suffix letter (the integer `l`/`L`, the float `f`/`F`/`d`/`D`) is upper under `Upper` and
    /// lower under `Lower`. A hex integer's trailing digit is not a suffix and is left alone.
    #[test]
    fn literal_suffix_case_actually_normalizes(
        src in java_with_suffix_literals(),
        case in suffix_case(),
    ) {
        let out = fmt_with(&src, &suffix_config(case));
        for (kind, t) in sig_tokens(&out) {
            let Some(&last) = t.as_bytes().last() else {
                continue;
            };
            let is_suffix = match kind {
                SyntaxKind::INT_LITERAL => matches!(last, b'l' | b'L'),
                SyntaxKind::FLOAT_LITERAL => matches!(last, b'f' | b'F' | b'd' | b'D'),
                _ => false,
            };
            if !is_suffix {
                continue;
            }
            match case {
                LiteralSuffixCase::Upper => prop_assert!(
                    last.is_ascii_uppercase(),
                    "found lower-case suffix in {t:?} under Upper"
                ),
                LiteralSuffixCase::Lower => prop_assert!(
                    last.is_ascii_lowercase(),
                    "found upper-case suffix in {t:?} under Lower"
                ),
                LiteralSuffixCase::Preserve => {}
            }
        }
    }

    /// Literal-suffix-case never drops or mangles a comment.
    #[test]
    fn literal_suffix_case_preserves_comments(src in javaish(), case in suffix_case()) {
        let out = fmt_with(&src, &suffix_config(case));
        prop_assert_eq!(comment_contents(&src), comment_contents(&out));
    }

    /// Literal-suffix-case never panics on arbitrary Unicode input.
    #[test]
    fn literal_suffix_case_never_panics(src in ".*") {
        let _ = fmt_with(&src, &suffix_config(LiteralSuffixCase::Upper));
        let _ = fmt_with(&src, &suffix_config(LiteralSuffixCase::Lower));
    }
}
