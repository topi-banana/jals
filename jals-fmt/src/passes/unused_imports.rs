//! R0.2 — delete an `import` whose simple name appears nowhere else in the file.
//!
//! # A name test, not a type test
//!
//! `RemoveUnusedImports` resolves nothing. It collects every identifier in
//! the compilation unit plus the reference names inside Javadoc's `{@link}` / `@see` / `@throws`,
//! and drops an import whose last component is not in that set. That is a **heuristic**: a local
//! variable named `List` keeps `java.util.List` alive even though nothing uses the type, and a
//! name shadowed by a nested class is likewise counted. Both are blind spots GJF has, and
//! reproducing them is the point — matching its output matters more than being cleverer than it.
//!
//! The upside is that the whole pass stays inside the CST. It needs no `jals-hir`, no classpath,
//! and no host I/O, so it runs identically in the browser. IntelliJ's optimize-imports takes the
//! other road — its wildcard aggregation counts *resolved* imports — which is exactly why that
//! setting does not project onto this one (`MAPPING.md` §7).
//!
//! # What is never removed
//!
//! An on-demand import (`import java.util.*;`) has no simple name to test, and a jals grouped
//! import (`import a.{B, C};`) would need per-member surgery that changes the node's shape rather
//! than deleting it. Both are kept.

use alloc::collections::BTreeSet;
use alloc::string::String;

use jals_syntax::ast::{AstNode, ImportDecl};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

/// The used-name set of a compilation unit.
pub(crate) struct UnusedImports;

impl UnusedImports {
    /// Every simple name that appears outside an `import` declaration.
    pub(crate) async fn used_names(root: &SyntaxNode) -> BTreeSet<String> {
        let mut used = BTreeSet::new();
        let mut yielder = jals_exec::Yielder::new();
        for element in root.descendants_with_tokens() {
            yielder.tick().await;
            let Some(tok) = element.into_token() else {
                continue;
            };
            match tok.kind() {
                SyntaxKind::IDENT => {
                    if !Self::inside_import(&tok) {
                        used.insert(tok.text().into());
                    }
                }
                SyntaxKind::DOC_COMMENT => Self::collect_javadoc_names(tok.text(), &mut used),
                _ => {}
            }
        }
        used
    }

    /// Whether a token sits inside an `import` declaration, whose own names must not count as
    /// uses — otherwise every import would keep itself alive.
    fn inside_import(tok: &jals_syntax::SyntaxToken) -> bool {
        tok.parent_ancestors()
            .any(|node| node.kind() == SyntaxKind::IMPORT_DECL)
    }

    /// Harvest the type names a Javadoc block references.
    ///
    /// A `@link` / `@linkplain` / `@see` / `@throws` / `@exception` / `@value` tag is followed by
    /// a reference like `Foo#bar(Baz)`. Every identifier-shaped run in the rest of that line
    /// counts, which over-approximates — and over-approximating is the safe direction, since the
    /// cost is keeping an import that could have gone.
    fn collect_javadoc_names(text: &str, used: &mut BTreeSet<String>) {
        /// The tags whose argument is a type reference.
        const REFERENCE_TAGS: [&str; 6] = [
            "@link",
            "@linkplain",
            "@see",
            "@throws",
            "@exception",
            "@value",
        ];

        for line in text.lines() {
            let trimmed = line.trim_start().trim_start_matches('*').trim_start();
            let Some(tag) = REFERENCE_TAGS.iter().find(|tag| trimmed.starts_with(**tag)) else {
                continue;
            };
            for word in trimmed[tag.len()..].split(|c: char| !Self::is_name_char(c)) {
                if !word.is_empty() && word.chars().next().is_some_and(char::is_alphabetic) {
                    used.insert(word.into());
                }
            }
        }
    }

    /// Whether `c` can appear in a Java identifier.
    fn is_name_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_' || c == '$'
    }

    /// Whether `decl` should survive the pass.
    ///
    /// An on-demand or grouped import is always kept (see the module docs); everything else lives
    /// iff its last name component is in `used`.
    pub(crate) fn is_used(decl: &ImportDecl, used: &BTreeSet<String>) -> bool {
        if decl.group().is_some() || decl.is_module() {
            return true;
        }
        let Some(name) = decl.name() else {
            return true;
        };
        let Some(last) = name
            .syntax()
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| tok.kind() == SyntaxKind::IDENT)
            .last()
        else {
            // No identifier at all: an on-demand `a.b.*`, or error-recovery debris. Keep it.
            return true;
        };
        used.contains(last.text())
    }
}
