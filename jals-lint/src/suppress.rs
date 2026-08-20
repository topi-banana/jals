//! In-source suppression: `@SuppressWarnings`, the way Java spells "not here".
//!
//! A `jalslint.toml` levels a rule for a whole directory tree. That is the wrong granularity for
//! the one call site where a rule is wrong, and every linter that ships rules with false positives
//! needs the other one — which is why `jals-lint/README.md` § Roadmap makes this the first item,
//! ahead of any individual rule.
//!
//! # What a name may be
//!
//! Three vocabularies, and all three are **derived from the registry** rather than listed here:
//!
//! - `"all"` — every rule, which is javac's spelling for the same thing.
//! - a rule name — `"unused-variables"`, the same string the diagnostic carries and the same key
//!   the `jalslint.toml` section declares.
//! - a section name — `"unused"`, a whole [`Category`](jals_config::Category).
//!
//! Deriving them is the point: a rule or a section added later is suppressible the day it lands,
//! with no second list to keep in step. It also buys the compatibility that matters most here for
//! free — javac's `@SuppressWarnings("unused")` and jals's `[unused]` section are the same word, so
//! the annotation a Java codebase already carries silences the three `[unused]` rules without
//! anyone rewriting it.
//!
//! A name none of the three knows is **ignored, silently**. `"unchecked"`, `"rawtypes"`,
//! `"serial"`, an inspection id belonging to some IDE — a real Java corpus is full of them, they are
//! addressed to other tools, and JLS §9.6.4.5 explicitly leaves an unrecognized name to the
//! compiler's discretion.
//!
//! # What a suppression covers
//!
//! The annotated declaration's whole significant span, the annotation itself included: a finding
//! **contained** in it is dropped. Nesting needs no innermost-wins rule, because `@SuppressWarnings`
//! has no negative form — "any containing host whose names match" is both correct and simpler.
//!
//! Two consequences worth stating rather than solving:
//!
//! - **`unused-imports` cannot be suppressed in source.** Java does not allow `@SuppressWarnings`
//!   on an import declaration, and imports sit outside the type declaration that could carry one.
//!   Inventing a file-level jals syntax for it would be a second suppression language; the
//!   `jalslint.toml` key is the answer.
//! - **`dead-code` reaches its own answer first.** Any annotation on a `private` member already
//!   exempts it from that rule under the default
//!   [`AnnotatedMembers::Skip`](jals_config::lint::AnnotatedMembers) — an `@Inject` and a
//!   `@SuppressWarnings("unchecked")` are alike evidence that something reaches the member without
//!   naming it — so on that one rule a suppression is not what silences the member. It becomes the
//!   only path once a project sets `annotated = "report"`, which is where a test holds it.
//! - **The match is syntactic**, on the annotation's last segment, so a user-defined
//!   `com.acme.SuppressWarnings` matches too. Resolving the annotation type would make this map
//!   depend on [`FileAnalysis`](jals_hir::FileAnalysis) — which the driver computes *lazily, after*
//!   rules start running — so the map would need the very thing it filters.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_exec::Yielder;
use jals_syntax::ast::{Annotation, AnnotationPair, AstNode, Expr, Literal};
use jals_syntax::{SyntaxKind, SyntaxNode};

use crate::rules::RuleMeta;
use crate::rules::significant;

/// The annotation that suppresses, by simple name (`java.lang.SuppressWarnings`).
const SUPPRESS_WARNINGS: &str = "SuppressWarnings";

/// The name that covers every rule — javac's, and the one name that is neither a rule nor a
/// section.
const ALL: &str = "all";

/// The element `@SuppressWarnings` declares. Only its value is read: `@SuppressWarnings(other = …)`
/// does not compile, and reading one anyway would make a typo suppress silently.
const VALUE: &str = "value";

/// One `@SuppressWarnings` in the file: what it covers, and what it names.
struct Suppression {
    /// The annotated declaration's significant span.
    range: Range<usize>,
    /// The names written inside the annotation, unrecognized ones included — matching happens
    /// against the registry in [`covers`](Suppression::covers), so nothing is discarded here.
    names: BTreeSet<String>,
}

impl Suppression {
    /// Whether this suppression's names reach `rule`.
    fn covers(&self, rule: &RuleMeta) -> bool {
        self.names.contains(ALL)
            || self.names.contains(rule.name)
            || self.names.contains(rule.category.config_name())
    }
}

/// Every `@SuppressWarnings` in one file, as spans and names.
///
/// Built once per lint that has something to suppress and consulted per finding. It reads the CST
/// alone, so it is available before any analysis — which is what lets the driver filter a
/// [`Checker::Syntactic`](crate::rules::Checker) rule's findings without resolving the file.
pub(crate) struct SuppressionMap {
    entries: Vec<Suppression>,
}

impl SuppressionMap {
    /// Walk `root` and record every `@SuppressWarnings` that names at least one thing.
    pub(crate) async fn compute(root: &SyntaxNode) -> Self {
        let mut yielder = Yielder::new();
        let mut entries = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            if node.kind() != SyntaxKind::ANNOTATION {
                continue;
            }
            let Some(annotation) = Annotation::cast(node.clone()) else {
                continue;
            };
            if annotation
                .name()
                .and_then(|name| name.last_segment())
                .as_deref()
                != Some(SUPPRESS_WARNINGS)
            {
                continue;
            }
            let names = Self::names(&annotation);
            // `@SuppressWarnings({})` and `@SuppressWarnings(SOME_CONSTANT)` both reach here with
            // nothing to match on. An entry covering everything and naming nothing would suppress
            // nothing anyway, so it is not recorded.
            if names.is_empty() {
                continue;
            }
            let Some(range) = Self::host(&node).as_ref().and_then(significant::range) else {
                continue;
            };
            entries.push(Suppression { range, names });
        }
        Self { entries }
    }

    /// Whether `rule`'s finding at `range` is suppressed: any recorded host that contains the
    /// finding and names the rule.
    pub(crate) fn suppresses(&self, rule: &RuleMeta, range: &Range<usize>) -> bool {
        self.entries.iter().any(|entry| {
            entry.range.start <= range.start && range.end <= entry.range.end && entry.covers(rule)
        })
    }

    /// The declaration an annotation applies to.
    ///
    /// Most declarations park their annotations inside a `MODIFIERS` child, while a type parameter,
    /// an enum constant and a parameter's type-use position write them as direct children — the two
    /// shapes `jals_hir`'s `decl_facts` already distinguishes. Walking up rather than matching the
    /// host's kind is deliberate: nothing has to enumerate what may carry an annotation, so a
    /// `module` declaration (JLS §9.6.4.5 lists `MODULE` among the targets) works with no entry of
    /// its own, and error-recovery debris cannot fall off a match arm.
    fn host(annotation: &SyntaxNode) -> Option<SyntaxNode> {
        let parent = annotation.parent()?;
        if parent.kind() == SyntaxKind::MODIFIERS {
            parent.parent()
        } else {
            Some(parent)
        }
    }

    /// The names one `@SuppressWarnings` writes.
    ///
    /// Three legal spellings reach here, and the grammar keeps them apart: the parser builds an
    /// `ANNOTATION_PAIR` only where it sees `IDENT =`, so `@SuppressWarnings("x")` and
    /// `@SuppressWarnings({"x", "y"})` are a bare `Expr` child of the argument list, while
    /// `@SuppressWarnings(value = …)` is the pair. Anything else in the list — a nested annotation,
    /// a constant reference, an expression, recovery debris — names nothing, which is the honest
    /// answer: a name jals cannot read from the source is a name it must not act on.
    fn names(annotation: &Annotation) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let Some(args) = annotation.args() else {
            return out;
        };
        for child in args.syntax().children() {
            if let Some(pair) = AnnotationPair::cast(child.clone()) {
                if pair.name().as_deref() == Some(VALUE) {
                    Self::collect(pair.value().as_ref(), &mut out);
                }
                continue;
            }
            Self::collect(Expr::cast(child).as_ref(), &mut out);
        }
        out
    }

    /// Add the string names `value` spells: one literal, or the literal elements of an array.
    ///
    /// Deliberately one level deep. JLS §9.7.1 gives an element-value array no nesting, so a `{{…}}`
    /// is not a spelling with names in it — it is source that does not compile.
    fn collect(value: Option<&Expr>, out: &mut BTreeSet<String>) {
        match value {
            Some(Expr::Literal(literal)) => out.extend(Self::name(literal)),
            Some(Expr::ArrayInit(array)) => {
                for element in array.elements() {
                    if let Expr::Literal(literal) = element {
                        out.extend(Self::name(&literal));
                    }
                }
            }
            _ => {}
        }
    }

    /// A plain, escape-free string literal's contents, or `None` for anything else.
    ///
    /// The same reading `jals_syntax::cfg`'s `feature_name` does for `feature = "…"`, and for the
    /// same reason: a rule name is a plain identifier, so a literal carrying an escape is not one
    /// and interpreting it would be a second, partial unescaper next to the lexer's.
    fn name(literal: &Literal) -> Option<String> {
        let token = literal.token()?;
        if token.kind() != SyntaxKind::STRING_LITERAL {
            return None;
        }
        let inner = token
            .text()
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))?;
        (!inner.contains('\\') && !inner.contains('"')).then(|| inner.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jals_exec::block_on_inline;

    /// The suppressions of `src`, as `start..end names` lines — the whole map, so a shape that
    /// records nothing is as visible as one that records the wrong span.
    fn map(src: &str) -> Vec<String> {
        let parse = block_on_inline(jals_syntax::Parse::parse(src));
        let map = block_on_inline(SuppressionMap::compute(&parse.syntax()));
        map.entries
            .iter()
            .map(|entry| {
                let names: Vec<&str> = entry.names.iter().map(String::as_str).collect();
                alloc::format!(
                    "{}..{} {}",
                    entry.range.start,
                    entry.range.end,
                    names.join(",")
                )
            })
            .collect()
    }

    #[test]
    fn the_three_argument_shapes_all_read() {
        // Single value, array, and the `value =` pair in both of its forms. All four are legal
        // Java; the parser gives the pair its own node and the other two a bare expression child,
        // so reading only one of the two paths silently loses the other.
        assert_eq!(
            map("class C { @SuppressWarnings(\"a\") int f; }"),
            ["10..39 a"]
        );
        assert_eq!(
            map("class C { @SuppressWarnings({\"a\", \"b\"}) int f; }"),
            ["10..46 a,b"]
        );
        assert_eq!(
            map("class C { @SuppressWarnings(value = \"a\") int f; }"),
            ["10..47 a"]
        );
        assert_eq!(
            map("class C { @SuppressWarnings(value = {\"a\", \"b\"}) int f; }"),
            ["10..54 a,b"]
        );
    }

    #[test]
    fn a_qualified_annotation_reads() {
        // `@java.lang.SuppressWarnings` is the same annotation; the match is on the last segment.
        assert_eq!(
            map("class C { @java.lang.SuppressWarnings(\"a\") int f; }"),
            ["10..49 a"]
        );
    }

    #[test]
    fn nothing_nameable_records_nothing() {
        // An empty array, a non-string element value, and a constant reference: each is either not
        // a name or not one this pass may read, and none of them may become an entry that covers a
        // span and matches nothing.
        assert!(map("class C { @SuppressWarnings({}) int f; }").is_empty());
        assert!(map("class C { @SuppressWarnings(1) int f; }").is_empty());
        assert!(map("class C { @SuppressWarnings(NAMES) int f; }").is_empty());
        // An element that is not `value`. `@SuppressWarnings` declares no other, so this does not
        // compile — and reading it anyway would make a typo suppress silently.
        assert!(map("class C { @SuppressWarnings(other = \"a\") int f; }").is_empty());
        // No argument list at all, which is a different path: `args()` is `None` rather than a list
        // holding nothing nameable.
        assert!(map("class C { @SuppressWarnings int f; }").is_empty());
        // An escape is not read: the name would need interpreting, which this pass does not do.
        assert!(map("class C { @SuppressWarnings(\"a\\u0062\") int f; }").is_empty());
    }

    #[test]
    fn the_span_is_the_host_declaration() {
        // The annotation covers what it annotates, not itself — a method's whole body included,
        // and starting at the annotation rather than at the newline rowan parks inside the node.
        assert_eq!(
            map("class C {\n  @SuppressWarnings(\"a\")\n  void m() { int x = 1; }\n}"),
            ["12..60 a"]
        );
        // On the type declaration it covers the whole class.
        assert_eq!(
            map("@SuppressWarnings(\"a\")\nclass C { void m() {} }"),
            ["0..46 a"]
        );
    }

    #[test]
    fn a_local_declaration_hosts_one() {
        // JLS §9.6.4.5 lists LOCAL_VARIABLE and PARAMETER among the targets, and neither is a
        // member: walking up from the annotation covers them with no entry of their own.
        assert_eq!(
            map("class C { void m() { @SuppressWarnings(\"a\") int x = 1; } }"),
            ["21..54 a"]
        );
        assert_eq!(
            map("class C { void m(@SuppressWarnings(\"a\") int x) {} }"),
            ["17..45 a"]
        );
    }

    #[test]
    fn broken_input_records_what_it_can() {
        // The extractor is total: `tests/invariants.rs` feeds arbitrary text, so an unclosed
        // argument list, a truncated file, and an annotation with no declaration after it must
        // each come back rather than panic.
        let _ = map("class C { @SuppressWarnings(\"a\" int f; }");
        let _ = map("@SuppressWarnings(\"a\")");
        let _ = map("@SuppressWarnings(");
        let _ = map("class C { @SuppressWarnings(\"a\") }");
    }
}
