//! The semantic analysis of one file, as a value.
//!
//! The crate's three layers — file-local name resolution, the project-wide [`ProjectIndex`], and
//! type inference — have a fixed order: resolve a file, index the project, then infer types
//! against both. Before this module every consumer spelled that order itself, and each one
//! re-ran the (whole-file) inference for every question it asked.
//!
//! The order lives here instead, in two values that differ by exactly what they know — plus a
//! third, [`TypedFile`], that is the witness the last step has run:
//!
//! | | knows | who holds one |
//! |---|---|---|
//! | [`FileAnalysis`] | one file's CST and name resolution | a host, cached per file |
//! | [`FileSemantics`] | that file **within** a [`ProjectIndex`] | one request, one lint pass, one compile |
//!
//! [`FileAnalysis`] depends on nothing but the file, so it survives an index rebuild and is the
//! half worth caching. [`FileSemantics`] borrows the index, so it *cannot* outlive the index it
//! was computed against — the staleness rule a hand-written inference cache would have to police
//! is not expressible here, which is why the memo lives on it. Everything that needs types goes
//! through [`FileSemantics::typed`], so a caller asking several questions pays for one inference.
//!
//! [`TypedFile`] is what that memo hands out: a file whose inference has run, and therefore the
//! only place per-node types are readable **without** an `await`. That is what a code generator
//! needs — `jals-javac` reads a type per expression on straight-line paths — and what an editor
//! query must not do, which is why it is reached only through [`FileSemantics::typed`].
//!
//! [`Resolved`] and [`TypeInference`] are the intermediate states of this pipeline and are not
//! exported: holding one would be holding a step, and the point of this module is that the steps
//! are not separately orderable.

use alloc::vec::Vec;
use core::cell::OnceCell;
use core::ops::Range;

use jals_syntax::SyntaxKind::NAME_REF;
use jals_syntax::cfg::CfgMap;
use jals_syntax::{SyntaxNode, SyntaxToken, TextSize};

use crate::def::{Def, DefId};
use crate::infer::TypeInference;
use crate::project::{FileId, MemberId, ProjectIndex};
use crate::reference::Reference;
use crate::resolve::Resolved;
use crate::resolve::collect::Collect;
use crate::ty::Ty;

/// One source file, parsed and name-resolved.
///
/// The half of semantic analysis that depends on the file alone: nothing here can be invalidated
/// by a change to another file, and nothing here can be stale with respect to a [`ProjectIndex`],
/// because it never saw one. That is what makes it the half a host caches per file — the language
/// server keeps one per open document and rebuilds only the one that was edited.
///
/// Bind it to a project with [`in_project`](Self::in_project) to reach everything that needs
/// cross-file facts. What it answers on its own is what needs no project: the file's own
/// bindings and references, and the two analyses that are file-local by nature.
// No `PartialEq`: a `SyntaxNode` handle compares by tree *identity*, so a derived one would answer
// "same parse instance" while reading as "same analysis". Compare `defs()` / `references()` instead.
#[derive(Debug, Clone)]
pub struct FileAnalysis {
    root: SyntaxNode,
    resolved: Resolved,
}

impl FileAnalysis {
    /// Parse `src` and resolve names within it.
    pub async fn parse(src: &str) -> Self {
        Self::of(&jals_syntax::Parse::parse(src).await.syntax()).await
    }

    /// Resolve names over an already-parsed CST `root` (the `SOURCE_FILE` node).
    ///
    /// The half a caller holding a cached parse tree calls without reparsing — the language
    /// server, which keeps an `Arc<Parse>` per document, and the compile backend, which parsed
    /// the staged tree.
    pub async fn of(root: &SyntaxNode) -> Self {
        Self::of_with_cfg(root, &CfgMap::default()).await
    }

    /// Like [`of`](Self::of), but skipping every `cfg`-disabled host in `cfg` (computed over the
    /// same text as `root`): a disabled declaration contributes no definition and nothing inside
    /// it is recorded as a reference — the analysis-side mirror of the compile frontend blanking
    /// the host. An empty (default) map analyses identically to [`of`](Self::of).
    pub async fn of_with_cfg(root: &SyntaxNode, cfg: &CfgMap) -> Self {
        Self {
            root: root.clone(),
            resolved: Resolved::resolve_node_with_cfg(root, cfg).await,
        }
    }

    /// The `SOURCE_FILE` node this was resolved over.
    pub const fn root(&self) -> &SyntaxNode {
        &self.root
    }

    /// The file's name resolution. Crate-internal: it is a step, not a result.
    pub(crate) const fn resolved(&self) -> &Resolved {
        &self.resolved
    }

    /// Every definition in the file, in [`DefId`] order.
    pub fn defs(&self) -> &[Def] {
        &self.resolved.defs
    }

    /// The definition with the given id.
    pub fn def(&self, id: DefId) -> &Def {
        self.resolved.def(id)
    }

    /// Every examined reference, sorted by start offset.
    pub fn references(&self) -> &[Reference] {
        &self.resolved.references
    }

    /// The reference covering byte `offset`, if any.
    pub fn reference_at(&self, offset: usize) -> Option<&Reference> {
        self.resolved.reference_at(offset)
    }

    /// The `NAME_REF` node `reference` is written in.
    ///
    /// The syntax behind the fact: the resolver saw this node when it recorded the reference, and
    /// every consumer that wanted it back — the "cannot resolve" pass here, the `implicit-this`
    /// rule in `jals-lint` — was re-walking the whole file to rebuild the same offset-keyed index
    /// the resolver had already thrown away.
    ///
    /// Answered by descending to the offset rather than from a map, because the map is the
    /// expensive half: a file-wide index costs every caller a full walk, while the callers that
    /// exist ask about a *filtered* handful of references and would leave most of it unread. That
    /// is an implementation choice behind this signature, not a promise — a memo can replace it
    /// without a caller noticing.
    ///
    /// `None` in two cases, and they mean different things. A **type** reference is recorded from
    /// the `TYPE` node it names rather than from a `NAME_REF`, so it never has a site and asking is
    /// simply the wrong question — every caller filters to
    /// [`Value`](crate::Namespace::Value) / [`Method`](crate::Namespace::Method) first. Otherwise
    /// `None` means the reference and the tree disagree, which is exactly the case not to conclude
    /// anything from.
    pub fn site_of(&self, reference: &Reference) -> Option<SyntaxNode> {
        let token = Self::token_starting_at(&self.root, reference.range.start)?;
        let site = token
            .parent_ancestors()
            .find(|node| node.kind() == NAME_REF)?;
        // The site must be the one this reference *names*, not an outer `NAME_REF` that merely
        // contains the offset.
        let first = Collect::first_ident_token(&site)?;
        (usize::from(first.text_range().start()) == reference.range.start).then_some(site)
    }

    /// The declaration node `def` was registered from.
    ///
    /// The `Def` half of [`site_of`](Self::site_of). It is the *declaration* and not the name's
    /// parent, which is the distinction that matters for a multi-declarator: `int a, b;` is one
    /// `FIELD_DECL` binding two definitions, and both answer with that one node.
    pub fn decl_of(&self, def: &Def) -> Option<SyntaxNode> {
        let span = self.resolved.decl_span(def.id);
        let token = Self::token_starting_at(&self.root, def.name_range.start)?;
        // The declaring node is always an ancestor of the name it binds, so the recorded span
        // identifies it without a list of declaration kinds to keep in step with the resolver.
        token
            .parent_ancestors()
            .find(|node| Collect::node_span(node) == *span)
    }

    /// The token that begins at byte `offset`, if one does.
    ///
    /// Right-biased because an offset on a token boundary sits *between* two tokens, and both a
    /// `Def`'s `name_range` and a `Reference`'s `range` start at the identifier they name.
    fn token_starting_at(root: &SyntaxNode, offset: usize) -> Option<SyntaxToken> {
        let at = TextSize::from(u32::try_from(offset).ok()?);
        let token = root.token_at_offset(at).right_biased()?;
        (usize::from(token.text_range().start()) == offset).then_some(token)
    }

    /// The definition the cursor at byte `offset` denotes, whether the cursor sits on a
    /// *reference* to it or on its own declaring name.
    pub fn symbol_at(&self, offset: usize) -> Option<DefId> {
        self.resolved.symbol_at(offset)
    }

    /// The declaration of `id` (when `include_declaration`) together with every reference to it,
    /// as byte ranges in document order.
    pub fn occurrences(&self, id: DefId, include_declaration: bool) -> Vec<Range<usize>> {
        self.resolved.occurrences(id, include_declaration)
    }

    /// Every definition nothing in this file can be denoting.
    ///
    /// The raw signal for unused-binding diagnostics, and a deliberate **over-approximation of
    /// use**: a definition is withheld not only when a reference resolves to it, but also when it
    /// is in the method name-space and any call spells its name (the scope chain binds a call to
    /// *an* overload rather than to the one the arguments select), and when it is a kind a member
    /// access could name and its name is spelled where the file-local pass cannot bind it
    /// (`this.x`, `Outer.Inner`, `X.class`, `@Anno`, the ambiguous-name qualifier of JLS §6.5.2,
    /// and anything inside a `cfg`-disabled host). "Unused" is a negative, and the only direction
    /// it may err in is silence.
    ///
    /// Still only *file-local*: an unreferenced field or method may be used from another file, so
    /// a consumer narrows it to the kinds — and the visibility ([`Def::is_private`]) — whose answer
    /// one file completes.
    pub fn unused_defs(&self) -> impl Iterator<Item = &Def> {
        self.resolved.unused_defs()
    }

    /// Bind this file to the project index it is analysed in.
    ///
    /// Cheap and **synchronous** — no inference runs here. `file` is this file's identity within
    /// `index`; the caller owns that convention (the editor partitions the id space, the compile
    /// backend numbers by position) and this never invents one.
    pub const fn in_project<'a>(
        &'a self,
        index: &'a ProjectIndex,
        file: FileId,
    ) -> FileSemantics<'a> {
        FileSemantics {
            analysis: self,
            index,
            file,
            types: OnceCell::new(),
        }
    }
}

/// One file, bound to the project index it is analysed in.
///
/// This is where `parse → resolve → index → infer` becomes a single value: the resolution, the
/// index, and the file's identity within it, plus the inference that needs all three. Inference
/// runs **at most once** per binding, on first demand, and every query below shares that one run.
///
/// Cheap to construct and meant to be short-lived — one editor request, one lint pass, one file's
/// compile. It borrows the index, so a host cannot store one across an index rebuild; that is
/// deliberate, and it is what makes the memo safe without a generation counter.
pub struct FileSemantics<'a> {
    analysis: &'a FileAnalysis,
    index: &'a ProjectIndex,
    file: FileId,
    types: OnceCell<TypeInference>,
}

impl<'a> FileSemantics<'a> {
    /// The file's own analysis, independent of this project.
    pub const fn analysis(&self) -> &'a FileAnalysis {
        self.analysis
    }

    /// The project index this file is bound to.
    pub const fn index(&self) -> &'a ProjectIndex {
        self.index
    }

    /// This file's identity within [`index`](Self::index).
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// The `SOURCE_FILE` node, for a caller that has only the binding. Crate-internal: outside the
    /// crate the same node is reached through [`analysis`](Self::analysis), and every analysis that
    /// needs the tree *and* the project lives in here.
    pub(crate) const fn root(&self) -> &'a SyntaxNode {
        self.analysis.root()
    }

    /// The file's name resolution. Crate-internal, like
    /// [`FileAnalysis::resolved`](FileAnalysis::resolved): it is a step, not a result.
    pub(crate) const fn resolved(&self) -> &'a Resolved {
        self.analysis.resolved()
    }

    /// The file's type inference, computed once and shared by every query that needs types.
    ///
    /// The async-once shape (compute, then publish) is the one already used by the editor's
    /// per-file resolution cache and the lint driver's: this runtime is single-threaded, but two
    /// queries interleaved at an await point can both see the empty cell and both compute — the
    /// value is a pure function of the file, the index, and the id, so the duplicate work is
    /// benign and the first `set` wins. No locking keeps it cancellation-safe: a dropped query
    /// leaves the cell either empty or fully published.
    pub async fn typed(&self) -> TypedFile<'_> {
        if self.types.get().is_none() {
            let computed = TypeInference::infer(
                self.analysis.root(),
                self.analysis.resolved(),
                self.index,
                self.file,
            )
            .await;
            let _ = self.types.set(computed);
        }
        TypedFile {
            analysis: self.analysis,
            index: self.index,
            file: self.file,
            types: self.types.get().expect("published just above"),
        }
    }
}

/// A file whose type inference has run: the per-node type answers, synchronously.
///
/// The only value in this crate that reads inferred types without an `await`, which is exactly
/// what a code generator needs — `jals-javac` holds one while lowering and reads a type per
/// expression — and exactly what an editor query must not do, which is why it is reached only
/// through [`FileSemantics::typed`]. Being reachable only from there is also what lets every
/// accessor below be total: the inference is already there.
#[derive(Clone, Copy)]
pub struct TypedFile<'s> {
    analysis: &'s FileAnalysis,
    index: &'s ProjectIndex,
    file: FileId,
    types: &'s TypeInference,
}

impl<'s> TypedFile<'s> {
    /// The file's own analysis.
    pub const fn analysis(&self) -> &'s FileAnalysis {
        self.analysis
    }

    /// The project index this file was inferred against.
    pub const fn index(&self) -> &'s ProjectIndex {
        self.index
    }

    /// This file's identity within [`index`](Self::index).
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// The `SOURCE_FILE` node.
    pub const fn root(&self) -> &'s SyntaxNode {
        self.analysis.root()
    }

    /// The inference itself. Crate-internal: it is a step, not a result.
    pub(crate) const fn inference(&self) -> &'s TypeInference {
        self.types
    }

    /// The declared / inferred type of a definition, [`Ty::Unknown`] where inference had no
    /// answer.
    pub fn type_of_def(&self, id: DefId) -> &'s Ty {
        self.types.type_of_def(id)
    }

    /// The type of the expression spanning exactly `span`, if one was inferred there.
    pub fn type_of_expr(&self, span: Range<usize>) -> Option<&'s Ty> {
        self.types.type_of_expr(span)
    }

    /// The type of the innermost expression covering byte `offset` — the hover query.
    pub fn type_at(&self, offset: usize) -> Option<&'s Ty> {
        self.types.type_at(offset)
    }

    /// The member the call spanning exactly `span` binds to.
    pub fn call_target_of(&self, span: Range<usize>) -> Option<MemberId> {
        self.types.call_target_of(span)
    }

    /// The field or enum constant the member access spanning exactly `span` binds to.
    pub fn field_target_of(&self, span: Range<usize>) -> Option<MemberId> {
        self.types.field_target_of(span)
    }
}

#[cfg(test)]
mod tests {
    use jals_exec::block_on_inline;

    use super::{FileAnalysis, FileId, ProjectIndex};

    /// The memo is the whole point of binding a file to a project: every query that needs types
    /// must share one inference. Pointer equality is the only way to say that from outside — two
    /// separate runs would compare equal on every observable answer, which is exactly why an
    /// accidental re-infer would go unnoticed.
    ///
    /// This lives in `src` because it reads the crate-internal inference the binding hands out; the
    /// alternative was exporting a step to test that the steps are not separately orderable.
    #[test]
    fn a_binding_infers_once_and_shares_it() {
        let src = "class C { int x; int get() { return x; } }";
        let analysis = block_on_inline(FileAnalysis::parse(src));
        let index =
            block_on_inline(ProjectIndex::builder(&[(FileId(0), analysis.root().clone())]).build());
        let semantics = analysis.in_project(&index, FileId(0));

        let first = block_on_inline(semantics.typed());
        let second = block_on_inline(semantics.typed());
        assert!(
            core::ptr::eq(first.inference(), second.inference()),
            "a second query must read the first query's inference, not run its own"
        );

        // A *separate* binding is a separate memo — that is what makes it safe to hold one only for
        // as long as the index it was computed against.
        let other = analysis.in_project(&index, FileId(0));
        let elsewhere = block_on_inline(other.typed());
        assert!(!core::ptr::eq(first.inference(), elsewhere.inference()));
    }
}
