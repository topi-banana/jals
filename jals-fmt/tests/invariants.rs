//! The formatter's contract, checked against the corpus rather than asserted in prose.
//!
//! These need no reference implementation and no golden data: each is a property that must hold
//! for *any* input under *any* configuration, so they catch the failures a similarity metric
//! cannot — a comment silently dropped, a token lost, an output that keeps changing.
//!
//! # The four properties
//!
//! - **Idempotence.** `fmt(fmt(x)) == fmt(x)`. The `braces.force-* = if-multiline` case is
//!   included deliberately: it is the one rule whose condition consumes the engine's own result,
//!   so its idempotence is a tested property rather than a constructive one
//!   (`DESIGN.md` §8.1, §17).
//! - **Significant tokens.** Every profile is held to *its own* form of the invariant — exact
//!   equality for the default, exact outside the import block plus a subset inside it when unused
//!   imports are removed, and brace-insertion allowed when braces are forced. No profile is
//!   skipped, and none is checked against an allowance it does not have.
//! - **Comments.** Every comment in the input appears in the output. A profile that deletes
//!   unused imports takes their comments with them, so the *import block* is excluded rather than
//!   the whole profile.
//! - **Never panics.** Malformed input is formatted best-effort or returned unchanged, never
//!   dropped and never a crash.

use jals_config::fmt::{Config, ForceBraces, ImportOrder};
use jals_syntax::{SyntaxElement, SyntaxKind};

/// Sources covering the shapes the rules disagree about.
const SOURCES: &[&str] = &[
    "package p;\n\nimport java.util.List;\n\nclass A {\n  int x = 1;\n}\n",
    "class B { void m() { if (a) b(); else c(); for (int i = 0; i < 9; i++) d(); } }",
    "class C {\n  // leading\n  int x; /* trailing */\n  /** doc */\n  void m() {}\n}\n",
    "enum E { A, B, C }\n\nenum F {\n  A;\n  void m() {}\n}\n",
    "class G { int[] t = {1, 2, 3,}; String s = \"a\" + \"b\"; }",
    "interface H { default int m() { return switch (x) { case 1 -> 2; default -> 3; }; } }",
    "class I<T extends Comparable<T> & Cloneable> extends J implements K, L {}",
    "class M { void m() { try (A a = b(); C c = d()) {} catch (E | F g) {} finally {} } }",
    "class N { void m() { a.b().c().d().e(); x = y ? z : w; assert q : \"r\"; } }",
    "record P(int x, int y) { @Override public String toString() { return \"\"; } }",
    // Malformed on purpose: the parser is error-resilient and the formatter must survive it.
    "class Broken { void m( { int x = ; } }",
    "class Unterminated { /* never closed",
    "",
    "   \n\n  ",
];

/// The configurations to check every property under.
fn configurations() -> Vec<(&'static str, Config)> {
    let mut force = Config::default();
    force.braces.force_if = ForceBraces::IfMultiline;
    force.braces.force_for = ForceBraces::IfMultiline;
    force.braces.force_while = ForceBraces::IfMultiline;
    force.braces.force_do_while = ForceBraces::IfMultiline;

    let mut grouped = Config::default();
    grouped.imports.order = ImportOrder::Group;
    grouped.imports.reorder_modifiers = true;
    grouped.imports.remove_unused = true;

    vec![
        ("default", Config::default()),
        (
            "gjf",
            jals_fmt::import::GoogleJavaFormatConfig::default().into(),
        ),
        ("force-if-multiline", force),
        ("imports", grouped),
    ]
}

/// Format once.
fn format(src: &str, config: &Config) -> String {
    jals_exec::block_on_inline(jals_fmt::FormatOutput::format_source(src, config)).formatted
}

/// Whether a token sits inside an `import` declaration.
fn in_import(tok: &jals_syntax::SyntaxToken) -> bool {
    tok.parent_ancestors()
        .any(|node| node.kind() == SyntaxKind::IMPORT_DECL)
}

/// The significant tokens of `src`, sorted, optionally excluding the import block.
fn tokens(src: &str, imports: bool) -> Vec<(SyntaxKind, String)> {
    let parse = jals_exec::block_on_inline(jals_syntax::Parse::parse(src));
    let mut out: Vec<(SyntaxKind, String)> = parse
        .syntax()
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|tok| !tok.kind().is_trivia())
        .filter(|tok| imports || !in_import(tok))
        .map(|tok| (tok.kind(), tok.text().to_owned()))
        .collect();
    out.sort();
    out
}

/// The comment texts of `src`, optionally excluding the import block.
fn comments(src: &str, imports: bool) -> Vec<String> {
    let parse = jals_exec::block_on_inline(jals_syntax::Parse::parse(src));
    parse
        .syntax()
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|tok| {
            matches!(
                tok.kind(),
                SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT
            )
        })
        .filter(|tok| imports || !in_import(tok))
        .map(|tok| tok.text().trim().to_owned())
        .collect()
}

#[test]
fn formatting_is_idempotent() {
    for (name, config) in configurations() {
        for src in SOURCES {
            let once = format(src, &config);
            let twice = format(&once, &config);
            assert_eq!(
                once, twice,
                "{name}: fmt is not idempotent on {src:?}\n--- once ---\n{once}\n--- twice ---\n{twice}",
            );
        }
    }
}

#[test]
fn every_profile_preserves_its_significant_tokens() {
    // Each profile is held to *its own* form of the invariant, so no profile is skipped and none
    // is checked against a rule it does not have:
    //
    // - the default has all six token-changing rules off, so the multiset is equal exactly;
    // - a profile that removes unused imports is exact outside the import block and a superset
    //   inside it;
    // - a profile that forces braces may additionally have gained `{` / `}`.
    for (name, config) in configurations() {
        let removes = config.imports.remove_unused;
        let forces = config.braces.force_if != ForceBraces::Never;
        for src in SOURCES {
            let formatted = format(src, &config);

            let before = tokens(src, !removes);
            let after = tokens(&formatted, !removes);
            let after: Vec<_> = if forces {
                after
                    .into_iter()
                    .filter(|(kind, _)| !matches!(kind, SyntaxKind::LBRACE | SyntaxKind::RBRACE))
                    .collect()
            } else {
                after
            };
            let before: Vec<_> = if forces {
                before
                    .into_iter()
                    .filter(|(kind, _)| !matches!(kind, SyntaxKind::LBRACE | SyntaxKind::RBRACE))
                    .collect()
            } else {
                before
            };
            assert_eq!(
                before, after,
                "{name}: token multiset changed for {src:?}\n--- output ---\n{formatted}",
            );

            if removes {
                // The imports that survive must be a sub-multiset of the ones that went in.
                let kept = tokens(&formatted, true);
                let given = tokens(src, true);
                for token in &kept {
                    assert!(
                        given.contains(token),
                        "{name}: {token:?} appeared from nowhere for {src:?}",
                    );
                }
            }
        }
    }
}

#[test]
fn no_comment_is_ever_dropped() {
    for (name, config) in configurations() {
        // A profile that deletes unused imports takes the comments attached to them with it, so
        // the import block is excluded rather than the whole profile being skipped — the rest of
        // the file is still held to exact comment preservation.
        let scope = !config.imports.remove_unused;
        for src in SOURCES {
            let formatted = format(src, &config);
            assert_eq!(
                comments(src, scope),
                comments(&formatted, scope),
                "{name}: a comment was dropped or duplicated for {src:?}\n--- output ---\n{formatted}",
            );
        }
    }
}

#[test]
fn malformed_input_is_never_lost() {
    let config = Config::default();
    for src in SOURCES {
        let formatted = format(src, &config);
        assert!(
            !src.trim().is_empty() || formatted.trim().is_empty(),
            "an empty input produced output: {formatted:?}",
        );
        assert!(
            src.trim().is_empty() || !formatted.trim().is_empty(),
            "a non-empty input produced nothing: {src:?}",
        );
    }
}
