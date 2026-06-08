//! Property tests for the formatter's correctness invariants.

use jals_fmt::{Config, format_source};
use jals_syntax::{SyntaxKind, parse};
use proptest::prelude::*;

fn fmt(src: &str) -> String {
    format_source(src, &Config::default()).formatted
}

fn fmt_with(src: &str, config: &Config) -> String {
    format_source(src, config).formatted
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
            Just("\n"),
            Just(" "),
            Just("\t"),
        ],
        0..40,
    )
    .prop_map(|parts| parts.concat())
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
}
