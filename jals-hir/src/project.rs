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
//! The layer is **pure**: [`ProjectIndex::builder`] takes already-parsed CST roots, so the host
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

impl Fqn {
    /// The simple (unqualified) name: the last dotted segment (`a.b.Outer.Inner` → `Inner`). Correct
    /// because every level — packages and nested types alike — is dotted.
    pub fn simple_name(&self) -> &str {
        self.0.rsplit('.').next().unwrap_or(&self.0)
    }
}

impl fmt::Display for Fqn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A dense identifier for an [`Item`] within one [`ProjectIndex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(u32);

/// Where an indexed [`Item`] comes from: the project's own sources, a `git`/`path` dependency's
/// sources, an external `.class` file, or an embedded standard-library stub. All are indexed by the
/// same machinery but treated differently at the edges — e.g. a stub has no real file the host can
/// open, so navigation into it is suppressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOrigin {
    /// Declared in one of the project source files the host supplied to [`ProjectIndex::builder`].
    Project,
    /// Declared in an embedded `java.lang` stub (see [`crate::stdlib`]); present only via
    /// [`ProjectIndexBuilder::with_stdlib`]. Carries signatures for inference and hover, but no
    /// host-openable location.
    Stdlib,
    /// Decoded from a `.class` file on the classpath (see [`crate::classpath`]); present only via
    /// [`ProjectIndexBuilder::with_classpath`]. Like a stub it has no host-openable source, but its
    /// declared member set is *complete* for that class, so it is not treated leniently the way a
    /// (deliberately partial) stub is.
    Classpath,
    /// Declared in an external **library source** file — the `.java` of a `git`/`path`
    /// `[dependencies]` entry the host folds in via
    /// [`with_source_deps`](ProjectIndexBuilder::with_source_deps). Indexed from real source (so
    /// its member set is complete and it is *not* treated leniently) and locatable at its real
    /// [`file`](Item::file) / [`name_range`](Item::name_range), so it resolves types and is a
    /// go-to-definition target — yet it is not one of the project's own files, so the host never lints
    /// or renames it.
    Source,
}

impl ItemOrigin {
    /// Whether an item of this origin lives in a file the host owns and may rewrite — the only origin
    /// the LSP renames or treats as a project input. Every other origin (a `java.lang` stub, a
    /// classpath `.class`, or a `git`/`path` library source) is external: navigable at most, never
    /// edited. An exhaustive match so a new origin must explicitly opt in here rather than silently
    /// becoming renamable.
    pub fn is_host_editable(self) -> bool {
        match self {
            ItemOrigin::Project => true,
            ItemOrigin::Stdlib | ItemOrigin::Classpath | ItemOrigin::Source => false,
        }
    }
}

/// An indexed type declaration: a class / interface / enum / record / annotation type, identified
/// by its fully-qualified name and locatable for go-to-definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The type's fully-qualified name.
    pub fqn: Fqn,
    /// Which kind of type declaration it is (always a [`Namespace::Type`] kind).
    pub kind: DefKind,
    /// Whether this comes from the project sources or an embedded stub.
    pub origin: ItemOrigin,
    /// The file the declaration lives in.
    pub file: FileId,
    /// The byte range of the declaring name token (the go-to-definition target).
    pub name_range: Range<usize>,
    /// The type's own type parameters in declaration order (`class Box<T>` → one `T`). Empty for a
    /// non-generic type. Recorded so generic member substitution (a later phase) can bind them to the
    /// arguments a use supplies.
    pub type_params: Vec<TypeParamDecl>,
    /// The project-internal supertypes (`extends` / `implements` that resolve to indexed types), each
    /// with the type arguments the clause supplies (`extends Container<String>` → `[String]`), for
    /// inherited-member lookup. A supertype outside the indexed sources (a JDK class, an unresolved
    /// name) is simply absent, so a member search up the chain stops at it gracefully.
    pub supertypes: Vec<Supertype>,
    /// Whether any `extends` / `implements` clause names a type *outside* the indexed project (a JDK
    /// or third-party class). When true, this type may inherit members — including method overloads —
    /// that the index cannot see, so a "no member / no overload" conclusion is not trustworthy.
    pub has_external_supertype: bool,
    /// A real-source go-to-definition override, for a [`Classpath`](ItemOrigin::Classpath) type whose
    /// library *sources* jar is available: the `(file, name range)` of the matching `.java`
    /// declaration. `None` for a project type (its own [`file`](Item::file) / [`name_range`](Item::name_range)
    /// already point at real source) and for a classpath/stub type with no source. Kept separate from
    /// [`file`](Item::file), which doubles as the type's member-resolution context (a classpath
    /// pseudo-file) and must not be repointed at the source.
    pub source_location: Option<(FileId, Range<usize>)>,
}

/// A type's declared type parameter (`<T>`, `<T extends Number>`): its name and its bounds, captured
/// as self-contained data like a [`Member`]'s type, to be resolved / substituted in a later phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeParamDecl {
    /// The parameter's name (`T`).
    pub name: String,
    /// The upper bounds after `extends` (`<T extends A & B>` → `[A, B]`); empty for an unbounded
    /// parameter (implicitly `Object`).
    pub bounds: Vec<MemberType>,
}

/// A resolved project-internal supertype: the indexed type it names, plus the type arguments the
/// `extends` / `implements` clause supplied to it (`extends Container<String>` → `[String]`). The
/// arguments are kept for generic inherited-member substitution; plain subtyping ignores them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supertype {
    /// The indexed supertype.
    pub id: ItemId,
    /// The type arguments supplied to it, captured like a [`Member`]'s type; empty for a raw use.
    pub args: Vec<MemberType>,
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
    /// A real-source go-to-definition override for a classpath member whose library *sources* jar is
    /// available — the `(file, name range)` of the matching `.java` declaration. `None` for a project
    /// member and for a classpath member with no source. Kept separate from [`file`](Member::file),
    /// which is the member's type-resolution context and must stay the classpath pseudo-file.
    pub source_location: Option<(FileId, Range<usize>)>,
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
    /// A named reference type: its simple name, the full dotted text when written qualified, the
    /// array dimension count, and its type arguments. Resolved cross-file later, in the declaring
    /// file's import context.
    Named {
        name: String,
        qualified: Option<String>,
        dims: u32,
        /// The type arguments as written (`List<String>` → `[String]`, `Map<K, V>` → `[K, V]`),
        /// each captured recursively like the outer type. Empty for a raw / non-generic use. A bare
        /// wildcard (`<?>`) is not captured. Resolved (and, later, substituted) in the declaring
        /// file's context.
        args: Vec<MemberType>,
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

/// A project's classpath `.class` files lowered to the index facts they contribute, ready to fold
/// into a [`ProjectIndex`]. Produced once by [`ProjectIndex::lower_classpath`] and reused across
/// rebuilds via [`ProjectIndexBuilder::with_classpath`], so a host that re-indexes on every
/// edit decodes the (unchanging) classpath only once.
pub struct LoweredClasspath {
    classes: Vec<crate::classpath::ClassfileClass>,
}

/// Where the types and members of a project's **library sources** are declared, keyed so a
/// `.class`-derived [`Classpath`](ItemOrigin::Classpath) item can be pointed at its real `.java`
/// declaration for go-to-definition. Built once by
/// [`index_source_locations`](ProjectIndex::index_source_locations) from the host's extracted library
/// sources (the `sources` jars of `[dependencies]`) and folded in by
/// [`ProjectIndexBuilder::with_source_locations`]. Pure data — the host
/// owns the file I/O and maps each [`FileId`] back to a real URL.
#[derive(Debug, Default)]
pub struct SourceLocations {
    /// Each type's fully-qualified name to its declaring `(file, name-token range)`.
    types: HashMap<String, (FileId, Range<usize>)>,
    /// A member by `(owner fqn, name, parameter count)` to its declaring `(file, name-token range)`,
    /// the precise key that disambiguates overloads.
    members: HashMap<(String, String, usize), (FileId, Range<usize>)>,
    /// A member by `(owner fqn, name)` only — the fallback when the parameter count does not match
    /// (a generic / varargs / synthetic-parameter mismatch between the `.class` and its source).
    members_by_name: HashMap<(String, String), (FileId, Range<usize>)>,
}

impl SourceLocations {
    /// The declaring location of the type named `fqn`, if its source is indexed.
    fn type_location(&self, fqn: &str) -> Option<(FileId, Range<usize>)> {
        self.types.get(fqn).cloned()
    }

    /// The declaring location of `owner`'s member `name`, preferring an exact `params`-arity match and
    /// falling back to name-only (so an overload whose arity does not line up still lands in the right
    /// file).
    fn member_location(
        &self,
        owner_fqn: &str,
        name: &str,
        params: usize,
    ) -> Option<(FileId, Range<usize>)> {
        // Build the arity-keyed lookup once; on a miss, reuse its strings for the name-only fallback
        // rather than re-allocating them, so a miss costs two allocations instead of four. (Empty maps
        // — the common no-`sources`-jar case — just miss both `get`s and return `None`.)
        let key = (owner_fqn.to_owned(), name.to_owned(), params);
        if let Some(loc) = self.members.get(&key) {
            return Some(loc.clone());
        }
        let (owner, member, _) = key;
        self.members_by_name.get(&(owner, member)).cloned()
    }
}

/// Fluent builder for a [`ProjectIndex`], created by [`ProjectIndex::builder`]. Each `with_*` turns
/// on one orthogonal input — the embedded `java.lang` stubs, the classpath `.class` facts, a
/// source-location overlay, and `git`/`path` source dependencies — and [`build`](Self::build) folds
/// every configured one in. Omit what you don't need; a project type still wins a fully-qualified-name
/// clash over a library/stub type. Pure and `wasm32`-compatible.
pub struct ProjectIndexBuilder<'a> {
    files: &'a [(FileId, SyntaxNode)],
    source_files: &'a [(FileId, SyntaxNode)],
    stdlib: bool,
    classpath: Option<&'a LoweredClasspath>,
    sources: Option<&'a SourceLocations>,
}

impl<'a> ProjectIndexBuilder<'a> {
    /// Also index the embedded `java.lang` stubs ([`crate::stdlib`]) as
    /// [`Stdlib`](ItemOrigin::Stdlib)-origin types. With them, a reference to a core JDK type
    /// (`String`, `Object`, …) resolves to a real [`Item`] with members and supertypes, so inference
    /// and hover see through it instead of stopping at an external name. Still pure and
    /// `wasm32`-compatible: the stub text is a compile-time constant parsed in memory.
    #[must_use]
    pub fn with_stdlib(mut self) -> Self {
        self.stdlib = true;
        self
    }

    /// Fold in the type, member, and generic signatures decoded from the project's classpath
    /// `.class` files as [`Classpath`](ItemOrigin::Classpath)-origin types. With them, a reference to
    /// an external library type resolves to a real [`Item`] with members and supertypes, so inference
    /// sees through it — `List<String>.get(0)` infers `String` through a loaded `java/util/List`.
    /// Lower the raw `.class` files once with [`ProjectIndex::lower_classpath`] and reuse the result
    /// across rebuilds. Does **not** imply [`with_stdlib`](Self::with_stdlib) — opt into both.
    #[must_use]
    pub fn with_classpath(mut self, classpath: &'a LoweredClasspath) -> Self {
        self.classpath = Some(classpath);
        self
    }

    /// Add a [`SourceLocations`] overlay so each classpath type/member that has matching library
    /// *source* gets a real `.java` go-to-definition target ([`Item::source_location`] /
    /// [`Member::source_location`]). Typing is unchanged — the `.class` files remain authoritative;
    /// the overlay only adds navigation. Build it once with
    /// [`ProjectIndex::index_source_locations`].
    #[must_use]
    pub fn with_source_locations(mut self, sources: &'a SourceLocations) -> Self {
        self.sources = Some(sources);
        self
    }

    /// Index `source_files` — the `.java` of `git`/`path` `[dependencies]` — as
    /// [`Source`](ItemOrigin::Source)-origin types. Unlike a classpath `.class` (typed from bytecode,
    /// navigated via a separate [`SourceLocations`] overlay) or a `-sources.jar` (navigation only),
    /// these files are the typing authority *and* the navigation target: their types resolve for
    /// inference/hover/completion and a reference into one goes to its real declaration. They are not
    /// project files, so the host neither lints nor renames them. Pure and `wasm32`-compatible — the
    /// host reads and parses the library `.java` and assigns their [`FileId`]s.
    #[must_use]
    pub fn with_source_deps(mut self, source_files: &'a [(FileId, SyntaxNode)]) -> Self {
        self.source_files = source_files;
        self
    }

    /// Build the index, folding in every configured input.
    #[must_use]
    pub fn build(self) -> ProjectIndex {
        let empty = SourceLocations::default();
        let sources = self.sources.unwrap_or(&empty);
        let classes = self.classpath.map_or(&[][..], |cp| &cp.classes[..]);
        ProjectIndex::build_inner(self.files, self.source_files, self.stdlib, classes, sources)
    }
}

impl ProjectIndex {
    /// Start building the index from each file's `SOURCE_FILE` root. Returns a
    /// [`ProjectIndexBuilder`]; chain `with_*` options and finish with
    /// [`build`](ProjectIndexBuilder::build). Pure: no I/O, never panics.
    ///
    /// Each file contributes its package, its type-name imports, and every type declaration it
    /// holds (top-level and nested). When two files declare the same fully-qualified name, the
    /// first one indexed wins. With no options the JDK / classpath is *not* indexed — opt in with
    /// [`with_stdlib`](ProjectIndexBuilder::with_stdlib),
    /// [`with_classpath`](ProjectIndexBuilder::with_classpath),
    /// [`with_source_locations`](ProjectIndexBuilder::with_source_locations), and
    /// [`with_source_deps`](ProjectIndexBuilder::with_source_deps), each turning on one orthogonal
    /// input.
    #[must_use]
    pub fn builder(files: &[(FileId, SyntaxNode)]) -> ProjectIndexBuilder<'_> {
        ProjectIndexBuilder {
            files,
            source_files: &[],
            stdlib: false,
            classpath: None,
            sources: None,
        }
    }

    /// Lower the classpath `.class` files to the index facts they contribute, once, so a host that
    /// re-indexes on every edit can reuse the result via
    /// [`ProjectIndexBuilder::with_classpath`] instead of
    /// re-decoding the (unchanging) classpath each time. Lowering decodes every member's descriptor
    /// and generic signature — the expensive half of folding a classpath in; registering the
    /// resulting facts (the cheap half) still runs per rebuild. Pure and `wasm32`-compatible.
    pub fn lower_classpath(classfiles: &[jals_classfile::ClassFile]) -> LoweredClasspath {
        LoweredClasspath {
            classes: classfiles
                .iter()
                .filter_map(crate::classpath::lower)
                .collect(),
        }
    }

    /// Index where the types and members of the host's extracted library *sources* are declared, once,
    /// so [`ProjectIndexBuilder::with_source_locations`] can reuse the
    /// result across rebuilds (the sources of a fixed dependency do not change). Each entry of
    /// `sources` is a library `.java` file's `(FileId, SOURCE_FILE root)`; the host registers those
    /// `FileId`s so it can map a match back to a real URL. Pure and `wasm32`-compatible.
    pub fn index_source_locations(sources: &[(FileId, SyntaxNode)]) -> SourceLocations {
        let mut locs = SourceLocations::default();
        for (file, root) in sources {
            let package = ast::SourceFile::cast(root.clone())
                .and_then(|s| s.package())
                .and_then(|p| p.name())
                .map(|n| n.text())
                .filter(|p| !p.is_empty());
            collect_source_locations(*file, root, package.as_deref(), None, &mut locs);
        }
        locs
    }

    fn build_inner(
        files: &[(FileId, SyntaxNode)],
        source_files: &[(FileId, SyntaxNode)],
        stdlib: bool,
        classes: &[crate::classpath::ClassfileClass],
        sources: &SourceLocations,
    ) -> ProjectIndex {
        let mut index = ProjectIndex {
            items: Vec::new(),
            by_fqn: HashMap::new(),
            files: HashMap::new(),
            members: Vec::new(),
            members_by_owner: HashMap::new(),
            decl_to_item: HashMap::new(),
        };

        // Stub compilation units are parsed here and given reserved high `FileId`s (counting down
        // from `u32::MAX`) so they never collide with the host's low, sequential ids. Each parsed
        // `SyntaxNode` keeps its tree alive for the duration of this build; afterwards every `Item` /
        // `Member` is self-contained data, so the nodes can be dropped.
        let stubs: Vec<(FileId, SyntaxNode)> = if stdlib {
            crate::stdlib::stub_sources()
                .iter()
                .enumerate()
                .map(|(i, src)| {
                    (
                        FileId(u32::MAX - i as u32),
                        jals_syntax::parse(src).syntax(),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };

        // Every compilation unit to index, in priority order: the host's project files first, then
        // the `git`/`path` library sources, then the embedded stubs — so on a fully-qualified-name
        // clash a project type wins over a library type wins over a stub (`by_fqn` keeps the first
        // insert). All three passes below walk this one origin-tagged list.
        let units: Vec<(FileId, &SyntaxNode, ItemOrigin)> = files
            .iter()
            .map(|(file, root)| (*file, root, ItemOrigin::Project))
            .chain(
                source_files
                    .iter()
                    .map(|(file, root)| (*file, root, ItemOrigin::Source)),
            )
            .chain(
                stubs
                    .iter()
                    .map(|(file, root)| (*file, root, ItemOrigin::Stdlib)),
            )
            .collect();

        // First pass: package, imports, and type declarations.
        for &(file, root, origin) in &units {
            index.collect_file(file, root, origin);
        }
        // Classpath `.class` files (already lowered to self-contained data) registered like source
        // types. Their reserved `FileId`s sit just below the stub block so they never collide.
        let classfile_block_start = u32::MAX - stubs.len() as u32 - 1;
        let classfiles: Vec<(FileId, &crate::classpath::ClassfileClass)> = classes
            .iter()
            .enumerate()
            .map(|(j, class)| (FileId(classfile_block_start - j as u32), class))
            .collect();
        let classfile_owners: Vec<ItemId> = classfiles
            .iter()
            .map(|&(file, class)| index.collect_classfile_type(file, class, sources))
            .collect();
        // Index each type's declaration site, so a same-file type reference (which resolves
        // file-locally, not through the project) can be mapped back to its item for find-references.
        for (i, item) in index.items.iter().enumerate() {
            index
                .decl_to_item
                .insert((item.file, item.name_range.start), ItemId(i as u32));
        }
        // Second pass: members and project-internal inheritance. It runs after every type is indexed
        // so a supertype declared later (or in another file / stub) still resolves.
        for &(file, root, _) in &units {
            index.collect_members_and_supertypes(file, root);
        }
        // The same second pass for classpath types, now that every type (project, stub, classpath) is
        // registered, so their supertypes resolve by fully-qualified name.
        for ((file, class), &owner) in classfiles.iter().copied().zip(&classfile_owners) {
            index.collect_classfile_members_and_supertypes(file, owner, class, sources);
        }
        index
    }

    /// Records one file's package, type-name imports, and type declarations (with the given
    /// `origin`). The first pass of [`build_inner`](ProjectIndex::build_inner), shared by project and
    /// stub files.
    fn collect_file(&mut self, file: FileId, root: &SyntaxNode, origin: ItemOrigin) {
        let Some(src) = ast::SourceFile::cast(root.clone()) else {
            return;
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
        self.files.insert(
            file,
            FileMeta {
                package: package.clone(),
                single_imports,
                on_demand,
            },
        );
        self.collect_types(file, root, package.as_deref(), None, origin);
    }

    /// Walks `node`, recording each type declaration with its fully-qualified name and threading the
    /// enclosing type's FQN into its descendants (so a nested `Inner` becomes `Outer.Inner`).
    fn collect_types(
        &mut self,
        file: FileId,
        node: &SyntaxNode,
        package: Option<&str>,
        enclosing: Option<&str>,
        origin: ItemOrigin,
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
                origin,
                file,
                name_range: byte_range(&name_tok),
                type_params: type_params_of(node),
                supertypes: Vec::new(),
                has_external_supertype: false,
                // A project / stub type's own `file` already points at real (or no) source.
                source_location: None,
            });
            self.by_fqn.entry(fqn.clone()).or_insert(id);
            next_enclosing = Some(fqn);
        }
        for child in node.children() {
            self.collect_types(file, &child, package, next_enclosing.as_deref(), origin);
        }
    }

    /// Pushes a fully-built [`Member`], giving it a dense [`MemberId`] and recording it under its
    /// owner. Shared by the source-file and classpath second passes.
    fn register_member(&mut self, owner: ItemId, member: Member) {
        let id = MemberId(self.members.len() as u32);
        self.members.push(member);
        self.members_by_owner.entry(owner).or_default().push(id);
    }

    /// Resolves a list of supertype references (each a [`MemberType::Named`]) against the index,
    /// keeping the ones that land on an indexed project type (with the arguments the clause supplied)
    /// and noting whether any resolves *outside* the project (so the type's full member set is not
    /// knowable). Shared by the source-file and classpath second passes.
    fn resolve_supertypes(&self, file: FileId, supers: &[MemberType]) -> (Vec<Supertype>, bool) {
        let mut supertypes = Vec::new();
        let mut has_external = false;
        for sup in supers {
            let MemberType::Named {
                name,
                qualified,
                args,
                ..
            } = sup
            else {
                continue;
            };
            match self.resolve_type_name(file, name, qualified.as_deref()) {
                TypeResolution::Project(id) => supertypes.push(Supertype {
                    id,
                    args: args.clone(),
                }),
                TypeResolution::External | TypeResolution::Unresolved => has_external = true,
            }
        }
        (supertypes, has_external)
    }

    /// Walks `node`, recording each type declaration's direct members and resolving its
    /// project-internal supertypes. Runs in [`build`](ProjectIndexBuilder::build)'s second pass, when every
    /// type is already indexed (so a forward / cross-file supertype reference resolves).
    fn collect_members_and_supertypes(&mut self, file: FileId, node: &SyntaxNode) {
        if type_decl_kind(node.kind()).is_some()
            && let Some(name_tok) = first_ident_token(node)
            && let Some(owner) = self.item_by_decl(file, byte_range(&name_tok).start)
        {
            // Members. Captured purely from the node; pushed here so each gets a dense `MemberId`.
            for member in members_of_decl(owner, file, node, name_tok.text()) {
                self.register_member(owner, member);
            }
            let (supertypes, has_external) =
                self.resolve_supertypes(file, &raw_supertypes_of(node));
            let item = &mut self.items[owner.0 as usize];
            item.supertypes = supertypes;
            item.has_external_supertype = has_external;
        }
        for child in node.children() {
            self.collect_members_and_supertypes(file, &child);
        }
    }

    /// Registers a classpath type (first pass): pushes its [`Item`] and a per-file [`FileMeta`], and
    /// returns the new [`ItemId`]. Members and supertypes are filled in by
    /// [`collect_classfile_members_and_supertypes`](Self::collect_classfile_members_and_supertypes),
    /// mirroring the two-pass source path so a forward / cross-file supertype still resolves.
    fn collect_classfile_type(
        &mut self,
        file: FileId,
        class: &crate::classpath::ClassfileClass,
        sources: &SourceLocations,
    ) -> ItemId {
        let id = ItemId(self.items.len() as u32);
        self.items.push(Item {
            fqn: Fqn(class.fqn.clone()),
            kind: class.kind,
            origin: ItemOrigin::Classpath,
            file,
            name_range: 0..0,
            type_params: class.type_params.clone(),
            supertypes: Vec::new(),
            has_external_supertype: false,
            // A real-source go-to-definition target, when this type's library source is indexed.
            source_location: sources.type_location(&class.fqn),
        });
        // A project or stub type of the same name wins (first insert).
        self.by_fqn.entry(class.fqn.clone()).or_insert(id);
        // Each classpath type gets its own pseudo-file with empty imports: every captured type name is
        // emitted fully-qualified, so it resolves through `resolve_qualified` without an import context.
        let package = class.fqn.rsplit_once('.').map(|(pkg, _)| pkg.to_string());
        self.files.insert(
            file,
            FileMeta {
                package,
                single_imports: Vec::new(),
                on_demand: Vec::new(),
            },
        );
        id
    }

    /// Registers a classpath type's members and resolves its supertypes by fully-qualified name
    /// (second pass), the classfile counterpart of
    /// [`collect_members_and_supertypes`](Self::collect_members_and_supertypes).
    fn collect_classfile_members_and_supertypes(
        &mut self,
        file: FileId,
        owner: ItemId,
        class: &crate::classpath::ClassfileClass,
        sources: &SourceLocations,
    ) {
        for member in &class.members {
            self.register_member(
                owner,
                Member {
                    owner,
                    name: member.name.clone(),
                    kind: member.kind,
                    file,
                    name_range: 0..0,
                    ty: member.ty.clone(),
                    params: member.params.clone(),
                    varargs: member.varargs,
                    // A real-source go-to-definition target, when the library source is indexed.
                    source_location: sources.member_location(
                        &class.fqn,
                        &member.name,
                        member.params.len(),
                    ),
                },
            );
        }
        let (supertypes, has_external) = self.resolve_supertypes(file, &class.supertypes);
        let item = &mut self.items[owner.0 as usize];
        item.supertypes = supertypes;
        item.has_external_supertype = has_external;
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

        // 4. Implicit `java.lang` import: an unqualified name is brought into every compilation unit
        //    from `java.lang`. When the stubs are indexed (via `with_stdlib`) it binds to one;
        //    when they are not, `java.lang.*` is absent from `by_fqn` and this falls through to the
        //    external handling below — identical to the pre-stub behaviour.
        if let Some(&id) = self.by_fqn.get(&format!("java.lang.{name}")) {
            return TypeResolution::Project(id);
        }

        // 5. Reachable from outside the index: an (unstubbed) implicit `java.lang` type, or any
        //    on-demand import that could supply an unindexed type. Either way, no diagnostic.
        if is_java_lang(name) || !meta.on_demand.is_empty() {
            return TypeResolution::External;
        }

        // 6. Nameable from nowhere.
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
                match item.origin {
                    // A project or library-source type's own `(file, name_range)` is its real source.
                    ItemOrigin::Project | ItemOrigin::Source => {
                        Some((item.file, item.name_range.clone()))
                    }
                    // A classpath type navigates into its library source when that source is indexed;
                    // otherwise it has no host-openable location.
                    ItemOrigin::Classpath => item.source_location.clone(),
                    // A stub has no real source at all.
                    ItemOrigin::Stdlib => None,
                }
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

    /// The member named `name` in name-space `namespace` declared *directly* on `owner` (no
    /// inheritance walk), if any. The single-level building block of the inheritance-aware
    /// [`resolve_member`](Self::resolve_member); used by generic member substitution, which threads
    /// type arguments down the chain one level at a time.
    pub fn declared_member(
        &self,
        owner: ItemId,
        name: &str,
        namespace: Namespace,
    ) -> Option<MemberId> {
        self.members_by_owner
            .get(&owner)?
            .iter()
            .copied()
            .find(|&id| {
                let member = &self.members[id.0 as usize];
                member.name == name && member.kind.namespace() == namespace
            })
    }

    /// Whether `name` is one of type `owner`'s own declared type parameters (`class Box<E>` → `E` is
    /// a type parameter). Used to tell a bare type-variable reference apart from a real type name.
    pub(crate) fn is_type_param(&self, owner: ItemId, name: &str) -> bool {
        self.item(owner).type_params.iter().any(|p| p.name == name)
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
        // The type's own members win over inherited ones — the walk reaches `current`'s
        // supertypes only after `declared_member` returns `None`.
        self.walk_supertypes(owner, |current| {
            self.declared_member(current, name, namespace)
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

    /// Every member reachable from `owner` — its own and those of its project-internal supertypes —
    /// in nearest-first order (the type itself, then each supertype, cycle-guarded). Unlike
    /// [`resolve_member`](Self::resolve_member) / [`resolve_members_all`](Self::resolve_members_all),
    /// which look one name up, this enumerates *every* member, for member completion. An inherited
    /// member overridden or shadowed nearer appears more than once (nearest first); the caller applies
    /// its own de-duplication policy. A member of an external supertype is not reachable and absent.
    pub fn members_of(&self, owner: ItemId) -> Vec<MemberId> {
        let mut out = Vec::new();
        self.walk_supertypes(owner, |current| {
            if let Some(ids) = self.members_by_owner.get(&current) {
                out.extend_from_slice(ids);
            }
            None::<()>
        });
        out
    }

    /// Whether the full set of overloads named `name` on `owner` is knowable from the index — a
    /// precondition for concluding "no overload matches" without a false positive.
    ///
    /// It is *not* knowable when `name` is an [`Object`](is_object_method) method (every type inherits
    /// `Object`'s overloads, which are not indexed), when `owner` or any project supertype `extends`
    /// / `implements` a type outside the project (which may declare further overloads we cannot see),
    /// or when the walk reaches a standard-library *stub* type, whose member set is deliberately
    /// partial (the common members only) — so a stub-owned or stub-inherited overload set is treated
    /// as incomplete, never yielding a "no overload" conclusion.
    pub fn method_set_complete(&self, owner: ItemId, name: &str) -> bool {
        if is_object_method(name) {
            return false;
        }
        self.walk_supertypes(owner, |current| {
            let item = &self.items[current.0 as usize];
            (item.origin == ItemOrigin::Stdlib || item.has_external_supertype).then_some(())
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
        self.walk_supertypes_stateful(start, (), |current, ()| visit(current), |_, (), _| ())
    }

    /// [`walk_supertypes`](Self::walk_supertypes) threading a per-type state `S` down the chain: each
    /// frame carries the state `descend` derives from its parent's (and the linking [`Supertype`]),
    /// so a stateful traversal — e.g. propagating generic type arguments to inherited members —
    /// rides on the single cycle-guarded walk rather than re-implementing it.
    pub(crate) fn walk_supertypes_stateful<S, R>(
        &self,
        start: ItemId,
        init: S,
        mut visit: impl FnMut(ItemId, &S) -> Option<R>,
        mut descend: impl FnMut(ItemId, &S, &Supertype) -> S,
    ) -> Option<R> {
        let mut visited = HashSet::new();
        let mut stack = vec![(start, init)];
        while let Some((current, state)) = stack.pop() {
            if !visited.insert(current) {
                continue;
            }
            if let Some(result) = visit(current, &state) {
                return Some(result);
            }
            for sup in self.items[current.0 as usize].supertypes.iter().rev() {
                let next = descend(current, &state, sup);
                stack.push((sup.id, next));
            }
        }
        None
    }
}

/// Walk a library source tree, mirroring [`ProjectIndex::collect_types`]'s recursion, and record each
/// type's and member's declaring `(file, name range)` into `locs` (first declaration wins, like
/// `by_fqn`). Member ranges and parameter counts come straight from [`members_of_decl`] so they line
/// up with how the index reads members; the `ItemId(0)` owner is a placeholder, only the member name,
/// arity, and range are read.
fn collect_source_locations(
    file: FileId,
    node: &SyntaxNode,
    package: Option<&str>,
    enclosing: Option<&str>,
    locs: &mut SourceLocations,
) {
    let mut next_enclosing = enclosing.map(str::to_string);
    if type_decl_kind(node.kind()).is_some()
        && let Some(name_tok) = first_ident_token(node)
    {
        let fqn = build_fqn(package, enclosing, name_tok.text());
        locs.types
            .entry(fqn.clone())
            .or_insert_with(|| (file, byte_range(&name_tok)));
        for member in members_of_decl(ItemId(0), file, node, name_tok.text()) {
            let loc = (member.file, member.name_range.clone());
            locs.members
                .entry((fqn.clone(), member.name.clone(), member.params.len()))
                .or_insert_with(|| loc.clone());
            locs.members_by_name
                .entry((fqn.clone(), member.name.clone()))
                .or_insert(loc);
        }
        next_enclosing = Some(fqn);
    }
    for child in node.children() {
        collect_source_locations(file, &child, package, next_enclosing.as_deref(), locs);
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
                // A project / stub member's own `file` already points at real (or no) source.
                source_location: None,
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
                        args: Vec::new(),
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

/// The supertypes of a type declaration `node`: the `extends` and `implements` clause types, each
/// captured as a [`MemberType`] (so its name, qualifier, and type arguments are all kept). Always a
/// [`MemberType::Named`] for a well-formed clause. Pure.
fn raw_supertypes_of(node: &SyntaxNode) -> Vec<MemberType> {
    let mut supertypes = Vec::new();
    for clause in node
        .children()
        .filter(|c| matches!(c.kind(), EXTENDS_CLAUSE | IMPLEMENTS_CLAUSE))
    {
        for ty in clause.children().filter_map(ast::Type::cast) {
            supertypes.push(member_type_of(Some(ty)));
        }
    }
    supertypes
}

/// The declared type parameters of a type declaration `node`, in order (`class Box<K, V>` → `K`,
/// `V`), each with its `extends` bounds captured. Empty for a non-generic type. Pure.
fn type_params_of(node: &SyntaxNode) -> Vec<TypeParamDecl> {
    node.children()
        .find_map(ast::TypeParams::cast)
        .map(|tps| {
            tps.params()
                .map(|tp| TypeParamDecl {
                    name: tp.name().unwrap_or_default(),
                    bounds: tp.bounds().map(|b| member_type_of(Some(b))).collect(),
                })
                .collect()
        })
        .unwrap_or_default()
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
            // `var`, a bare wildcard (`?`), or a missing keyword: no nameable type.
            Some("var" | "?") | None => MemberType::Unknown,
            Some(k) => MemberType::Primitive {
                keyword: k.to_string(),
                dims,
            },
        }
    } else {
        let args = ty
            .type_arg_types()
            .map(|a| member_type_of(Some(a)))
            .collect();
        MemberType::Named {
            name: ty.simple_name().unwrap_or_default(),
            qualified: ty.qualified_text().filter(|q| q.contains('.')),
            dims,
            args,
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
