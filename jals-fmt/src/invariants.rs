//! The formatter's contract, checked against the corpus rather than asserted in prose.
//!
//! These need no reference implementation and no golden data: each is a property that must hold for
//! *any* input under *any* configuration, so they catch the failures a similarity metric cannot — a
//! comment silently dropped, a token lost, an output that keeps changing.
//!
//! # The five properties
//!
//! - **Idempotence.** `fmt(fmt(x)) == fmt(x)`. The `braces.force-* = if-multiline` case is included
//!   deliberately: it is the one rule whose condition consumes the engine's own result, so its
//!   idempotence is a tested property rather than a constructive one (`DESIGN.md` §8.1, §17).
//! - **The fail-safe never fires.** Every source formats to something the formatter can vouch for.
//!   The other four are *preservation* properties, and a total fallback satisfies all of them at
//!   once — `output == input` is exactly what preservation permits. This is the *progress* property,
//!   and its absence is why a bug that returned whole files unformatted had no test-shaped hole to
//!   fall into.
//! - **Significant tokens.** A token no [`OPERATIONS`](crate::passes::token_license::OPERATIONS) row
//!   can reach must come out with an identical count, and a token inside a scope a row may *empty*
//!   may go missing but never appear.
//! - **Comments.** Every comment in the input appears in the output. A profile that deletes unused
//!   imports takes their comments with them, so the *import block* is excluded rather than the whole
//!   profile.
//! - **Never panics.** Malformed input is formatted best-effort or returned unchanged, never dropped
//!   and never a crash.
//!
//! Alongside them, one thing that is *not* a property of the formatter but of this file's own
//! position: the fail-safe's verdict has to survive the trip out through
//! [`FormatOutput`](crate::FormatOutput). The corpus is the only place that can compare the two, since
//! it is the only place that can see both [`Formatted`] and the public output.
//!
//! # Why this lives in `src`
//!
//! It reads its allowances off a [`License`], and `cargo test` compiles the library twice — once
//! with `cfg(test)`, once without, for `tests/` to link against — so a crate-internal type is
//! structurally unreachable from an integration test. The alternative was widening a five-item
//! public surface for a test.
//!
//! The cost of being inside is that these drive [`Formatter::run`] directly instead of the public
//! [`FormatOutput::format_source`](crate::FormatOutput::format_source), which is the only stage that
//! entry point adds today. Should it ever grow a second one, that stage would sit outside every
//! property here — so a step added there belongs in [`Formatter::run`], or this corpus needs a second
//! driver.
//!
//! The split is deliberate: the **policy** (which changes are allowed) comes from the license, so it
//! cannot disagree with the fail-safe. The **comparison** is this module's own, so a bug in
//! [`TokenBudget`](crate::passes::TokenBudget) cannot hide behind itself. Sharing the policy and
//! duplicating the mechanism is the arrangement that has neither of the two failure modes; the
//! version of this file that re-derived the allowances from config fields had read `force-if` alone
//! where the fail-safe read all four `force-*` keys, and had no reflow allowance at all.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use jals_config::fmt::{Config, ForceBraces, ImportOrder};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxToken};

use crate::comments::CommentMap;
use crate::passes::pipeline::{Formatted, Formatter};
use crate::passes::token_license::{License, Sites};
use crate::style::Style;

/// A significant-token multiset, counted rather than collected.
///
/// Counted because a *set* cannot see `import a.B; import a.B;` collapsing into one, which is
/// exactly the class of loss the token property exists to catch.
type Tokens = BTreeMap<(SyntaxKind, String), usize>;

/// The sources and configurations every property is checked against.
struct Corpus;

impl Corpus {
    /// Sources covering the shapes the rules disagree about.
    const SOURCES: &'static [&'static str] = &[
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
        // A grouped import whose trailing comma the dialect drops — the crate's one unconditional
        // token change. Paired with a body that is *obviously* misformatted on purpose: the drop
        // used to make the fail-safe reject the whole run, and a source that was already canonical
        // would have come back byte-identical either way and satisfied every property.
        "import a.{B,};\nclass  A  {  }\n",
        // The same construct with the comma separating something, so an allowance greedy enough to
        // swallow a separator shows up here rather than in the field.
        "import a.{B, C};\nclass  A  {  }\n",
        // Long enough for `reflow-long-strings` to fire, which the `gjf` profile turns on — so the
        // reflow path is walked here, and held to idempotence. It does *not* stand in for the
        // allowance: withdrawing the reflow row makes `Formatter::run` discard the rewrap and keep
        // the plain layout, which is still vouched for, so the loss is invisible from out here. What
        // pins the allowance is `token_budget`'s unit test, which asks `accepts` directly.
        "class T {\n  void m() {\n    throw new RuntimeException(\"a single very long literal that runs well past the hundred column limit and then some more\");\n  }\n}\n",
        // An over-long literal inside a formatter-disabled region, which only the `formatter-tags`
        // profile below actually disables. L4 runs after the lowering walk and over re-parsed text,
        // so it is the one stage that can still write into a region every earlier stage left alone.
        "class D {\n  // @formatter:off\n  String k = \"a single very long literal that runs well past the hundred column limit\";\n  // @formatter:on\n  int  y  =  1;\n}\n",
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

        // `formatter-tags` with a reflow on: the combination that reaches L4's own copy of the
        // disabled-region veto. Without the reflow the only stage that could write into a region is
        // the lowering walk, which has honored `OffOn` all along.
        let mut tagged = Config::default();
        tagged.layout.formatter_tags = true;
        tagged.wrapping.reflow_long_strings = true;

        alloc::vec![
            ("default", Config::default()),
            (
                "gjf",
                crate::import::GoogleJavaFormatConfig::default().into(),
            ),
            ("force-if-multiline", force),
            ("imports", grouped),
            ("formatter-tags", tagged),
        ]
    }

    /// Format once, keeping whether the formatter could vouch for the result.
    fn run(src: &str, config: &Config) -> Formatted {
        jals_exec::block_on_inline(async {
            let (style, _) = Style::reify(config, src);
            let parse = jals_syntax::Parse::parse(src).await;
            let errors = parse.errors().len();
            Formatter::run(&parse.syntax(), src, errors, &style).await
        })
    }

    /// Format once.
    fn format(src: &str, config: &Config) -> String {
        Self::run(src, config).text()
    }

    /// The license `config` grants, resolved the way the engine resolves it.
    fn license(src: &str, config: &Config) -> License {
        Style::reify(config, src).0.license
    }

    /// Whether `tok` sits inside a node of one of `scopes`.
    fn within(tok: &SyntaxToken, scopes: &[SyntaxKind]) -> bool {
        tok.parent_ancestors()
            .any(|node| scopes.contains(&node.kind()))
    }

    /// The significant tokens of `src` that `license` promises to leave alone, with their counts.
    ///
    /// Two exclusions, both read off the table rather than off a config field: a token some row can
    /// reach, and anything inside a scope some row may empty. What is left is the part of the file no
    /// operation is allowed to touch, so it is held to exact equality.
    fn untouched(src: &str, license: License) -> Tokens {
        Self::counted(src, license, |tok, license, sites, scopes| {
            !license.claims(tok, sites) && !Self::within(tok, scopes)
        })
    }

    /// The significant tokens of `src` inside a scope `license` lets a row empty, with their counts.
    ///
    /// The other half of [`untouched`](Self::untouched)'s second exclusion. Those tokens are outside
    /// exact equality because a row may *delete* them — but deletion is all it may do, so nothing may
    /// **appear** there, and that much is still checkable. Without it an import declaration's tokens
    /// leave this file's view entirely, and a token materializing inside one is left to the fail-safe
    /// alone.
    fn removable(src: &str, license: License) -> Tokens {
        Self::counted(src, license, |tok, _, _, scopes| Self::within(tok, scopes))
    }

    /// The significant tokens of `src` that `keep` selects, with their counts.
    ///
    /// The two selections share this walk so they cannot come apart on what a *significant* token is,
    /// or on how the table's scopes are resolved.
    fn counted(
        src: &str,
        license: License,
        keep: impl Fn(&SyntaxToken, License, &Sites, &[SyntaxKind]) -> bool,
    ) -> Tokens {
        let scopes: Vec<SyntaxKind> = license.removable_scopes().collect();
        let parse = jals_exec::block_on_inline(jals_syntax::Parse::parse(src));
        let root = parse.syntax();
        // The reflow row scopes its kinds to these nodes, so resolving them is part of asking the
        // table which tokens it claims — through the same `StringWrapper::sites` the pass reads.
        let sites = Sites::of(&root, license);
        let mut out = Tokens::new();
        for tok in root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| !tok.kind().is_trivia())
            .filter(|tok| keep(tok, license, &sites, &scopes))
        {
            *out.entry((tok.kind(), tok.text().to_owned())).or_insert(0) += 1;
        }
        out
    }

    /// The comment texts of `src`, optionally excluding the import block.
    fn comments(src: &str, imports: bool) -> Vec<String> {
        let parse = jals_exec::block_on_inline(jals_syntax::Parse::parse(src));
        parse
            .syntax()
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            // The crate's own definition of a comment, not a second list beside it: a fourth
            // comment kind would otherwise be one this property silently stopped checking.
            .filter(|tok| CommentMap::is_comment(tok.kind()))
            .filter(|tok| imports || !Self::within(tok, &[SyntaxKind::IMPORT_DECL]))
            .map(|tok| tok.text().trim().to_owned())
            .collect()
    }
}

#[test]
fn formatting_is_idempotent() {
    for (name, config) in Corpus::configurations() {
        for src in Corpus::SOURCES {
            let once = Corpus::format(src, &config);
            let twice = Corpus::format(&once, &config);
            assert_eq!(
                once, twice,
                "{name}: fmt is not idempotent on {src:?}\n--- once ---\n{once}\n--- twice ---\n{twice}",
            );
        }
    }
}

#[test]
fn the_fail_safe_never_fires_on_the_corpus() {
    // The progress property. Every other property here is a *preservation* property, and a total
    // fallback satisfies all of them at once — which is how a defect that handed back whole files
    // unformatted lived in a corpus that was checked four ways.
    for (name, config) in Corpus::configurations() {
        for src in Corpus::SOURCES {
            let outcome = Corpus::run(src, &config);
            assert!(
                matches!(outcome, Formatted::Vouched(_)),
                "{name}: the fail-safe rejected the output for {src:?}, so the whole file came back \
                 unformatted. Either a pass changed a token no row licenses, or a row is missing \
                 from `OPERATIONS`.",
            );
        }
    }
}

#[test]
fn the_public_output_reports_the_verdict_the_pipeline_reached() {
    // Not a property of the formatter — a check that the one bit callers act on is still wired to the
    // decision it names. `jals fmt --check` fails on a fallback, so a `vouched` that silently went
    // constant would put the CLI back to reporting a refused file as clean, which is the symptom the
    // whole fail-safe audit started from.
    for (name, config) in Corpus::configurations() {
        for src in Corpus::SOURCES {
            let internal = Corpus::run(src, &config);
            let public =
                jals_exec::block_on_inline(crate::FormatOutput::format_source(src, &config));
            assert_eq!(
                public.vouched,
                internal.vouched(),
                "{name}: `FormatOutput::vouched` disagrees with the pipeline for {src:?}",
            );
            assert_eq!(
                public.formatted,
                internal.text(),
                "{name}: `FormatOutput::formatted` is not the pipeline's text for {src:?}",
            );
        }
    }
}

#[test]
fn a_token_no_operation_claims_is_never_touched() {
    // The allowances come from the license, so this cannot be held to a rule the fail-safe does not
    // apply — nor miss one it does. The comparison is this module's own, so it is still an
    // independent witness rather than the fail-safe agreeing with itself.
    for (name, config) in Corpus::configurations() {
        for src in Corpus::SOURCES {
            let license = Corpus::license(src, &config);
            let formatted = Corpus::format(src, &config);
            assert_eq!(
                Corpus::untouched(src, license),
                Corpus::untouched(&formatted, license),
                "{name}: a token no row claims changed for {src:?}\n--- output ---\n{formatted}",
            );

            // A `RemovesSubtrees` row licenses *deletion* inside its scope and nothing else, so what
            // survives there has to be a sub-multiset of what went in. Exact equality is not
            // available — the row may empty the scope — but "appeared from nowhere" still is.
            let given = Corpus::removable(src, license);
            for (token, count) in Corpus::removable(&formatted, license) {
                assert!(
                    given.get(&token).copied().unwrap_or(0) >= count,
                    "{name}: {token:?} appeared inside a removable scope for {src:?}\n\
                     --- output ---\n{formatted}",
                );
            }
        }
    }
}

#[test]
fn no_comment_is_ever_dropped() {
    for (name, config) in Corpus::configurations() {
        // A profile that deletes unused imports takes the comments attached to them with it, so the
        // import block is excluded rather than the whole profile being skipped — the rest of the
        // file is still held to exact comment preservation.
        let scope = !config.imports.remove_unused;
        for src in Corpus::SOURCES {
            let formatted = Corpus::format(src, &config);
            assert_eq!(
                Corpus::comments(src, scope),
                Corpus::comments(&formatted, scope),
                "{name}: a comment was dropped or duplicated for {src:?}\n--- output ---\n{formatted}",
            );
        }
    }
}

#[test]
fn malformed_input_is_never_lost() {
    let config = Config::default();
    for src in Corpus::SOURCES {
        let formatted = Corpus::format(src, &config);
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
