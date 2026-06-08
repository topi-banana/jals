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
}
