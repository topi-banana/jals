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

/// The simple names of the file's imports that the file itself never spells.
///
/// Phrased as what is *left over* rather than as what is used, because the question only ever has
/// a handful of needles and the haystack is the whole token stream: narrowing a set the size of
/// the import list costs nothing per token and lets the walk stop the moment the last needle is
/// found, where collecting every name the file spells allocates one `String` per identifier
/// occurrence and per comment word — tens of thousands on a large file, on a pass that runs per
/// keystroke.
struct Unspelled(HashSet<String>);

impl Unspelled {
    /// Removes from `wanted` every name the file spells outside its own import declarations, and
    /// every identifier-shaped word of every comment.
    ///
    /// Two exclusions, one walk. An import must not count as a use of itself — that is the whole
    /// question — so an `IMPORT_DECL` contributes no identifiers, which is also what removes a
    /// static import's member name and every package segment beside it. Its *comments* still
    /// count: a Javadoc `{@link Foo}` is the entire reason plenty of imports exist, and a
    /// `// keep Foo` written between two imports is the same claim spelled less formally — rowan
    /// parks that comment inside the import that follows it.
    async fn narrow(root: &SyntaxNode, mut wanted: HashSet<String>) -> Self {
        let mut yielder = jals_exec::Yielder::new();
        // An import declaration is a direct child of the source file, so the node is the whole
        // exclusion; a stray direct token of the file is not one, and counts like any other name.
        for element in root.children_with_tokens() {
            let imported = element
                .as_node()
                .is_some_and(|node| node.kind() == IMPORT_DECL);
            for token in Self::tokens(&element) {
                // Ticked per *traversed* token rather than per matching one: a long run of
                // literals or punctuation must not spend the amortized budget on nothing.
                yielder.tick().await;
                if wanted.is_empty() {
                    return Self(wanted);
                }
                // The *decoded* spelling (JLS §3.3), like a `Def`'s name: `\u0053et` and `Set` are
                // one name, and an import matched against the raw text would miss the escaped use.
                // §3.3 runs before the lexer even recognizes a comment, so a Javadoc reference
                // written with an escape is decoded too.
                let text = jals_syntax::decoded_ident(&token);
                if token.kind() == IDENT {
                    if !imported {
                        wanted.remove(text.as_ref());
                    }
                } else if token.kind().is_trivia() {
                    for word in text
                        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
                        .filter(|word| !word.is_empty())
                    {
                        wanted.remove(word);
                    }
                }
            }
        }
        Self(wanted)
    }

    /// Every token under `element`, in source order — the element itself when it is a token.
    fn tokens(element: &SyntaxElement) -> impl Iterator<Item = SyntaxToken> + use<> {
        let (node, token) = match element {
            SyntaxElement::Node(node) => (Some(node.clone()), None),
            SyntaxElement::Token(token) => (None, Some(token.clone())),
        };
        node.into_iter()
            .flat_map(|node| {
                node.descendants_with_tokens()
                    .filter_map(SyntaxElement::into_token)
            })
            .chain(token)
    }

    /// Whether the file never spells `name`.
    fn never_spelled(&self, name: &str) -> bool {
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
        let mut yielder = jals_exec::Yielder::new();
        // The candidates first, and the token walk only if there are any: a file whose imports are
        // all on-demand or module ones — or which has none at all — has nothing to look for, and
        // walking every token of it to find that out is the whole cost of the analysis.
        let mut candidates: Vec<(UnusedImport, String)> = Vec::new();
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
            // independently: each member is checked, and reported, on its own. Everything after the
            // simple name is the same for both shapes, so only the candidates differ.
            let mut members: Vec<(Range<usize>, String, Option<String>)> = Vec::new();
            if let Some(group) = import.group() {
                let prefix = name.text();
                members.extend(group.members().map(|member| {
                    (
                        Collect::significant_span(member.syntax()),
                        alloc::format!("{prefix}.{}", member.text()),
                        // `None` for an on-demand member (`{concurrent.*}`), which names no
                        // single type.
                        member.last_segment(),
                    )
                }));
            } else {
                // The simple name is the last segment either way: a single-type import binds the
                // type, and a static import binds the member, and both are what the source then
                // spells. `None` for an on-demand import (`a.b.*`).
                members.push((
                    Collect::significant_span(import.syntax()),
                    name.text(),
                    name.last_segment(),
                ));
            }
            for (range, name, simple) in members {
                let Some(simple) = simple else {
                    continue;
                };
                candidates.push((
                    UnusedImport {
                        range,
                        name,
                        is_static,
                    },
                    simple,
                ));
            }
        }
        if candidates.is_empty() {
            return Vec::new();
        }
        let unspelled = Unspelled::narrow(
            root,
            candidates
                .iter()
                .map(|(_, simple)| simple.clone())
                .collect(),
        )
        .await;
        candidates
            .into_iter()
            .filter(|(_, simple)| unspelled.never_spelled(simple))
            .map(|(import, _)| import)
            .collect()
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
