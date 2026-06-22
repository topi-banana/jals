//! The resolver: build a scope tree and bind references to definitions.
//!
//! Resolution is two passes over the CST. Pass 1 ([`build`]) walks the tree, creating scopes and
//! registering definitions, and records each reference together with the scope it sits in. Pass 2
//! ([`Resolver::run`]) looks each recorded reference up its scope chain. Because pass 1 registers
//! every definition before pass 2 resolves anything, forward references into a hoisting scope (a
//! field or method used before its declaration) resolve without a separate pre-scan.

mod build;
pub(crate) mod collect;

use std::collections::HashSet;

use jals_syntax::SyntaxKind::CALL_EXPR;
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::def::{Def, DefId, DefKind, Namespace};
use crate::reference::{Reference, Resolution};
use crate::scope::{Scope, ScopeId, ScopeKind};
use collect::{byte_range, first_ident_token};

/// The result of resolving names within one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Every definition, indexed by [`DefId`].
    pub defs: Vec<Def>,
    /// Every scope, indexed by [`ScopeId`]; scope `0` is the file scope.
    pub scopes: Vec<Scope>,
    /// Every examined reference, sorted by start offset.
    pub references: Vec<Reference>,
}

impl Resolved {
    /// The definition with the given id.
    pub fn def(&self, id: DefId) -> &Def {
        &self.defs[id.0 as usize]
    }

    /// The scope with the given id.
    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0 as usize]
    }

    /// The reference covering byte `offset`, if any.
    fn reference_at(&self, offset: usize) -> Option<&Reference> {
        self.references
            .iter()
            .find(|r| r.range.start <= offset && offset < r.range.end)
    }

    /// The definition the reference covering byte `offset` resolves to, if any.
    ///
    /// This is the go-to-definition query: pass the cursor offset, get the target definition.
    pub fn definition_at(&self, offset: usize) -> Option<&Def> {
        let id = self.reference_at(offset)?.resolution.def_id()?;
        Some(self.def(id))
    }

    /// Every reference that resolves to `id` (the find-references query).
    pub fn references_to(&self, id: DefId) -> impl Iterator<Item = &Reference> {
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
    pub fn occurrences(&self, id: DefId, include_declaration: bool) -> Vec<std::ops::Range<usize>> {
        let mut ranges: Vec<std::ops::Range<usize>> =
            self.references_to(id).map(|r| r.range.clone()).collect();
        if include_declaration {
            ranges.push(self.def(id).name_range.clone());
        }
        ranges.sort_by_key(|r| r.start);
        ranges
    }

    /// Every definition that no reference resolves to.
    ///
    /// This is the raw signal for unused-binding diagnostics; a consumer narrows it to the kinds it
    /// cares about (e.g. local variables). Note that an unreferenced field or method is not
    /// necessarily unused — it may be used from another file — so callers should filter by kind.
    pub fn unused_defs(&self) -> impl Iterator<Item = &Def> {
        let referenced: HashSet<DefId> = self
            .references
            .iter()
            .filter_map(|r| r.resolution.def_id())
            .collect();
        self.defs
            .iter()
            .filter(move |d| !referenced.contains(&d.id))
    }

    /// Every reference that bound to no file-local definition.
    pub fn unresolved(&self) -> impl Iterator<Item = &Reference> {
        self.references
            .iter()
            .filter(|r| r.resolution == Resolution::Unresolved)
    }
}

/// A reference recorded in pass 1, before its scope chain has been searched.
struct RawRef {
    range: std::ops::Range<usize>,
    name: String,
    namespace: Namespace,
    scope: ScopeId,
    /// The full dotted text of a qualified type reference (`a.b.C`); `None` for a simple name.
    qualified: Option<String>,
}

/// Builds the scope tree and resolves references for one file.
pub(crate) struct Resolver {
    root: SyntaxNode,
    defs: Vec<Def>,
    scopes: Vec<Scope>,
    raw_refs: Vec<RawRef>,
}

impl Resolver {
    /// Creates a resolver rooted at `root` (the `SOURCE_FILE` node), seeded with the file scope.
    pub(crate) fn new(root: &SyntaxNode) -> Resolver {
        let r = root.text_range();
        let file_scope = Scope {
            id: ScopeId(0),
            kind: ScopeKind::File,
            parent: None,
            range: usize::from(r.start())..usize::from(r.end()),
            defs: Vec::new(),
        };
        Resolver {
            root: root.clone(),
            defs: Vec::new(),
            scopes: vec![file_scope],
            raw_refs: Vec::new(),
        }
    }

    /// Runs both passes and returns the result.
    pub(crate) fn run(mut self) -> Resolved {
        let root = self.root.clone();
        self.build(&root, ScopeId(0));

        let raw_refs = std::mem::take(&mut self.raw_refs);
        let mut references = Vec::with_capacity(raw_refs.len());
        for raw in raw_refs {
            // A qualified type name (`a.b.C`) never binds to a file-local definition; leave it
            // unresolved for the project layer, which resolves it against a fully-qualified name.
            let resolution = if raw.qualified.is_some() {
                Resolution::Unresolved
            } else {
                match self.lookup(raw.scope, &raw.name, raw.namespace, raw.range.start) {
                    Some(id) => Resolution::Def(id),
                    None => Resolution::Unresolved,
                }
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
        }
    }

    /// Creates a child scope of `parent` covering `node`, and returns its id.
    fn new_scope(&mut self, kind: ScopeKind, parent: ScopeId, node: &SyntaxNode) -> ScopeId {
        let id = ScopeId(self.scopes.len() as u32);
        let r = node.text_range();
        self.scopes.push(Scope {
            id,
            kind,
            parent: Some(parent),
            range: usize::from(r.start())..usize::from(r.end()),
            defs: Vec::new(),
        });
        id
    }

    /// Registers a definition named by `name_tok` in `scope`, and returns its id.
    fn add_def(&mut self, scope: ScopeId, kind: DefKind, name_tok: &SyntaxToken) -> DefId {
        let id = DefId(self.defs.len() as u32);
        self.defs.push(Def {
            id,
            kind,
            name: name_tok.text().to_string(),
            name_range: byte_range(name_tok),
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
        let Some(tok) = first_ident_token(node) else {
            return;
        };
        let namespace = if node.parent().map(|p| p.kind()) == Some(CALL_EXPR) {
            Namespace::Method
        } else {
            Namespace::Value
        };
        self.raw_refs.push(RawRef {
            range: byte_range(&tok),
            name: tok.text().to_string(),
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
        self.raw_refs.push(RawRef {
            range: byte_range(&tok),
            name: tok.text().to_string(),
            namespace: Namespace::Type,
            scope,
            qualified,
        });
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
