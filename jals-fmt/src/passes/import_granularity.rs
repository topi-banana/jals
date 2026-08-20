//! R0.4 — import re-granulation, as a plan the compilation-unit visitor emits.
//!
//! `[imports] granularity` decides how many types one declaration names, over the jals dialect's
//! grouped import (`import java.util.{HashMap, List};`). It is the Java answer to rustfmt's
//! `imports_granularity`, and like that option it is a *rewrite*, not a reordering: splitting a
//! group gains an `import`, a `;` and a copy of the shared prefix, and merging loses them.
//!
//! # Plan, then emit — but the plan is not a permutation
//!
//! [`ImportPlan`](super::ImportPlan) can promise a preserved token multiset because it only
//! reorders the original nodes. This one cannot, so it carries the second half of the promise
//! instead: whatever it does, **the set of types the import block names is unchanged**.
//! [`ImportNames`] is that set, and it is the predicate the fail-safe shares — the same
//! one-predicate-two-callers rule the trailing comma and the reflow sites already follow
//! (`token_license`'s module doc). A rebuild that concatenates a prefix wrong therefore shows up
//! as an import of a type the input never named, rather than as a diff nobody checks.
//!
//! # What it will not touch
//!
//! - A **wildcard** import (`import a.*;`) is never merged. It names no type, so it has no entry
//!   to contribute, and a group would have to hold a bare `*` member to represent it.
//! - A **module** import (`import module M;`) is never merged or split: it has no members.
//! - A declaration carrying a jals **attribute** (`#[cfg(feature = "x")] import a.{B, C};`) is
//!   neither merged nor split. An attribute is a *condition on the declaration*, and re-cutting
//!   would redistribute it: splitting leaves the promoted members ungated, merging puts the first
//!   declaration's condition over everyone else's members. Both change what the file compiles to,
//!   and neither is visible to the fail-safe — the attribute's tokens sit inside the `IMPORT_DECL`
//!   the row already waives, and [`ImportNames`] answers about types, not conditions.
//! - Merging only joins declarations that are **already adjacent** in the emission order and share
//!   a prefix. Making non-adjacent ones adjacent is `[imports] order`'s job, and doing it here
//!   would silently reorder a block the user asked to preserve.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;

use jals_config::fmt::ImportGranularity;
use jals_syntax::ast::{AstNode, ImportDecl, QualifiedName};
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode};

/// Every type an import declaration names, fully qualified.
///
/// The predicate `[imports] granularity` rewrites by and [`TokenBudget`](super::TokenBudget)
/// checks against. Two implementations of "which types does this declaration name" is exactly the
/// drift `token_license` exists to prevent, so there is one.
pub(crate) mod import_names {
    use super::{AstNode, ImportDecl, S, String, SyntaxElement, SyntaxNode, ToOwned, Vec};

    /// The declarations `decl` desugars to, each spelled `[static ]<fully qualified name>`.
    ///
    /// One entry for a plain import, one per member for a grouped one, and none for a module
    /// import — which names a module rather than a type, and so has nothing a re-granulation could
    /// move.
    pub(crate) fn of(decl: &ImportDecl) -> Vec<String> {
        if decl.is_module() {
            return Vec::new();
        }
        let lead = if decl.is_static() { "static " } else { "" };
        let Some(name) = decl.name() else {
            return Vec::new();
        };
        let name = text_of(name.syntax());
        decl.group().map_or_else(
            || alloc::vec![alloc::format!("{lead}{name}")],
            |group| {
                group
                    .members()
                    .map(|member| {
                        let member = text_of(member.syntax());
                        alloc::format!("{lead}{name}.{member}")
                    })
                    .collect()
            },
        )
    }

    /// Whether a declaration carries a jals attribute, so re-cutting it would redistribute a
    /// condition rather than a layout.
    pub(crate) fn is_conditional(decl: &ImportDecl) -> bool {
        decl.syntax()
            .children()
            .any(|child| child.kind() == S::ATTRIBUTE)
    }

    /// A node's significant tokens, concatenated — no whitespace, no comments.
    pub(crate) fn text_of(node: &SyntaxNode) -> String {
        node.descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| !tok.kind().is_trivia())
            .map(|tok| tok.text().to_owned())
            .collect()
    }

    /// The package prefix a declaration's members share: the whole name for a grouped import, the
    /// qualifier for a plain one.
    ///
    /// `None` for a declaration with no prefix to share — an unqualified or wildcard import, or a
    /// module import — which is therefore never merged with anything.
    pub(crate) fn prefix(decl: &ImportDecl) -> Option<String> {
        if decl.is_module() || is_conditional(decl) {
            return None;
        }
        let name = decl.name()?;
        if decl.group().is_some() {
            return Some(text_of(name.syntax()));
        }
        if name.is_wildcard() {
            return None;
        }
        name.qualifier()
    }
}

/// One declaration as the import block will emit it.
///
/// [`Whole`](Self::Whole) is the only variant that reaches the ordinary token path unchanged; the
/// other two are why `[imports] granularity` has a row in `token_license`.
#[derive(Clone)]
pub(crate) enum Unit {
    /// A source declaration, emitted exactly as written.
    Whole(SyntaxNode),
    /// One member of a grouped import, promoted to a plain declaration of its own.
    ///
    /// `lead` marks the member that inherits the declaration's *real* leading tokens; every later
    /// member of the same group emits synthetic copies, so each source token is emitted once and
    /// its comments travel with it.
    Split {
        decl: SyntaxNode,
        member: SyntaxNode,
        lead: bool,
    },
    /// Several adjacent declarations sharing a prefix, emitted as one grouped import.
    Merge(Vec<SyntaxNode>),
}

impl Unit {
    /// The source declaration this unit stands at, when it stands at exactly one.
    ///
    /// `None` for a unit the cut *added* — a second member promoted out of a group, or a merge of
    /// several declarations. Such a unit has no single node whose leading blank lines it could
    /// inherit, so the caller falls back to the enforced count.
    pub(crate) const fn source(&self) -> Option<&SyntaxNode> {
        match self {
            Self::Whole(node) => Some(node),
            Self::Split {
                decl, lead: true, ..
            } => Some(decl),
            Self::Split { lead: false, .. } | Self::Merge(_) => None,
        }
    }
}

/// The re-granulation of an already-ordered import block.
pub(crate) mod granularity {
    use super::{
        AstNode, ImportDecl, ImportGranularity, String, SyntaxNode, Unit, Vec, import_names,
    };

    /// Re-cut an ordered import block to `granularity`, preserving its order.
    ///
    /// Each input is a declaration with the blank lines that precede it. A unit the cut *adds*
    /// takes no blank lines of its own, and a separation is never merged away: a blank line is a
    /// group boundary `[imports] order` drew, and joining across one would undo it.
    pub(crate) fn apply(
        block: Vec<(SyntaxNode, usize)>,
        granularity: ImportGranularity,
    ) -> Vec<(Unit, usize)> {
        match granularity {
            ImportGranularity::Preserve => block
                .into_iter()
                .map(|(node, blanks)| (Unit::Whole(node), blanks))
                .collect(),
            ImportGranularity::Item => split(block),
            ImportGranularity::Package => merge(block),
        }
    }

    /// One declaration per member of every grouped import; everything else untouched.
    fn split(block: Vec<(SyntaxNode, usize)>) -> Vec<(Unit, usize)> {
        let mut out = Vec::with_capacity(block.len());
        for (node, blanks) in block {
            let members: Vec<SyntaxNode> = ImportDecl::cast(node.clone())
                .filter(|decl| !import_names::is_conditional(decl))
                .and_then(|decl| decl.group())
                .map(|group| {
                    group
                        .members()
                        .map(|member| member.syntax().clone())
                        .collect()
                })
                .unwrap_or_default();
            // A group with no members is `import a.{};` — nothing to promote, and dropping it
            // would lose the declaration entirely.
            if members.is_empty() {
                out.push((Unit::Whole(node), blanks));
                continue;
            }
            for (nth, member) in members.into_iter().enumerate() {
                out.push((
                    Unit::Split {
                        decl: node.clone(),
                        member,
                        lead: nth == 0,
                    },
                    if nth == 0 { blanks } else { 0 },
                ));
            }
        }
        out
    }

    /// Maximal runs of adjacent declarations sharing a prefix, joined into one grouped import.
    fn merge(block: Vec<(SyntaxNode, usize)>) -> Vec<(Unit, usize)> {
        let mut out: Vec<(Unit, usize)> = Vec::with_capacity(block.len());
        let mut run: Vec<SyntaxNode> = Vec::new();
        let mut key: Option<(bool, String)> = None;
        let mut lead = 0usize;

        for (node, blanks) in block {
            let current = ImportDecl::cast(node.clone())
                .and_then(|decl| import_names::prefix(&decl).map(|at| (decl.is_static(), at)));
            let joins = blanks == 0 && current.is_some() && key == current && !run.is_empty();
            if joins {
                run.push(node);
                continue;
            }
            flush(&mut out, core::mem::take(&mut run), lead);
            if current.is_some() {
                key = current;
                lead = blanks;
                run.push(node);
            } else {
                key = None;
                out.push((Unit::Whole(node), blanks));
            }
        }
        flush(&mut out, run, lead);
        out
    }

    /// Emit a finished run: a group when it joins two or more declarations, otherwise the
    /// declaration as written.
    ///
    /// A run of one is deliberately *not* wrapped: `import a.B;` becoming `import a.{B};` adds
    /// braces to say nothing, and would grow a one-member group on every plain import in the file.
    fn flush(out: &mut Vec<(Unit, usize)>, run: Vec<SyntaxNode>, lead: usize) {
        match run.len() {
            0 => {}
            1 => out.push((
                Unit::Whole(run.into_iter().next().expect("length checked")),
                lead,
            )),
            _ => out.push((Unit::Merge(run), lead)),
        }
    }
}

/// The parts of a declaration a re-granulation emits separately.
pub(crate) mod parts {
    use super::{AstNode, ImportDecl, QualifiedName, S, String, SyntaxElement, SyntaxNode, Vec};

    /// The declaration's children up to and including its prefix name — attributes, `import`, the
    /// `static` / `module` keywords, and the [`QualifiedName`].
    pub(crate) fn lead(decl: &SyntaxNode) -> Vec<SyntaxElement> {
        decl.children_with_tokens()
            .take_while(|child| child.kind() != S::IMPORT_GROUP && child.kind() != S::SEMICOLON)
            .collect()
    }

    /// Everything after the prefix: the group's delimiters and the terminating `;`.
    ///
    /// These are the tokens a split or a merge drops. They are handed to the visitor rather than
    /// silently skipped so their comments can still be emitted.
    pub(crate) fn tail(decl: &SyntaxNode) -> Vec<SyntaxElement> {
        decl.children_with_tokens()
            .skip_while(|child| child.kind() != S::IMPORT_GROUP && child.kind() != S::SEMICOLON)
            .collect()
    }

    /// The member text a plain declaration contributes to a merged group: its last segment.
    pub(crate) fn last_segment(decl: &SyntaxNode) -> Option<String> {
        ImportDecl::cast(decl.clone())
            .and_then(|decl| decl.name())
            .as_ref()
            .and_then(QualifiedName::last_segment)
    }
}

#[cfg(test)]
mod tests {
    use jals_config::fmt::{Config, ImportGranularity};
    use jals_config::{Feature, FeatureSet};

    /// Format `src` with only `[imports] granularity` moved, in a project enabling the dialect.
    fn cut(src: &str, granularity: ImportGranularity) -> crate::FormatOutput {
        let mut cfg = Config::default();
        cfg.imports.granularity = granularity;
        let out = jals_exec::block_on_inline(crate::FormatOutput::format_source(
            src,
            &cfg,
            FeatureSet::resolve(&[Feature::GroupedImports]),
        ));
        assert!(
            !out.fell_back(),
            "the fail-safe refused its own output, so nothing was formatted:\n{}",
            out.formatted,
        );
        out
    }

    #[test]
    fn a_group_splits_into_one_declaration_per_member() {
        let out = cut(
            "import java.util.{HashMap, List};\nclass Z {}\n",
            ImportGranularity::Item,
        );
        assert!(
            out.formatted.contains("import java.util.HashMap;"),
            "{}",
            out.formatted
        );
        assert!(
            out.formatted.contains("import java.util.List;"),
            "{}",
            out.formatted
        );
        assert!(!out.formatted.contains('{') || out.formatted.contains("class Z"));
    }

    #[test]
    fn a_nested_member_keeps_its_own_qualifier_when_split() {
        // The member is `regex.Pattern`, so the promoted declaration is the *concatenation* of the
        // prefix and the member — the rebuild `Content::ImportedNames` exists to check.
        let out = cut(
            "import java.util.{regex.Pattern, concurrent.*};\nclass Z {}\n",
            ImportGranularity::Item,
        );
        assert!(
            out.formatted.contains("import java.util.regex.Pattern;"),
            "{}",
            out.formatted
        );
        assert!(
            out.formatted.contains("import java.util.concurrent.*;"),
            "{}",
            out.formatted
        );
    }

    #[test]
    fn adjacent_imports_of_one_package_merge_into_a_group() {
        let out = cut(
            "import java.util.HashMap;\nimport java.util.List;\nclass Z {}\n",
            ImportGranularity::Package,
        );
        assert!(
            out.formatted.contains("import java.util.{HashMap, List};"),
            "{}",
            out.formatted
        );
    }

    #[test]
    fn a_lone_import_never_grows_a_one_member_group() {
        // `flush`'s rule: braces that say nothing are not added, and adding them would make every
        // plain import in the file grow a group.
        let out = cut(
            "import java.util.List;\nimport java.io.File;\nclass Z {}\n",
            ImportGranularity::Package,
        );
        assert!(!out.formatted.contains('{') || out.formatted.contains("class Z"));
        assert!(
            out.formatted.contains("import java.util.List;")
                && out.formatted.contains("import java.io.File;"),
            "{}",
            out.formatted
        );
    }

    #[test]
    fn a_wildcard_is_never_merged() {
        let out = cut(
            "import java.util.*;\nimport java.util.List;\nclass Z {}\n",
            ImportGranularity::Package,
        );
        assert!(
            out.formatted.contains("import java.util.*;"),
            "{}",
            out.formatted
        );
    }

    #[test]
    fn a_static_import_never_merges_with_a_plain_one() {
        let out = cut(
            "import static java.lang.Math.PI;\nimport java.lang.Math.E;\nclass Z {}\n",
            ImportGranularity::Package,
        );
        assert!(
            out.formatted.contains("import static java.lang.Math.PI;"),
            "{}",
            out.formatted
        );
    }

    #[test]
    fn splitting_then_merging_returns_the_block_it_started_from() {
        let split = cut(
            "import java.util.{HashMap, List};\nclass Z {}\n",
            ImportGranularity::Item,
        );
        let merged = cut(&split.formatted, ImportGranularity::Package);
        assert!(
            merged
                .formatted
                .contains("import java.util.{HashMap, List};"),
            "{}",
            merged.formatted
        );
    }

    #[test]
    fn merging_is_refused_when_the_project_has_not_enabled_the_dialect() {
        // The hazard the feature set exists for: the parser is lossless and the fail-safe is
        // satisfied, so nothing downstream of the formatter would notice a grouped import in a
        // vanilla project — `javac` would.
        let mut cfg = Config::default();
        cfg.imports.granularity = ImportGranularity::Package;
        let out = jals_exec::block_on_inline(crate::FormatOutput::format_source(
            "import java.util.HashMap;\nimport java.util.List;\nclass Z {}\n",
            &cfg,
            FeatureSet::default(),
        ));
        assert!(
            out.formatted.contains("import java.util.HashMap;")
                && out.formatted.contains("import java.util.List;"),
            "{}",
            out.formatted
        );
        assert!(
            out.warnings
                .iter()
                .any(|warning| warning.message.contains("grouped-imports")),
            "the rounding has to be reported, or it looks like it took effect",
        );
    }

    #[test]
    fn splitting_is_allowed_without_the_dialect() {
        // The other direction only ever *removes* dialect syntax, so a project that never enabled
        // it is exactly where splitting is most useful.
        let mut cfg = Config::default();
        cfg.imports.granularity = ImportGranularity::Item;
        let out = jals_exec::block_on_inline(crate::FormatOutput::format_source(
            "import java.util.{HashMap, List};\nclass Z {}\n",
            &cfg,
            FeatureSet::default(),
        ));
        assert!(!out.fell_back(), "{}", out.formatted);
        assert!(
            out.formatted.contains("import java.util.HashMap;"),
            "{}",
            out.formatted
        );
    }
}

#[cfg(test)]
mod attribute_tests {
    use jals_config::fmt::{Config, ImportGranularity};
    use jals_config::{Feature, FeatureSet};

    fn cut(src: &str, granularity: ImportGranularity) -> String {
        let mut cfg = Config::default();
        cfg.imports.granularity = granularity;
        let out = jals_exec::block_on_inline(crate::FormatOutput::format_source(
            src,
            &cfg,
            FeatureSet::resolve(&[Feature::GroupedImports, Feature::Attributes]),
        ));
        assert!(!out.fell_back(), "the fail-safe refused its own output");
        out.formatted
    }

    #[test]
    fn an_attributed_group_is_not_split() {
        // Splitting would leave every member but the first ungated — a change to what the file
        // compiles to, and one no token or name check can see.
        let src = "#[cfg(feature = \"x\")]\nimport java.util.{HashMap, List};\nclass Z {}\n";
        let out = cut(src, ImportGranularity::Item);
        assert!(out.contains("{HashMap, List}"), "{out}");
        assert_eq!(out.matches("import ").count(), 1, "{out}");
    }

    #[test]
    fn an_attributed_import_is_not_merged() {
        // Merging would put the first declaration's condition over the second one's member.
        let src = "#[cfg(feature = \"x\")]\nimport java.util.HashMap;\nimport java.util.List;\nclass Z {}\n";
        let out = cut(src, ImportGranularity::Package);
        assert!(out.contains("import java.util.HashMap;"), "{out}");
        assert!(out.contains("import java.util.List;"), "{out}");
        assert!(!out.contains('{') || out.contains("class Z"), "{out}");
    }

    #[test]
    fn an_attribute_on_the_second_import_also_blocks_the_merge() {
        let src = "import java.util.HashMap;\n#[cfg(feature = \"x\")]\nimport java.util.List;\nclass Z {}\n";
        let out = cut(src, ImportGranularity::Package);
        assert!(out.contains("import java.util.HashMap;"), "{out}");
        assert!(out.contains("import java.util.List;"), "{out}");
    }
}
