//! Project-wide (cross-file) type-name resolution: a symbol index over many files.
//!
//! The file-local pass ([`crate::resolve`]) leaves a type-name reference
//! [`Unresolved`](Resolution::Unresolved) whenever no
//! file-local definition names it — an imported type, a same-package sibling in another file, an
//! external (JDK / third-party) type. A [`ProjectIndex`] indexes the type *declarations* of every
//! source file by fully-qualified name and resolves those leftover references against them, using
//! Java's type-name lookup order (single-type import → same package → on-demand import → implicit
//! `java.lang` / unindexed). This is the basis for cross-file go-to-definition and the "cannot
//! resolve symbol" diagnostic.
//!
//! The layer is **pure**: [`ProjectIndex::build`] takes already-parsed CST roots, so the host
//! (the language server / CLI) owns all filesystem work and this module stays `wasm32`-compatible.
//! It never panics — an incomplete tree simply contributes whatever type declarations it has.
//!
//! Scope: **type names only**. Members (`obj.field`, method calls) need type inference and are left
//! to a later phase. External bytecode / the JDK classpath is not indexed; a name that *could* come
//! from there resolves to [`TypeResolution::External`] (no diagnostic) rather than
//! [`TypeResolution::Unresolved`], so the "cannot resolve" signal stays free of false positives.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;

use jals_syntax::SyntaxKind::{
    ANNOTATION_TYPE_DECL, CLASS_BODY, CLASS_DECL, CONSTRUCTOR_DECL, ELLIPSIS, ENUM_BODY,
    ENUM_CONSTANT, ENUM_DECL, EXTENDS_CLAUSE, FIELD_DECL, IMPLEMENTS_CLAUSE, INTERFACE_DECL,
    LBRACK, METHOD_DECL, RECORD_DECL,
};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use crate::Namespace;
use crate::def::DefKind;
use crate::reference::{Reference, Resolution};
use crate::resolve::Resolved;
use crate::resolve::collect::{byte_range, first_ident_token};

/// Identifies a file within a [`ProjectIndex`]. The host maps it to a path / URL; the index only
/// ever compares and stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub u32);

/// A fully-qualified type name, dotted at every level (`a.b.Outer.Inner`). Nested types use `.`
/// (the source-level canonical name), matching how imports spell them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fqn(String);

impl fmt::Display for Fqn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A dense identifier for an [`Item`] within one [`ProjectIndex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(u32);

/// An indexed type declaration: a class / interface / enum / record / annotation type, identified
/// by its fully-qualified name and locatable for go-to-definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The type's fully-qualified name.
    pub fqn: Fqn,
    /// Which kind of type declaration it is (always a [`Namespace::Type`] kind).
    pub kind: DefKind,
    /// The file the declaration lives in.
    pub file: FileId,
    /// The byte range of the declaring name token (the go-to-definition target).
    pub name_range: Range<usize>,
    /// The project-internal supertypes (`extends` / `implements` that resolve to indexed types), for
    /// inherited-member lookup. A supertype outside the indexed sources (a JDK class, an unresolved
    /// name) is simply absent, so a member search up the chain stops at it gracefully.
    pub supertypes: Vec<ItemId>,
    /// Whether any `extends` / `implements` clause names a type *outside* the indexed project (a JDK
    /// or third-party class). When true, this type may inherit members — including method overloads —
    /// that the index cannot see, so a "no member / no overload" conclusion is not trustworthy.
    pub has_external_supertype: bool,
}

/// A dense identifier for a [`Member`] within one [`ProjectIndex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MemberId(u32);

/// A member of an indexed type: a field, method, constructor, or enum constant. Methods and
/// constructors live in the [`Method`](Namespace::Method) name-space; fields and enum constants in
/// [`Value`](Namespace::Value), mirroring [`DefKind::namespace`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The type that declares this member.
    pub owner: ItemId,
    /// The member's simple name.
    pub name: String,
    /// What kind of member it is (`Field`, `Method`, `Constructor`, or `EnumConstant`).
    pub kind: DefKind,
    /// The file the declaration lives in.
    pub file: FileId,
    /// The byte range of the declaring name token (the go-to-definition target).
    pub name_range: Range<usize>,
    /// The member's declared value type — a field's type or a method's return type — captured as
    /// resolvable data (no CST handle), to be turned into a concrete type later (type inference) in
    /// this member's *declaring* file context. A constructor has none ([`MemberType::Unknown`]).
    pub ty: MemberType,
    /// A method's formal parameters, in order (each a name plus a type captured like
    /// [`ty`](Member::ty), resolved in the declaring file's context). Empty for non-methods. Used to
    /// check call arguments and to render signature help.
    pub params: Vec<Param>,
    /// Whether this method's last parameter is a varargs (`int... xs`). A varargs method accepts a
    /// variable arity, so argument checking skips it. Always `false` for non-methods.
    pub varargs: bool,
}

/// A method's formal parameter: its declared name (absent for a `_` / unreadable parameter) and its
/// type, captured as self-contained data like [`Member::ty`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// The parameter's declared name, or `None` for an unnamed (`_`) or unreadable parameter.
    pub name: Option<String>,
    /// The parameter's declared type.
    pub ty: MemberType,
}

/// A member's declared type, captured at index time as self-contained data so the [`ProjectIndex`]
/// holds no CST references. A named type keeps the spelling as written (simple, plus the full dotted
/// text when qualified) to be resolved against the declaring file later; array dimensions are kept
/// as a count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberType {
    /// A primitive type, by keyword spelling (`"int"`), with `dims` array levels (`int[]` → 1).
    Primitive { keyword: String, dims: u32 },
    /// The `void` return type.
    Void,
    /// A named reference type: its simple name, the full dotted text when written qualified, and the
    /// array dimension count. Resolved cross-file later, in the declaring file's import context.
    Named {
        name: String,
        qualified: Option<String>,
        dims: u32,
    },
    /// No resolvable value type — a constructor, a `var` slot, or a type that could not be read.
    Unknown,
}

/// The cross-file resolution of a type-name reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeResolution {
    /// Bound to a type declared somewhere in the indexed project sources.
    Project(ItemId),
    /// Not in the indexed sources, but plausibly provided from outside them — an import target, an
    /// `java.lang` name, or a name reachable through an on-demand import. Left unresolved on
    /// purpose, with **no** diagnostic: the JDK / classpath is not indexed, so this may well exist.
    External,
    /// Provably nameable from nowhere: no import, no same-package declaration, not `java.lang`.
    /// This is the "cannot resolve symbol" signal.
    Unresolved,
}

impl TypeResolution {
    /// The indexed project item this resolved to, or `None` for an [`External`](Self::External) /
    /// [`Unresolved`](Self::Unresolved) name. The counterpart to [`crate::Resolution::def_id`].
    pub fn project_id(self) -> Option<ItemId> {
        match self {
            TypeResolution::Project(id) => Some(id),
            TypeResolution::External | TypeResolution::Unresolved => None,
        }
    }
}

/// Per-file resolution context: its package and the imports that bring type names into scope.
struct FileMeta {
    /// The declared package, or `None` for the default (unnamed) package.
    package: Option<String>,
    /// Single-type imports `import a.b.Foo;` as `(simple name, fully-qualified name)`.
    single_imports: Vec<(String, String)>,
    /// On-demand imports `import a.b.*;` as the package prefix (`a.b`).
    on_demand: Vec<String>,
}

/// A symbol index over a set of source files, resolving type names across them.
pub struct ProjectIndex {
    items: Vec<Item>,
    by_fqn: HashMap<String, ItemId>,
    files: HashMap<FileId, FileMeta>,
    /// Every indexed member, by [`MemberId`].
    members: Vec<Member>,
    /// Members grouped by their declaring type, for member lookup.
    members_by_owner: HashMap<ItemId, Vec<MemberId>>,
    /// A type declaration's `(file, name-token start)` back to its [`ItemId`], so a *file-local*
    /// type reference (one that resolved to a same-file definition) can be mapped to its project
    /// item — the basis for whole-project find-references.
    decl_to_item: HashMap<(FileId, usize), ItemId>,
}

impl ProjectIndex {
    /// Builds the index from each file's `SOURCE_FILE` root. Pure: no I/O, never panics.
    ///
    /// Each file contributes its package, its type-name imports, and every type declaration it
    /// holds (top-level and nested). When two files declare the same fully-qualified name, the
    /// first one indexed wins.
    pub fn build(files: &[(FileId, SyntaxNode)]) -> ProjectIndex {
        let mut index = ProjectIndex {
            items: Vec::new(),
            by_fqn: HashMap::new(),
            files: HashMap::new(),
            members: Vec::new(),
            members_by_owner: HashMap::new(),
            decl_to_item: HashMap::new(),
        };
        for (file, root) in files {
            let Some(src) = ast::SourceFile::cast(root.clone()) else {
                continue;
            };
            let package = src
                .package()
                .and_then(|p| p.name())
                .map(|n| n.text())
                .filter(|p| !p.is_empty());

            let mut single_imports = Vec::new();
            let mut on_demand = Vec::new();
            for import in src.imports() {
                // Type-name resolution ignores static and module imports.
                if import.is_static() || import.is_module() {
                    continue;
                }
                let Some(name) = import.name() else {
                    continue;
                };
                if name.is_wildcard() {
                    if let Some(pkg) = name.qualifier() {
                        on_demand.push(pkg);
                    }
                } else if let Some(simple) = name.last_segment() {
                    single_imports.push((simple, name.text()));
                }
            }
            index.files.insert(
                *file,
                FileMeta {
                    package: package.clone(),
                    single_imports,
                    on_demand,
                },
            );
            index.collect_types(*file, root, package.as_deref(), None);
        }
        // Index each type's declaration site, so a same-file type reference (which resolves
        // file-locally, not through the project) can be mapped back to its item for find-references.
        for (i, item) in index.items.iter().enumerate() {
            index
                .decl_to_item
                .insert((item.file, item.name_range.start), ItemId(i as u32));
        }
        // Second pass: members and project-internal inheritance. It runs after every type is indexed
        // so a supertype declared later (or in another file) still resolves.
        for (file, root) in files {
            index.collect_members_and_supertypes(*file, root);
        }
        index
    }

    /// Walks `node`, recording each type declaration with its fully-qualified name and threading the
    /// enclosing type's FQN into its descendants (so a nested `Inner` becomes `Outer.Inner`).
    fn collect_types(
        &mut self,
        file: FileId,
        node: &SyntaxNode,
        package: Option<&str>,
        enclosing: Option<&str>,
    ) {
        let mut next_enclosing = enclosing.map(str::to_string);
        if let Some(kind) = type_decl_kind(node.kind())
            && let Some(name_tok) = first_ident_token(node)
        {
            let fqn = build_fqn(package, enclosing, name_tok.text());
            let id = ItemId(self.items.len() as u32);
            self.items.push(Item {
                fqn: Fqn(fqn.clone()),
                kind,
                file,
                name_range: byte_range(&name_tok),
                supertypes: Vec::new(),
                has_external_supertype: false,
            });
            self.by_fqn.entry(fqn.clone()).or_insert(id);
            next_enclosing = Some(fqn);
        }
        for child in node.children() {
            self.collect_types(file, &child, package, next_enclosing.as_deref());
        }
    }

    /// Walks `node`, recording each type declaration's direct members and resolving its
    /// project-internal supertypes. Runs in [`build`](ProjectIndex::build)'s second pass, when every
    /// type is already indexed (so a forward / cross-file supertype reference resolves).
    fn collect_members_and_supertypes(&mut self, file: FileId, node: &SyntaxNode) {
        if type_decl_kind(node.kind()).is_some()
            && let Some(name_tok) = first_ident_token(node)
            && let Some(owner) = self.item_by_decl(file, byte_range(&name_tok).start)
        {
            // Members. Captured purely from the node; pushed here so each gets a dense `MemberId`.
            for member in members_of_decl(owner, file, node, name_tok.text()) {
                let id = MemberId(self.members.len() as u32);
                self.members.push(member);
                self.members_by_owner.entry(owner).or_default().push(id);
            }
            // Supertypes: keep the ones that resolve to an indexed project type, and note whether any
            // resolves *outside* the project (so the type's full member set is not knowable).
            let mut supertypes = Vec::new();
            let mut has_external = false;
            for (name, qualified) in raw_supertypes_of(node) {
                match self.resolve_type_name(file, &name, qualified.as_deref()) {
                    TypeResolution::Project(id) => supertypes.push(id),
                    TypeResolution::External | TypeResolution::Unresolved => has_external = true,
                }
            }
            self.items[owner.0 as usize].supertypes = supertypes;
            self.items[owner.0 as usize].has_external_supertype = has_external;
        }
        for child in node.children() {
            self.collect_members_and_supertypes(file, &child);
        }
    }

    /// Resolves a simple type name `name` referenced from `file`, in Java's lookup order.
    fn resolve_type(&self, file: FileId, name: &str) -> TypeResolution {
        let Some(meta) = self.files.get(&file) else {
            // A file we never indexed: stay conservative and emit no diagnostic.
            return TypeResolution::External;
        };

        // 1. Single-type import. The import is proof the name exists; if it points into our sources
        //    we can go-to-def, otherwise it is external (e.g. a JDK type).
        if let Some((_, fqn)) = meta.single_imports.iter().find(|(s, _)| s == name) {
            return match self.by_fqn.get(fqn) {
                Some(&id) => TypeResolution::Project(id),
                None => TypeResolution::External,
            };
        }

        // 2. Same package (includes the file's own top-level types).
        if let Some(&id) = self.by_fqn.get(&qualify(meta.package.as_deref(), name)) {
            return TypeResolution::Project(id);
        }

        // 3. On-demand imports `import a.b.*;`. A single hit binds; several distinct hits are
        //    ambiguous, so we stay conservative and treat it as external (no diagnostic).
        let mut hits = meta
            .on_demand
            .iter()
            .filter_map(|pkg| self.by_fqn.get(&format!("{pkg}.{name}")).copied());
        if let Some(first) = hits.next() {
            return if hits.any(|other| other != first) {
                TypeResolution::External
            } else {
                TypeResolution::Project(first)
            };
        }

        // 4. Reachable from outside the index: an implicit `java.lang` type, or any on-demand
        //    import that could supply an unindexed type. Either way, no diagnostic.
        if is_java_lang(name) || !meta.on_demand.is_empty() {
            return TypeResolution::External;
        }

        // 5. Nameable from nowhere.
        TypeResolution::Unresolved
    }

    /// Resolves a qualified type name (`a.b.C`) referenced from `file`. A name we have indexed binds
    /// to it; any other fully-qualified name is taken to be external (no diagnostic), since the JDK
    /// and third-party classpath are not indexed.
    fn resolve_qualified(&self, qualified: &str) -> TypeResolution {
        match self.by_fqn.get(qualified) {
            Some(&id) => TypeResolution::Project(id),
            None => TypeResolution::External,
        }
    }

    /// Resolves the type-name `reference` (simple or qualified) from `file` against the project.
    pub fn resolve_reference(&self, file: FileId, reference: &Reference) -> TypeResolution {
        match &reference.qualified {
            Some(qualified) => self.resolve_qualified(qualified),
            None => self.resolve_type(file, &reference.name),
        }
    }

    /// Resolves a type *name* from `file`: the full dotted text when `qualified`, otherwise the
    /// simple `name` in Java's lookup order. This is the counterpart to
    /// [`resolve_reference`](ProjectIndex::resolve_reference) for a caller holding only a captured
    /// spelling rather than a CST [`Reference`] — namely a member's declared [`MemberType`], which
    /// inference turns into a concrete type.
    pub fn resolve_type_name(
        &self,
        file: FileId,
        name: &str,
        qualified: Option<&str>,
    ) -> TypeResolution {
        match qualified {
            Some(qualified) => self.resolve_qualified(qualified),
            None => self.resolve_type(file, name),
        }
    }

    /// The cross-file go-to-definition target for the reference covering byte `offset` in `file`,
    /// given that file's [`Resolved`]: a file-local definition if there is one, otherwise the
    /// project type the reference names. Returns the target file and the name's byte range.
    pub fn definition_at(
        &self,
        file: FileId,
        resolved: &Resolved,
        offset: usize,
    ) -> Option<(FileId, Range<usize>)> {
        if let Some(def) = resolved.definition_at(offset) {
            return Some((file, def.name_range.clone()));
        }
        let reference = resolved
            .references
            .iter()
            .find(|r| r.range.start <= offset && offset < r.range.end)?;
        if reference.namespace != Namespace::Type {
            return None;
        }
        match self.resolve_reference(file, reference) {
            TypeResolution::Project(id) => {
                let item = &self.items[id.0 as usize];
                Some((item.file, item.name_range.clone()))
            }
            TypeResolution::External | TypeResolution::Unresolved => None,
        }
    }

    /// The byte ranges of `file`'s type-name references that resolve to nothing — neither
    /// file-locally nor across the project. These are the "cannot resolve symbol" spans; a name
    /// that might come from outside the indexed sources is deliberately excluded.
    pub fn unresolved_types(&self, file: FileId, resolved: &Resolved) -> Vec<Range<usize>> {
        resolved
            .references
            .iter()
            .filter(|r| r.namespace == Namespace::Type && r.resolution == Resolution::Unresolved)
            .filter(|r| self.resolve_reference(file, r) == TypeResolution::Unresolved)
            .map(|r| r.range.clone())
            .collect()
    }

    /// The item with the given id.
    pub fn item(&self, id: ItemId) -> &Item {
        &self.items[id.0 as usize]
    }

    /// Every indexed type declaration.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.items.iter()
    }

    /// The member with the given id.
    pub fn member(&self, id: MemberId) -> &Member {
        &self.members[id.0 as usize]
    }

    /// The project item declared at `name_start` in `file`, if that position is a type declaration's
    /// name. Maps a file-local type definition back to its cross-file [`ItemId`].
    pub fn item_by_decl(&self, file: FileId, name_start: usize) -> Option<ItemId> {
        self.decl_to_item.get(&(file, name_start)).copied()
    }

    /// Resolves a member named `name` in name-space `namespace` (value for a field / enum constant,
    /// method for a method / constructor) on type `owner`, searching the type itself and then its
    /// project-internal supertypes.
    ///
    /// The nearest declaration wins (an own member shadows an inherited one), and the inheritance
    /// walk is cycle-guarded and stops at any supertype outside the index — the JDK and third-party
    /// members are simply not found (returns `None`) rather than guessed.
    pub fn resolve_member(
        &self,
        owner: ItemId,
        name: &str,
        namespace: Namespace,
    ) -> Option<MemberId> {
        self.walk_supertypes(owner, |current| {
            // The type's own members win over inherited ones — the walk reaches `current`'s
            // supertypes only after this returns `None`.
            let ids = self.members_by_owner.get(&current)?;
            ids.iter().copied().find(|&id| {
                let member = &self.members[id.0 as usize];
                member.name == name && member.kind.namespace() == namespace
            })
        })
    }

    /// Every member named `name` in name-space `namespace` reachable from `owner` (the type and its
    /// project-internal supertypes), nearest-first. Unlike [`resolve_member`](Self::resolve_member),
    /// which returns the single nearest match, this returns *all* candidates — the overload set a
    /// call's arguments must be checked against.
    pub fn resolve_members_all(
        &self,
        owner: ItemId,
        name: &str,
        namespace: Namespace,
    ) -> Vec<MemberId> {
        let mut out = Vec::new();
        // Always returning `None` walks the whole (cycle-guarded) chain, accumulating into `out`.
        self.walk_supertypes(owner, |current| {
            if let Some(ids) = self.members_by_owner.get(&current) {
                for &id in ids {
                    let member = &self.members[id.0 as usize];
                    if member.name == name && member.kind.namespace() == namespace {
                        out.push(id);
                    }
                }
            }
            None::<()>
        });
        out
    }

    /// Whether the full set of overloads named `name` on `owner` is knowable from the index — a
    /// precondition for concluding "no overload matches" without a false positive.
    ///
    /// It is *not* knowable when `name` is an [`Object`](is_object_method) method (every type inherits
    /// `Object`'s overloads, which are not indexed) or when `owner` or any project supertype `extends`
    /// / `implements` a type outside the project (which may declare further overloads we cannot see).
    pub fn method_set_complete(&self, owner: ItemId, name: &str) -> bool {
        if is_object_method(name) {
            return false;
        }
        self.walk_supertypes(owner, |current| {
            self.items[current.0 as usize]
                .has_external_supertype
                .then_some(())
        })
        .is_none()
    }

    /// Whether project type `s` is `t` or a transitive subtype of it, walking `s`'s indexed
    /// supertype chain. Reflexive (`s == t` is `true`) and cycle-guarded — the reference-subtyping
    /// half of assignment conversion ([`Ty::is_assignable_to`](crate::Ty::is_assignable_to)).
    pub fn is_subtype(&self, s: ItemId, t: ItemId) -> bool {
        self.walk_supertypes(s, |current| (current == t).then_some(()))
            .is_some()
    }

    /// Walks `start` and its project-internal supertypes, each visited once with a cycle guard in
    /// nearest-first / earlier-declared-first order, calling `visit` on each. The first `Some`
    /// `visit` yields stops the walk and is returned; an exhausted walk yields `None`. The shared
    /// inheritance traversal behind member resolution ([`resolve_member`](Self::resolve_member)) and
    /// subtyping ([`is_subtype`](Self::is_subtype)).
    fn walk_supertypes<R>(
        &self,
        start: ItemId,
        mut visit: impl FnMut(ItemId) -> Option<R>,
    ) -> Option<R> {
        let mut visited = HashSet::new();
        let mut stack = vec![start];
        while let Some(current) = stack.pop() {
            if !visited.insert(current) {
                continue;
            }
            if let Some(result) = visit(current) {
                return Some(result);
            }
            for &supertype in self.items[current.0 as usize].supertypes.iter().rev() {
                stack.push(supertype);
            }
        }
        None
    }
}

/// The direct value/executable members of a type declaration `node` (owned by `owner`, in `file`):
/// fields, methods, constructors, and enum constants. Nested type declarations are *not* members
/// here — they are their own [`Item`]s. `owner_simple` is the declaring type's simple name, used as
/// an enum constant's type. Pure: reads only the node.
fn members_of_decl(
    owner: ItemId,
    file: FileId,
    node: &SyntaxNode,
    owner_simple: &str,
) -> Vec<Member> {
    let mut members = Vec::new();
    // The body holds the members directly (a `ClassBody`, or an `EnumBody` whose constants and
    // members are both direct children).
    let Some(body) = node
        .children()
        .find(|c| matches!(c.kind(), CLASS_BODY | ENUM_BODY))
    else {
        return members;
    };
    let mut push =
        |name_tok: &SyntaxToken, kind: DefKind, ty: MemberType, params: Vec<Param>, varargs| {
            members.push(Member {
                owner,
                name: name_tok.text().to_string(),
                kind,
                file,
                name_range: byte_range(name_tok),
                ty,
                params,
                varargs,
            });
        };
    for member in body.children() {
        match member.kind() {
            FIELD_DECL => {
                if let Some(field) = ast::FieldDecl::cast(member.clone()) {
                    let ty = member_type_of(field.ty());
                    for name in field.names() {
                        push(&name, DefKind::Field, ty.clone(), Vec::new(), false);
                    }
                }
            }
            METHOD_DECL => {
                if let Some(name) = first_ident_token(&member) {
                    let ty = member_type_of(
                        ast::MethodDecl::cast(member.clone()).and_then(|m| m.return_type()),
                    );
                    let (params, varargs) = params_of(&member);
                    push(&name, DefKind::Method, ty, params, varargs);
                }
            }
            CONSTRUCTOR_DECL => {
                if let Some(name) = first_ident_token(&member) {
                    push(
                        &name,
                        DefKind::Constructor,
                        MemberType::Unknown,
                        Vec::new(),
                        false,
                    );
                }
            }
            ENUM_CONSTANT => {
                if let Some(name) = first_ident_token(&member) {
                    let ty = MemberType::Named {
                        name: owner_simple.to_string(),
                        qualified: None,
                        dims: 0,
                    };
                    push(&name, DefKind::EnumConstant, ty, Vec::new(), false);
                }
            }
            _ => {}
        }
    }
    members
}

/// A method declaration's formal parameters (in order) and whether it is varargs (its last
/// parameter is `int... xs`). Each parameter's name and type are captured as self-contained data,
/// the type like a field's. Pure.
fn params_of(method: &SyntaxNode) -> (Vec<Param>, bool) {
    let mut params = Vec::new();
    let mut varargs = false;
    if let Some(list) = ast::MethodDecl::cast(method.clone()).and_then(|m| m.params()) {
        for param in list.params() {
            if param
                .syntax()
                .children_with_tokens()
                .filter_map(|it| it.into_token())
                .any(|t| t.kind() == ELLIPSIS)
            {
                varargs = true;
            }
            params.push(Param {
                name: param.name(),
                ty: member_type_of(param.ty()),
            });
        }
    }
    (params, varargs)
}

/// The supertype type names of a type declaration `node`: the `extends` and `implements` clause
/// types, each as `(simple name, full dotted text if qualified)`. Pure.
fn raw_supertypes_of(node: &SyntaxNode) -> Vec<(String, Option<String>)> {
    let mut supertypes = Vec::new();
    for clause in node
        .children()
        .filter(|c| matches!(c.kind(), EXTENDS_CLAUSE | IMPLEMENTS_CLAUSE))
    {
        for ty in clause.children().filter_map(ast::Type::cast) {
            if let Some(name) = ty.simple_name() {
                let qualified = ty.qualified_text().filter(|q| q.contains('.'));
                supertypes.push((name, qualified));
            }
        }
    }
    supertypes
}

/// Captures a member's declared type (`ast::Type`) as a self-contained [`MemberType`]. `None` (a
/// missing type) and a `var` type are [`MemberType::Unknown`].
fn member_type_of(ty: Option<ast::Type>) -> MemberType {
    let Some(ty) = ty else {
        return MemberType::Unknown;
    };
    // One pass over the type's direct tokens: count array `[`s and capture the leading keyword.
    let mut dims = 0u32;
    let mut keyword: Option<SyntaxToken> = None;
    for token in ty
        .syntax()
        .children_with_tokens()
        .filter_map(|it| it.into_token())
    {
        match token.kind() {
            LBRACK => dims += 1,
            k if keyword.is_none() && !k.is_trivia() => keyword = Some(token),
            _ => {}
        }
    }
    if ty.is_primitive_or_var() {
        match keyword.as_ref().map(SyntaxToken::text) {
            Some("void") => MemberType::Void,
            Some("var") | None => MemberType::Unknown,
            Some(k) => MemberType::Primitive {
                keyword: k.to_string(),
                dims,
            },
        }
    } else {
        MemberType::Named {
            name: ty.simple_name().unwrap_or_default(),
            qualified: ty.qualified_text().filter(|q| q.contains('.')),
            dims,
        }
    }
}

/// The [`DefKind`] for a type-declaration node kind, or `None` if it is not a type declaration.
pub(crate) fn type_decl_kind(kind: SyntaxKind) -> Option<DefKind> {
    match kind {
        CLASS_DECL => Some(DefKind::Class),
        INTERFACE_DECL => Some(DefKind::Interface),
        ENUM_DECL => Some(DefKind::Enum),
        RECORD_DECL => Some(DefKind::Record),
        ANNOTATION_TYPE_DECL => Some(DefKind::AnnotationType),
        _ => None,
    }
}

/// Builds a fully-qualified name. A nested type appends to its enclosing type's FQN (which already
/// carries the package); a top-level type prepends the package, if any.
fn build_fqn(package: Option<&str>, enclosing: Option<&str>, simple: &str) -> String {
    match enclosing {
        Some(e) => format!("{e}.{simple}"),
        None => qualify(package, simple),
    }
}

/// Qualifies `simple` with `package` (`Some("a.b")` → `a.b.Simple`; `None` → `Simple`).
fn qualify(package: Option<&str>, simple: &str) -> String {
    match package {
        Some(p) => format!("{p}.{simple}"),
        None => simple.to_string(),
    }
}

/// Whether `name` is a method every type inherits from `java.lang.Object`. A call to one of these
/// may bind to `Object`'s declaration (not indexed), so the project member set for it is incomplete.
fn is_object_method(name: &str) -> bool {
    const OBJECT_METHODS: &[&str] = &[
        "equals",
        "hashCode",
        "toString",
        "getClass",
        "clone",
        "notify",
        "notifyAll",
        "wait",
        "finalize",
    ];
    OBJECT_METHODS.contains(&name)
}

/// Whether `name` is a commonly-used implicit `java.lang` type (imported into every file). Kept
/// small and conservative: it only needs to cover the names that would otherwise produce false
/// "cannot resolve" diagnostics in files with no imports.
fn is_java_lang(name: &str) -> bool {
    const JAVA_LANG: &[&str] = &[
        "Object",
        "String",
        "CharSequence",
        "StringBuilder",
        "StringBuffer",
        "Number",
        "Byte",
        "Short",
        "Integer",
        "Long",
        "Float",
        "Double",
        "Boolean",
        "Character",
        "Void",
        "Math",
        "System",
        "Runtime",
        "Process",
        "Thread",
        "Runnable",
        "Iterable",
        "Comparable",
        "Cloneable",
        "AutoCloseable",
        "Class",
        "Enum",
        "Record",
        "Throwable",
        "Error",
        "Exception",
        "RuntimeException",
        "IllegalArgumentException",
        "IllegalStateException",
        "NullPointerException",
        "IndexOutOfBoundsException",
        "UnsupportedOperationException",
        "ClassCastException",
        "ArithmeticException",
        "Override",
        "Deprecated",
        "SuppressWarnings",
        "FunctionalInterface",
        "SafeVarargs",
    ];
    JAVA_LANG.contains(&name)
}
