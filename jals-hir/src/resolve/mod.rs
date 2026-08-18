//! The resolver: build a scope tree and bind references to definitions.
//!
//! Resolution is two passes over the CST. Pass 1 ([`build`]) walks the tree, creating scopes and
//! registering definitions, and records each reference together with the scope it sits in. Pass 2
//! ([`Resolver::run`]) looks each recorded reference up its scope chain. Because pass 1 registers
//! every definition before pass 2 resolves anything, forward references into a hoisting scope (a
//! field or method used before its declaration) resolve without a separate pre-scan.

mod build;
pub(crate) mod collect;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use hashbrown::HashSet;
use jals_syntax::SyntaxKind::{CALL_EXPR, CLASS_LITERAL, FIELD_ACCESS, METHOD_REF_EXPR};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::cfg::CfgMap;
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::def::{Def, DefId, DefKind, Namespace};
use crate::reference::{Reference, Resolution};
use crate::scope::{Scope, ScopeId, ScopeKind};
use collect::Collect;

/// The result of resolving names within one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// Every definition, indexed by [`DefId`].
    pub defs: Vec<Def>,
    /// Every scope, indexed by [`ScopeId`]; scope `0` is the file scope.
    pub scopes: Vec<Scope>,
    /// Every examined reference, sorted by start offset.
    pub references: Vec<Reference>,
    /// Simple names the file *mentions* where the file-local pass cannot bind them to a
    /// definition: the right-hand name of a member access (`recv.name`) or a method reference
    /// (`recv::name`), the left-hand name of a class literal (`X.class`), every segment of a
    /// qualified type name (`Outer.Inner`), every segment of an annotation's name (`@Anno`), and
    /// every identifier inside a `cfg`-disabled host — which binds nothing but still *spells* the
    /// names of declarations that are themselves enabled. Each is stored decoded, like a [`Def`]'s
    /// name.
    ///
    /// This is deliberately **not** resolution. A mention names no definition and two unrelated
    /// declarations may share one; nothing here binds anything, and no [`Reference`] is recorded
    /// for it. It exists because "unused" is a *negative*, and a negative needs to have looked
    /// everywhere: a member or a nested type whose name is mentioned somewhere the resolver cannot
    /// follow might be exactly the one meant, so its non-resolution is not evidence of disuse.
    /// Over-approximating the set trades a false negative for never a false positive, which is the
    /// only direction an unused diagnostic may err in.
    pub mentions: HashSet<String>,
}

impl Resolved {
    /// Resolves names over an already-parsed CST `root` (the `SOURCE_FILE` node), skipping every
    /// `cfg`-disabled host in `cfg` (computed over the same text as `root`): a disabled
    /// declaration contributes no definition and nothing inside it is recorded as a reference —
    /// the analysis-side mirror of the compile frontend blanking the host. An empty (default) map
    /// resolves the whole file.
    ///
    /// The one entry point, reached only through [`FileAnalysis`](crate::FileAnalysis): resolution
    /// is a step of the analysis, and a caller holding one on its own could sequence the steps
    /// itself.
    pub(crate) async fn resolve_node_with_cfg(root: &SyntaxNode, cfg: &CfgMap) -> Self {
        Resolver::new(root, cfg).run().await
    }

    /// The definition with the given id.
    pub fn def(&self, id: DefId) -> &Def {
        &self.defs[id.0 as usize]
    }

    /// The scope with the given id.
    fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0 as usize]
    }

    /// The innermost (narrowest) scope whose range covers byte `offset` — the cursor's scope. `None`
    /// only for an offset outside the file; otherwise the file scope (which covers everything) bounds
    /// the search, and the chain then climbs `parent`.
    fn scope_at(&self, offset: usize) -> Option<ScopeId> {
        self.scopes
            .iter()
            .filter(|scope| scope.range.start <= offset && offset <= scope.range.end)
            .min_by_key(|scope| scope.range.end - scope.range.start)
            .map(|scope| scope.id)
    }

    /// Every definition visible at byte `offset`, innermost scope outward. A sequential scope (block /
    /// `for` / resources) contributes only the bindings declared before `offset`; every other scope
    /// hoists all of its bindings (parameters, type parameters, fields, methods). The same visibility
    /// rule [`Resolver::lookup`] applies, but yielding every visible binding rather than resolving one
    /// name. Not deduped — a binding and an outer one it shadows both appear, inner first; a caller
    /// wanting one-per-name keeps the first seen.
    pub(crate) fn visible_defs(&self, offset: usize) -> impl Iterator<Item = &Def> {
        let mut chain = Vec::new();
        let mut scope = self.scope_at(offset);
        while let Some(sid) = scope {
            chain.push(sid);
            scope = self.scope(sid).parent;
        }
        chain.into_iter().flat_map(move |sid| {
            let scope = self.scope(sid);
            let sequential = scope.kind.is_sequential();
            scope
                .defs
                .iter()
                .map(move |&did| self.def(did))
                .filter(move |def| !(sequential && def.name_range.start >= offset))
        })
    }

    /// The reference covering byte `offset`, if any.
    pub fn reference_at(&self, offset: usize) -> Option<&Reference> {
        self.references
            .iter()
            .find(|r| r.range.start <= offset && offset < r.range.end)
    }

    /// The definition the reference covering byte `offset` resolves to, if any.
    ///
    /// This is the go-to-definition query: pass the cursor offset, get the target definition.
    pub(crate) fn definition_at(&self, offset: usize) -> Option<&Def> {
        let id = self.reference_at(offset)?.resolution.def_id()?;
        Some(self.def(id))
    }

    /// Every reference that resolves to `id` (the find-references query).
    fn references_to(&self, id: DefId) -> impl Iterator<Item = &Reference> {
        self.references
            .iter()
            .filter(move |r| r.resolution == Resolution::Def(id))
    }

    /// The definition the cursor at byte `offset` denotes, whether the cursor sits on a *reference*
    /// to it or on its own declaring name.
    ///
    /// This is the symbol-under-cursor query shared by find-references and document-highlight: from
    /// either end of a binding, recover the binding. A reference covering `offset` resolves through
    /// its [`Resolution`] (so an [`Unresolved`](Resolution::Unresolved) one yields `None`); failing
    /// that, a definition whose name token covers `offset` is the answer. `None` if the cursor is on
    /// neither.
    pub fn symbol_at(&self, offset: usize) -> Option<DefId> {
        // A reference covering the offset is authoritative — even an unresolved one yields `None`
        // rather than falling through to a same-spanned declaring name.
        if let Some(reference) = self.reference_at(offset) {
            return reference.resolution.def_id();
        }
        self.defs
            .iter()
            .find(|d| d.name_range.start <= offset && offset < d.name_range.end)
            .map(|d| d.id)
    }

    /// The declaration of `id` (when `include_declaration`) together with every reference to it, as
    /// byte ranges in document order.
    ///
    /// This is the occurrence set behind find-references and document-highlight: from a binding,
    /// the spans across the file that denote it.
    pub fn occurrences(
        &self,
        id: DefId,
        include_declaration: bool,
    ) -> Vec<core::ops::Range<usize>> {
        let mut ranges: Vec<core::ops::Range<usize>> =
            self.references_to(id).map(|r| r.range.clone()).collect();
        if include_declaration {
            ranges.push(self.def(id).name_range.clone());
        }
        ranges.sort_by_key(|r| r.start);
        ranges
    }

    /// Every definition nothing in this file can be denoting.
    ///
    /// A use can look three ways, and a definition has to survive all three to be reported:
    ///
    /// - No recorded reference *resolves* to it.
    /// - If it lives in the [method name-space](Namespace::Method), no call spells its name either.
    ///   A method is overloadable and the scope chain binds a call to *a* declaration of the name
    ///   rather than to the overload the arguments select — that needs types this pass has not got
    ///   — so the name is the finest granularity it honestly has. Without this, eight of
    ///   `java.util.Arrays`' nine `binarySearch0` overloads read as unused because the one call
    ///   landed on the ninth.
    /// - If it is a [kind a member access could name](DefKind::is_member), its name is not among
    ///   the file's [mentions](Self::mentions). Without this, `this.x` and `Outer.Inner` are
    ///   invisible here and every field, method, and nested type they name reads as unused.
    ///
    /// This is the raw signal for unused-binding diagnostics, and still only *file-local*: an
    /// unreferenced field or method may be used from another file, so a consumer narrows it to the
    /// kinds — and the visibility ([`Def::is_private`]) — whose answer one file completes.
    pub fn unused_defs(&self) -> impl Iterator<Item = &Def> {
        let referenced: HashSet<DefId> = self
            .references
            .iter()
            .filter_map(|r| r.resolution.def_id())
            .collect();
        let called: HashSet<&str> = self
            .references
            .iter()
            .filter(|r| r.namespace == Namespace::Method)
            .map(|r| r.name.as_str())
            .collect();
        // One early return per bullet above, in the order they cost.
        self.defs.iter().filter(move |def| {
            if referenced.contains(&def.id) {
                return false;
            }
            if def.kind.namespace() == Namespace::Method && called.contains(def.name.as_str()) {
                return false;
            }
            !def.kind.is_member() || !self.mentions.contains(def.name.as_str())
        })
    }
}

/// A reference recorded in pass 1, before its scope chain has been searched.
struct RawRef {
    range: core::ops::Range<usize>,
    name: String,
    namespace: Namespace,
    scope: ScopeId,
    /// The full dotted text of a qualified type reference (`a.b.C`); `None` for a simple name.
    qualified: Option<String>,
}

/// Builds the scope tree and resolves references for one file.
pub(crate) struct Resolver {
    root: SyntaxNode,
    /// The file's `cfg` evaluation; pass 1 skips every disabled host (and, by not descending,
    /// its whole subtree). Empty when the caller has no attributes to apply.
    cfg: CfgMap,
    defs: Vec<Def>,
    scopes: Vec<Scope>,
    raw_refs: Vec<RawRef>,
    /// The names collected for [`Resolved::mentions`].
    mentions: HashSet<String>,
    /// Amortized-yield countdown for the pass-1 walk (a field, not a local `Yielder`, because the
    /// walk is recursive — every visited node shares the one budget).
    yield_left: u32,
}

impl Resolver {
    /// One unit of pass-1 work: yields once per [`jals_exec::Yielder::DEFAULT_PERIOD`] nodes.
    async fn tick(&mut self) {
        self.yield_left -= 1;
        if self.yield_left == 0 {
            self.yield_left = jals_exec::Yielder::DEFAULT_PERIOD;
            jals_exec::yield_now().await;
        }
    }
}

impl Resolver {
    /// Creates a resolver rooted at `root` (the `SOURCE_FILE` node), seeded with the file scope.
    fn new(root: &SyntaxNode, cfg: &CfgMap) -> Self {
        let file_scope = Scope {
            id: ScopeId(0),
            kind: ScopeKind::File,
            parent: None,
            range: Collect::node_span(root),
            defs: Vec::new(),
        };
        Self {
            root: root.clone(),
            cfg: cfg.clone(),
            defs: Vec::new(),
            scopes: vec![file_scope],
            raw_refs: Vec::new(),
            mentions: HashSet::new(),
            yield_left: jals_exec::Yielder::DEFAULT_PERIOD,
        }
    }

    /// Runs both passes and returns the result.
    async fn run(mut self) -> Resolved {
        let root = self.root.clone();
        self.build(&root, ScopeId(0)).await;

        let mut yielder = jals_exec::Yielder::new();
        let raw_refs = core::mem::take(&mut self.raw_refs);
        let mut references = Vec::with_capacity(raw_refs.len());
        for raw in raw_refs {
            yielder.tick().await;
            // A qualified type name (`a.b.C`) never binds to a file-local definition; leave it
            // unresolved for the project layer, which resolves it against a fully-qualified name.
            let resolution = if raw.qualified.is_some() {
                Resolution::Unresolved
            } else {
                self.lookup(raw.scope, &raw.name, raw.namespace, raw.range.start)
                    .map_or(Resolution::Unresolved, Resolution::Def)
            };
            references.push(Reference {
                range: raw.range,
                name: raw.name,
                namespace: raw.namespace,
                resolution,
                qualified: raw.qualified,
            });
        }
        references.sort_by_key(|r| r.range.start);

        Resolved {
            defs: self.defs,
            scopes: self.scopes,
            references,
            mentions: self.mentions,
        }
    }

    /// Creates a child scope of `parent` covering `node`, and returns its id.
    fn new_scope(&mut self, kind: ScopeKind, parent: ScopeId, node: &SyntaxNode) -> ScopeId {
        let id = ScopeId(self.scopes.len() as u32);
        self.scopes.push(Scope {
            id,
            kind,
            parent: Some(parent),
            range: Collect::node_span(node),
            defs: Vec::new(),
        });
        id
    }

    /// Registers a definition named by `name_tok` and declared by `decl` in `scope`, and returns
    /// its id.
    ///
    /// `decl` is the declaring node rather than the name token's parent, because the two differ
    /// where it matters: `int a, b;` is one `FIELD_DECL` binding two names, and both are `private`
    /// exactly when the one declaration is.
    fn add_def(
        &mut self,
        scope: ScopeId,
        kind: DefKind,
        name_tok: &SyntaxToken,
        decl: &SyntaxNode,
    ) -> DefId {
        let id = DefId(self.defs.len() as u32);
        let facts = Collect::decl_facts(decl);
        self.defs.push(Def {
            id,
            kind,
            // The *decoded* spelling (JLS §3.3): `int \u0061;` declares `a`, and keying a
            // definition on the raw text made it a different name from every plain-spelled use.
            name: jals_syntax::decoded_ident(name_tok).into_owned(),
            name_range: Collect::byte_range(name_tok),
            is_private: facts.is_private,
            is_annotated: facts.is_annotated,
            scope,
        });
        self.scopes[scope.0 as usize].defs.push(id);
        id
    }

    /// Records the `NAME_REF` `node` as a reference in `scope`.
    ///
    /// Only identifier references are recorded; `this` / `super` (keyword name-refs) have no
    /// file-local definition target and are skipped. The namespace is decided by position: a bare
    /// callee of a call is a method reference, everything else is a value reference.
    fn record_ref(&mut self, scope: ScopeId, node: &SyntaxNode) {
        let Some(tok) = Collect::first_ident_token(node) else {
            return;
        };
        let parent = node.parent().map(|p| p.kind());
        let namespace = if parent == Some(CALL_EXPR) {
            Namespace::Method
        } else {
            Namespace::Value
        };
        let name = jals_syntax::decoded_ident(&tok).into_owned();
        // A name in *qualifier* position (`Holder.numberGenerator`, `Holder::get`) is what JLS
        // §6.5.2 calls an ambiguous name: it denotes a variable, or a type, or a package, and only
        // reclassification decides which. This pass looks it up as a value and stops there, so a
        // type used solely to qualify a static member resolves to nothing — which is why the name
        // is also recorded as a mention, and why `UUID`'s `private static class Holder` is not
        // read as unused for being reached only through `Holder.numberGenerator`.
        //
        // `Holder.class` is the same shape with the verdict already settled: a class literal's
        // left-hand side is a *type* and nothing else (JLS §15.8.2), and the grammar spells a bare
        // one as a `NAME_REF` — so the value-namespace lookup above can only miss, and without the
        // mention every `private` nested type reached solely through `X.class` reads as dead.
        if matches!(parent, Some(FIELD_ACCESS | METHOD_REF_EXPR | CLASS_LITERAL))
            && !self.mentions.contains(&name)
        {
            self.mentions.insert(name.clone());
        }
        self.raw_refs.push(RawRef {
            range: Collect::byte_range(&tok),
            name,
            namespace,
            scope,
            qualified: None,
        });
    }

    /// Records the type named by the `TYPE` `node` as a [`Namespace::Type`] reference in `scope`.
    ///
    /// A primitive, `var`, or `void` type carries no resolvable name and is skipped. The recorded
    /// range is the simple-name identifier (the last `IDENT` of a dotted type), so go-to-definition
    /// lands on the type name. A qualified type (`a.b.C`) keeps its full dotted text in `qualified`
    /// and is left unresolved by the file-local pass — only the project layer can bind it.
    fn record_type_ref(&mut self, scope: ScopeId, node: &SyntaxNode) {
        let Some(ty) = ast::Type::cast(node.clone()) else {
            return;
        };
        // A primitive / `var` / `void` type has no simple-name token, so this also skips them.
        let Some(tok) = ty.simple_name_token() else {
            return;
        };
        // The full dotted text only for a qualified type (`a.b.C`); a bare name has no `.`.
        let qualified = ty.qualified_text().filter(|q| q.contains('.'));
        // Every segment of a qualified name is a mention: the reference below carries only the
        // simple name `C`, so without this an `Outer` that `Outer.Inner` names — a nested type, or
        // the import that made the prefix nameable — has no trace in the analysis at all.
        if qualified.is_some() {
            self.record_mentions(node);
        }
        self.raw_refs.push(RawRef {
            range: Collect::byte_range(&tok),
            name: jals_syntax::decoded_ident(&tok).into_owned(),
            namespace: Namespace::Type,
            scope,
            qualified,
        });
    }

    /// Records every `IDENT` directly under `node` in [`Resolved::mentions`], decoded.
    ///
    /// Direct tokens only, which is what makes one call fit all four mention shapes: a member
    /// access and a method reference keep the member name as their own token while the receiver is
    /// a child *node*, and a qualified name or an annotation name spells every segment as a direct
    /// token of the one node passed in.
    fn record_mentions(&mut self, node: &SyntaxNode) {
        for tok in Collect::direct_ident_tokens(node) {
            self.mention(&jals_syntax::decoded_ident(&tok));
        }
    }

    /// Records one already-decoded name in [`Resolved::mentions`].
    ///
    /// The membership probe is not an optimization detail: `decoded_ident` borrows for a name
    /// carrying no escape, so `into_owned()` allocates on *every* occurrence, and the same handful
    /// of receivers (`System`, `Objects`, `this`-qualified fields) is spelled hundreds of times in
    /// one file. Probing first allocates once per distinct name instead of once per occurrence, on
    /// a pass that runs per keystroke.
    fn mention(&mut self, name: &str) {
        if !self.mentions.contains(name) {
            self.mentions
                .insert(alloc::string::ToString::to_string(name));
        }
    }

    /// Records every `IDENT` anywhere under a `cfg`-disabled `node` as a mention.
    ///
    /// The host itself contributes no definition, no scope and no reference — the code will not be
    /// compiled, so nothing in it may *bind*. But "unused" is a negative over the whole file, and
    /// the declaration a disabled host names is usually **not** disabled: it serves the other
    /// feature set, where the same file does use it. Dropping the host whole would make every
    /// `private` member reached only from behind a flag read as dead, and the fix the diagnostic
    /// asks for would break the build the flag turns on. That is the same argument
    /// [`unused_imports`](crate::FileAnalysis::unused_imports) makes for reading an import's
    /// evidence off the token stream, and this is where the binding analyses get it: a mention is
    /// exactly the shape of evidence that says "somewhere in this file, someone spells this" while
    /// binding nothing.
    pub(super) fn record_disabled_mentions(&mut self, node: &SyntaxNode) {
        for tok in node
            .descendants_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|tok| tok.kind() == jals_syntax::SyntaxKind::IDENT)
        {
            self.mention(&jals_syntax::decoded_ident(&tok));
        }
    }

    /// Records an annotation's name segments as mentions.
    ///
    /// An annotation names a type, and the file-local pass records no reference for it: a `TYPE`
    /// node is what [`record_type_ref`](Self::record_type_ref) reads, and `@Anno` has a
    /// `QUALIFIED_NAME` instead. Recording it as a *mention* rather than as a reference keeps that
    /// gap out of the unused analyses without also handing the project layer a type reference it
    /// would then have to resolve — which would change what `cannot-resolve` reports, a separate
    /// question from this one.
    fn record_annotation_mention(&mut self, node: &SyntaxNode) {
        if let Some(name) = ast::Annotation::cast(node.clone()).and_then(|a| a.name()) {
            self.record_mentions(name.syntax());
        }
    }

    /// Looks `name` up from `scope` outward, in name-space `ns`.
    ///
    /// In a sequential scope (block / for / resources) only definitions declared before
    /// `ref_start` are visible, and the nearest preceding one wins; other scopes hoist all their
    /// definitions. The first scope with a match stops the search, so an inner binding shadows an
    /// outer one of the same name.
    fn lookup(&self, from: ScopeId, name: &str, ns: Namespace, ref_start: usize) -> Option<DefId> {
        let mut cur = Some(from);
        while let Some(sid) = cur {
            let scope = &self.scopes[sid.0 as usize];
            let sequential = scope.kind.is_sequential();
            let mut found = None;
            for &did in &scope.defs {
                let def = &self.defs[did.0 as usize];
                if def.name != name || def.kind.namespace() != ns {
                    continue;
                }
                if sequential && def.name_range.start >= ref_start {
                    continue;
                }
                found = Some(did);
            }
            if found.is_some() {
                return found;
            }
            cur = scope.parent;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use jals_exec::block_on_inline;
    use jals_syntax::cfg::CfgMap;
    use proptest::prelude::*;

    use super::Resolved;

    /// Java-ish source built from brace-bearing fragments, so the generated trees actually nest
    /// scopes. Deliberately allowed to be unbalanced: a recovered tree still gets a scope tree, and
    /// that is where an out-of-bounds range would come from.
    fn braced() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just("class C"),
                Just("void m()"),
                Just("if (x)"),
                Just("for (;;)"),
                Just("int x = 1;"),
                Just("{"),
                Just("}"),
                Just("("),
                Just(")"),
                Just(" "),
            ],
            0..40,
        )
        .prop_map(|parts| parts.concat())
    }

    proptest! {
        /// Every scope's range is well-formed and within the source bounds.
        ///
        /// The definition and reference halves of this property are checked through the public
        /// surface in `tests/invariants.rs`; the scope tree is crate-internal — it is a step of the
        /// analysis, not a result — so its half is checked here rather than by exporting the step.
        #[test]
        fn scope_ranges_are_in_bounds(src in braced()) {
            let parse = block_on_inline(jals_syntax::Parse::parse(&src));
            let resolved = block_on_inline(Resolved::resolve_node_with_cfg(
                &parse.syntax(),
                &CfgMap::default(),
            ));
            for scope in &resolved.scopes {
                prop_assert!(scope.range.start <= scope.range.end);
                prop_assert!(scope.range.end <= src.len());
            }
        }
    }
}
