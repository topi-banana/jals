//! The fail-safe: does the output still hold the input's tokens?
//!
//! Formatting must never silently damage a file. After rendering, the output is re-parsed and its
//! significant tokens are compared against the input's. If the comparison fails, the **input is
//! returned unchanged** — a file the formatter cannot handle is better left alone than corrupted.
//!
//! # Why this is not "no tokens were lost"
//!
//! The naive check would defeat half the feature set. `imports.remove-unused` deletes declarations
//! by design; `[literals]` rewrites a token's spelling; `[braces] force-*` inserts braces. Each is
//! a declared exception, and the declarations live in
//! [`token_license`](super::token_license) — `DESIGN.md` §20's table as data.
//!
//! This module reads **only** a [`License`]. It does not see [`Config`](jals_config::fmt::Config),
//! and that is the point: reconstructing "what was allowed" from config fields is what let an
//! operation with no config key slip through unlicensed. Adding a token-changing pass means adding
//! a row to the table, not a branch here.
//!
//! The new-syntax-error half of the check is unconditional: no rule may make a file stop parsing.

use crate::passes::import_granularity;
use crate::passes::string_wrapper;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use jals_syntax::ast::{AstNode, ImportDecl};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use super::token_license::{Content, Lane, License, Sites};

/// A significant-token multiset, keyed by the lane that answers for each token.
///
/// The lane comes first so the lanes are disjoint partitions of the key space: [`Ledger::covers`]
/// dispatches on it and gives each one its own rule in a single pass.
type Budget = BTreeMap<(Lane, SyntaxKind, String), usize>;

/// One tree, reduced to everything the comparison needs.
struct Ledger {
    /// Every counted token, by lane.
    flat: Budget,
    /// What the tokens a scoped allowance removed from the count must nonetheless still spell.
    concatenations: String,
    text_blocks: String,
    /// Every type the import block names, fully qualified — sorted, so the comparison is a
    /// multiset one and re-ordering the block is not a difference.
    imported_names: Vec<String>,
}

impl Ledger {
    /// Reduce `root` under `license`.
    fn of(root: &SyntaxNode, license: License) -> Self {
        let sites = Sites::of(root, license);
        let mut flat = Budget::new();
        for tok in root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| !tok.kind().is_trivia())
        {
            let lane = license.lane(&tok, &sites);
            if matches!(lane, Lane::Redistributed(_)) {
                // Not counted at all: the pass may add and remove these, so only the content check
                // can answer for them.
                continue;
            }
            // A respelled kind drops its text, and *only* that kind does. Emptying every kind's
            // text — one flag for the whole file — hid a renamed identifier too.
            let text = if license.respells(tok.kind()) {
                String::new()
            } else {
                tok.text().into()
            };
            *flat.entry((lane, tok.kind(), text)).or_insert(0) += 1;
        }
        Self {
            flat,
            concatenations: sites.into_content(),
            // Only gathered when a row scoped a text block's spelling out of the count. Otherwise
            // `covers` never reads it, and collecting it would walk the whole tree a second time to
            // build a string nothing compares.
            text_blocks: if license.checks(Content::TextBlocks) {
                Self::text_block_content(root)
            } else {
                String::new()
            },
            imported_names: if license.checks(Content::ImportedNames) {
                Self::imported_names(root)
            } else {
                Vec::new()
            },
        }
    }

    /// Every text block's body, in source order, separated.
    ///
    /// The one thing re-indenting a text block may not change: what it spells once its incidental
    /// whitespace is stripped.
    fn text_block_content(root: &SyntaxNode) -> String {
        root.descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| tok.kind() == SyntaxKind::TEXT_BLOCK)
            .map(|tok| string_wrapper::text_block_content(tok.text()))
            .collect::<Vec<_>>()
            .join("\u{1}")
    }

    /// Every type the import block names, fully qualified and sorted.
    ///
    /// A grouped import contributes one entry per member, spelled as the plain declaration it
    /// desugars to — which is exactly what makes merging and splitting invisible here and a
    /// mis-rebuilt prefix visible. `static` is part of the entry: `import static a.B;` and
    /// `import a.B;` are different declarations, and a re-granulation must not turn one into the
    /// other.
    fn imported_names(root: &SyntaxNode) -> Vec<String> {
        let mut names: Vec<String> = root
            .descendants()
            .filter_map(ImportDecl::cast)
            .flat_map(|decl| import_granularity::import_names::of(&decl))
            .collect();
        names.sort_unstable();
        names
    }

    /// Whether `after` is a rendering of `self` that `license` authorizes.
    fn covers(&self, after: &Self, license: License) -> bool {
        // Both key sets are walked, so a key present on only one side is judged rather than
        // skipped.
        for key in self.flat.keys().chain(after.flat.keys()) {
            let before = self.flat.get(key).copied().unwrap_or(0);
            let now = after.flat.get(key).copied().unwrap_or(0);
            let ok = match key.0 {
                Lane::Exact => now == before,
                Lane::Insertable(_) => now >= before,
                Lane::Removable(_) => now <= before,
                // Never entered `flat`.
                Lane::Redistributed(_) => true,
            };
            if !ok {
                return false;
            }
        }

        // What the scoped-out tokens must nonetheless still spell.
        if license.checks(Content::Concatenations) && self.concatenations != after.concatenations {
            return false;
        }
        if license.checks(Content::TextBlocks) && self.text_blocks != after.text_blocks {
            return false;
        }
        // Subset, not equality — the reason is [`Content::ImportedNames`]' own. Both sides are
        // sorted, so this is one merge walk rather than a quadratic containment test.
        if license.checks(Content::ImportedNames)
            && !Self::is_submultiset(&after.imported_names, &self.imported_names)
        {
            return false;
        }
        true
    }

    /// Whether every entry of `part` appears in `whole` at least as often. Both must be sorted.
    fn is_submultiset(part: &[String], whole: &[String]) -> bool {
        let mut rest = whole;
        for name in part {
            match rest.iter().position(|candidate| candidate == name) {
                Some(at) => rest = &rest[at + 1..],
                None => return false,
            }
        }
        true
    }
}

/// Verifies that a formatted output still accounts for its input.
pub(crate) mod budget {
    use super::{Ledger, License, SyntaxNode};

    /// Whether `formatted` is a rendering of `src` that `license` authorizes.
    pub(crate) async fn accepts(
        src: &str,
        src_tree: &SyntaxNode,
        src_errors: usize,
        formatted: &str,
        license: License,
    ) -> bool {
        // An output that no longer parses is never acceptable, whatever the input's own state.
        let reparsed = jals_syntax::Parse::parse(formatted).await;
        if reparsed.errors().len() > src_errors {
            return false;
        }
        // A formatter that produced nothing from a non-empty file has lost everything.
        if formatted.trim().is_empty() && !src.trim().is_empty() {
            return false;
        }

        let before = Ledger::of(src_tree, license);
        let after = Ledger::of(&reparsed.syntax(), license);
        before.covers(&after, license)
    }
}

#[cfg(test)]
mod tests {
    use super::budget;
    use jals_config::fmt::{Config, ForceBraces, HexLiteralCase};

    use crate::style::Style;

    /// One `accepts` call, spelled the way [`pipeline::run`](crate::passes::Formatter) spells it.
    mod verdict {
        use super::*;

        /// Whether `formatted` is an acceptable rendering of `src` under `config`.
        pub(crate) fn of(src: &str, formatted: &str, config: &Config) -> bool {
            jals_exec::block_on_inline(async {
                let (style, _) = Style::reify(config, src, jals_config::FeatureSet::default());
                let parse = jals_syntax::Parse::parse(src).await;
                let errors = parse.errors().len();
                budget::accepts(src, &parse.syntax(), errors, formatted, style.license).await
            })
        }

        /// A config with only `braces.force-do-while` moved off its default.
        pub(crate) fn only_force_do_while() -> Config {
            let mut cfg = Config::default();
            cfg.braces.force_do_while = ForceBraces::Always;
            cfg
        }

        /// A config with only `imports.remove-unused` moved off its default.
        pub(crate) fn removing_unused_imports() -> Config {
            let mut cfg = Config::default();
            cfg.imports.remove_unused = true;
            cfg
        }

        /// A config with only `[literals] hex-case` moved off its default.
        pub(crate) fn respelling_literals() -> Config {
            let mut cfg = Config::default();
            cfg.literals.hex_case = HexLiteralCase::Upper;
            cfg
        }

        /// A config with only `wrapping.reflow-long-strings` moved off its default.
        pub(crate) fn reflowing_strings() -> Config {
            let mut cfg = Config::default();
            cfg.wrapping.reflow_long_strings = true;
            cfg
        }
    }

    // ===== Unconditional halves: no config can waive these =====

    #[test]
    fn an_output_that_stopped_parsing_is_never_acceptable() {
        assert!(!verdict::of(
            "class A { int x; }",
            "class A { int x;",
            &Config::default(),
        ));
    }

    #[test]
    fn emptying_a_nonempty_file_is_never_acceptable() {
        assert!(!verdict::of("class A {}", "", &Config::default()));
    }

    #[test]
    fn an_empty_input_may_stay_empty() {
        assert!(verdict::of("", "", &Config::default()));
    }

    // ===== `DESIGN.md` §20 R0.1 / R0.3: reordering is multiset-neutral =====

    #[test]
    fn reordering_imports_is_accepted_because_the_check_is_a_multiset() {
        assert!(verdict::of(
            "import b.C;\nimport a.B;\nclass A {}\n",
            "import a.B;\nimport b.C;\nclass A {}\n",
            &Config::default(),
        ));
    }

    #[test]
    fn reordering_modifiers_is_accepted_for_the_same_reason() {
        assert!(verdict::of(
            "class A { final static public int X = 1; }",
            "class A { public static final int X = 1; }",
            &Config::default(),
        ));
    }

    #[test]
    fn a_token_lost_under_the_default_config_is_rejected() {
        assert!(!verdict::of(
            "class A { int x; int y; }",
            "class A { int x; }",
            &Config::default(),
        ));
    }

    // ===== `[literals]`: compare by kind, since a literal's text may change =====

    #[test]
    fn a_respelled_hex_literal_is_accepted_when_literals_is_active() {
        assert!(verdict::of(
            "class A { int x = 0xff; }",
            "class A { int x = 0xFF; }",
            &verdict::respelling_literals(),
        ));
    }

    #[test]
    fn the_same_respelling_is_rejected_when_literals_is_preserve() {
        assert!(!verdict::of(
            "class A { int x = 0xff; }",
            "class A { int x = 0xFF; }",
            &Config::default(),
        ));
    }

    #[test]
    fn a_renamed_identifier_is_rejected_even_when_literals_is_active() {
        // The guard for scoping the respelling to the two kinds `literals::apply` can touch.
        // A single by-kind flag for the whole file made *every* token's spelling unverifiable the
        // moment any `[literals]` rule was switched on, so a renamed identifier went unnoticed.
        assert!(!verdict::of(
            "class A { int counted = 0xff; }",
            "class A { int renamed = 0xff; }",
            &verdict::respelling_literals(),
        ));
    }

    #[test]
    fn a_respelled_string_is_rejected_even_when_literals_is_active() {
        // Same scoping, the other kind a whole-file flag used to blind: no row licenses a string's
        // spelling unless a reflow is on, and then only what it *spells* may survive re-cutting.
        assert!(!verdict::of(
            "class A { String s = \"a\"; int x = 0xff; }",
            "class A { String s = \"b\"; int x = 0xFF; }",
            &verdict::respelling_literals(),
        ));
    }

    #[test]
    fn a_text_block_that_vanished_is_rejected_even_under_reflow() {
        // `reflow-long-strings` licenses re-indenting a text block, not losing one. Folding the
        // re-indent into the rewrap's row took `TEXT_BLOCK` out of the count altogether, leaving a
        // whole-file content string as the only witness; as its own respelling row the count is
        // checked again.
        assert!(!verdict::of(
            "class A { String a = \"\"\"\n    x\n    \"\"\"; String b = \"\"\"\n    x\n    \"\"\"; }",
            "class A { String a = \"\"\"\n    x\n    \"\"\"; String b = \"\"; }",
            &verdict::reflowing_strings(),
        ));
    }

    #[test]
    fn a_literal_that_became_another_kind_is_rejected_even_by_kind() {
        assert!(!verdict::of(
            "class A { int x = 1; }",
            "class A { int x = \"s\"; }",
            &verdict::respelling_literals(),
        ));
    }

    // ===== `DESIGN.md` §20 R0.2: unused-import removal deletes declarations =====

    #[test]
    fn remove_unused_accepts_a_dropped_import() {
        assert!(verdict::of(
            "import a.B;\nclass A {}\n",
            "class A {}\n",
            &verdict::removing_unused_imports(),
        ));
    }

    #[test]
    fn remove_unused_still_rejects_a_token_lost_outside_the_import_block() {
        assert!(!verdict::of(
            "import a.B;\nclass A { int x; int y; }\n",
            "import a.B;\nclass A { int x; }\n",
            &verdict::removing_unused_imports(),
        ));
    }

    // ===== `[braces] force-*`: the only rule that adds tokens =====

    #[test]
    fn force_do_while_alone_accepts_the_braces_it_inserts() {
        // The guard for `forces_braces`' four-way OR. The invariant properties used to read
        // `force_if` alone, so a profile that set only this key held the formatter to an allowance
        // it does not have; they read the license now, and this is what keeps the license honest.
        assert!(verdict::of(
            "class A { void m() { do x(); while (c); } }",
            "class A { void m() { do { x(); } while (c); } }",
            &verdict::only_force_do_while(),
        ));
    }

    #[test]
    fn a_brace_is_still_never_allowed_to_go_missing() {
        assert!(!verdict::of(
            "class A { void m() { do { x(); } while (c); } }",
            "class A { void m() { do x(); while (c); } }",
            &verdict::only_force_do_while(),
        ));
    }

    #[test]
    fn extra_braces_are_rejected_when_no_force_rule_is_on() {
        let mut cfg = Config::default();
        cfg.braces.force_switch_arm = jals_config::fmt::ForceBraces::Never;
        assert!(!verdict::of(
            "class A { void m() { do x(); while (c); } }",
            "class A { void m() { do { x(); } while (c); } }",
            &cfg,
        ));
    }

    // ===== `DESIGN.md` §20 R4.1: a reflow re-cuts the pieces, so content is compared =====

    #[test]
    fn reflow_accepts_a_concatenation_re_split_at_other_boundaries() {
        assert!(verdict::of(
            "class A { String s = \"ab\" + \"c\"; }",
            "class A { String s = \"a\" + \"bc\"; }",
            &verdict::reflowing_strings(),
        ));
    }

    #[test]
    fn reflow_rejects_a_concatenation_whose_content_changed() {
        assert!(!verdict::of(
            "class A { String s = \"ab\" + \"c\"; }",
            "class A { String s = \"a\" + \"bd\"; }",
            &verdict::reflowing_strings(),
        ));
    }

    #[test]
    fn the_same_re_split_is_rejected_when_reflow_is_off() {
        assert!(!verdict::of(
            "class A { String s = \"ab\" + \"c\"; }",
            "class A { String s = \"a\" + \"bc\"; }",
            &Config::default(),
        ));
    }

    // ===== The dialect's grouped-import trailing comma =====

    #[test]
    fn the_dialect_comma_drop_is_licensed_under_every_config() {
        // `visit/dialect.rs` drops this comma unconditionally, so the row that licenses it is
        // unconditional too. Before the row existed the fail-safe rejected the drop and the whole
        // file came back unformatted — and only under the *default* config, because
        // `imports.remove-unused` happened to waive the import block when it was on.
        for cfg in [Config::default(), verdict::removing_unused_imports()] {
            assert!(
                verdict::of(
                    "import a.{B,};\nclass A {}\n",
                    "import a.{B};\nclass A {}\n",
                    &cfg,
                ),
                "the drop must be licensed by its own row, not by another operation's allowance",
            );
        }
    }

    #[test]
    fn the_comma_licence_does_not_reach_a_comma_outside_a_grouped_import() {
        // The site predicate is what keeps the allowance from spreading. An argument list's comma
        // is not a grouped import's trailing comma, whatever the config says.
        assert!(!verdict::of(
            "class A { void m() { f(1, 2); } }",
            "class A { void m() { f(1 2); } }",
            &Config::default(),
        ));
    }

    #[test]
    fn a_separator_comma_may_never_go_missing() {
        // The guard against a filter greedy enough to eat a separator once the trailing comma is
        // licensed.
        assert!(!verdict::of(
            "import a.{B, C};\nclass A {}\n",
            "import a.{B C};\nclass A {}\n",
            &Config::default(),
        ));
    }
}
