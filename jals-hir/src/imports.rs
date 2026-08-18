//! `unused-imports` analysis for import declarations: an import whose name the file never spells.
//!
//! An import is not a binding — nothing declares it and no [`Def`](crate::Def) stands for it — so
//! it is answered here rather than by the resolver: an import is used when the simple name it
//! brings into scope is *spelled* anywhere else in the file. The evidence is the token stream, not
//! the resolution: every `IDENT` outside the import declarations themselves, plus every
//! identifier-shaped word inside a comment — which is what an import used only by a Javadoc
//! `{@link Foo}` has to its name.
//!
//! **Conservative — never a false positive**, and reading tokens rather than references is what
//! makes that true twice over. Matching on the simple name alone over-approximates use: an
//! unrelated `Foo` anywhere in the file, in any position, spares an `import a.b.Foo`. And a name
//! spelled inside a `cfg`-disabled host still counts, which resolution-based evidence could not
//! manage — the resolver skips a disabled host, while the import above it is not disabled and
//! serves the *other* feature set, where the same file does use it.
//!
//! Two shapes are never reported, because one file cannot decide them: an on-demand (wildcard)
//! import names no single type, so there is nothing to look for, and a module import names neither
//! a type nor a member. `wildcard-import` is the rule that has something to say about the first.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use hashbrown::HashSet;
use jals_syntax::SyntaxKind::{IDENT, IMPORT_DECL};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{SyntaxElement, SyntaxNode, SyntaxToken};

use crate::resolve::collect::Collect;

/// An import declaration whose name the file never uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnusedImport {
    /// The byte range to report: the whole `import …;` declaration, or — for a jals grouped import,
    /// where the members beside it may be perfectly good — the one member.
    pub range: Range<usize>,
    /// The imported name in full (`java.util.Map`; `java.util.Map.entry` for a static import), with
    /// a grouped member's shared prefix folded back in, so it reads as the import it stands for
    /// rather than as the fragment it is written as.
    pub name: String,
    /// Whether this is an `import static` declaration, which imports a *member* rather than a type.
    pub is_static: bool,
}

/// The names a file spells anywhere, in any position — the set an import is checked against.
struct Used(HashSet<String>);

impl Used {
    /// Collects every identifier the file spells outside its own import declarations, plus the
    /// identifier-shaped words of every comment.
    ///
    /// Two walks, because the two exclusions differ. An import must not count as a use of itself —
    /// that is the whole question — so the identifier walk skips the declaration whole, which is
    /// also what removes a static import's member name and every package segment beside it. The
    /// comment walk skips nothing: a Javadoc `{@link Foo}` is the entire reason plenty of imports
    /// exist, and a `// keep Foo` written between two imports is the same claim spelled less
    /// formally — rowan parks that comment inside the import that follows it, which the first walk
    /// has just stepped over.
    async fn collect(root: &SyntaxNode) -> Self {
        let mut names = HashSet::new();
        let mut yielder = jals_exec::Yielder::new();
        // An import declaration is a direct child of the source file, so skipping the node is the
        // whole exclusion.
        for node in root.children().filter(|node| node.kind() != IMPORT_DECL) {
            for token in Self::tokens(&node).filter(|token| token.kind() == IDENT) {
                yielder.tick().await;
                // The *decoded* spelling (JLS §3.3), like a `Def`'s name: `\u0053et` and `Set` are
                // one name, and an import matched against the raw text would miss the escaped use.
                names.insert(jals_syntax::decoded_ident(&token).into_owned());
            }
        }
        for token in Self::tokens(root).filter(|token| token.kind().is_trivia()) {
            yielder.tick().await;
            names.extend(
                token
                    .text()
                    .split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
                    .filter(|word| !word.is_empty())
                    .map(String::from),
            );
        }
        Self(names)
    }

    /// Every token under `node`, in source order.
    fn tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
        node.descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
    }

    /// Whether `name` appears anywhere in the file.
    fn spells(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

impl crate::analysis::FileAnalysis {
    /// Every import declaration the file never uses, in source order.
    ///
    /// File-local by nature, and by more than convenience: an import is scoped to the compilation
    /// unit that writes it, so this file has seen every use there can be. See the module docs for
    /// what counts as a use and for the two shapes that are never reported.
    pub async fn unused_imports(&self) -> Vec<UnusedImport> {
        let root = self.root();
        let Some(source) = ast::SourceFile::cast(root.clone()) else {
            return Vec::new();
        };
        let used = Used::collect(root).await;
        let mut yielder = jals_exec::Yielder::new();
        let mut out = Vec::new();
        for import in source.imports() {
            yielder.tick().await;
            // A module import (`import module java.base;`) names neither a type nor a member, so no
            // simple name stands for it and there is nothing to look for.
            if import.is_module() {
                continue;
            }
            let Some(name) = import.name() else {
                continue; // broken parse (`import ;`) — nothing was imported.
            };
            let is_static = import.is_static();
            // A jals grouped import is several imports sharing a prefix, and they are used
            // independently: each member is checked, and reported, on its own.
            if let Some(group) = import.group() {
                let prefix = name.text();
                for member in group.members() {
                    let Some(simple) = member.last_segment() else {
                        continue; // an on-demand member (`{concurrent.*}`) names no single type.
                    };
                    if used.spells(&simple) {
                        continue;
                    }
                    out.push(UnusedImport {
                        range: Collect::significant_span(member.syntax()),
                        name: alloc::format!("{prefix}.{}", member.text()),
                        is_static,
                    });
                }
                continue;
            }
            // The simple name is the last segment either way: a single-type import binds the type,
            // and a static import binds the member, and both are what the source then spells.
            let Some(simple) = name.last_segment() else {
                continue; // an on-demand import (`a.b.*`) names no single type.
            };
            if used.spells(&simple) {
                continue;
            }
            out.push(UnusedImport {
                range: Collect::significant_span(import.syntax()),
                name: name.text(),
                is_static,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use crate::analysis::FileAnalysis;

    fn unused(src: &str) -> alloc::vec::Vec<alloc::string::String> {
        jals_exec::block_on_inline(async {
            FileAnalysis::parse(src)
                .await
                .unused_imports()
                .await
                .into_iter()
                .map(|import| import.name)
                .collect()
        })
    }

    #[test]
    fn an_import_no_name_spells_is_unused() {
        assert_eq!(
            unused("import java.util.List;\nimport java.util.Map;\nclass C { List<String> l; }"),
            ["java.util.Map"],
        );
    }

    #[test]
    fn a_wildcard_or_module_import_is_never_reported() {
        assert!(unused("import java.util.*;\nimport module java.base;\nclass C {}").is_empty());
    }

    #[test]
    fn an_annotation_uses_its_import() {
        // The resolver records no reference for `@Retention` — an annotation name is a
        // `QUALIFIED_NAME`, not a `TYPE` — so this passes only through the mention set.
        assert!(
            unused("import java.lang.annotation.Retention;\n@Retention(null) class C {}")
                .is_empty()
        );
    }

    #[test]
    fn a_javadoc_link_uses_its_import() {
        assert!(unused("import java.util.Set;\n/** See {@link Set}. */\nclass C {}").is_empty());
    }

    #[test]
    fn a_static_import_is_used_by_a_bare_call() {
        assert_eq!(
            unused(
                "import static java.lang.Math.max;\nimport static java.lang.Math.min;\n\
                 class C { int m() { return max(1, 2); } }"
            ),
            ["java.lang.Math.min"],
        );
    }

    #[test]
    fn a_grouped_member_is_reported_alone() {
        assert_eq!(
            unused("import java.util.{List, Map};\nclass C { List<String> l; }"),
            ["java.util.Map"],
        );
    }

    #[test]
    fn a_qualified_prefix_uses_its_import() {
        // `Outer.Inner` records a reference named `Inner` only; the prefix survives as a mention.
        assert!(unused("import a.Outer;\nclass C { Outer.Inner field; }").is_empty());
    }

    #[test]
    fn the_reported_range_is_the_declaration_without_its_leading_trivia() {
        let import = jals_exec::block_on_inline(async {
            FileAnalysis::parse("\nimport java.util.Map;\nclass C {}")
                .await
                .unused_imports()
                .await
        });
        assert_eq!(import[0].range, 1..22);
    }
}
