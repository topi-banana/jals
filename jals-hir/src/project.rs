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

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;

use jals_syntax::SyntaxKind::{
    ANNOTATION_TYPE_DECL, CLASS_DECL, ENUM_DECL, INTERFACE_DECL, RECORD_DECL,
};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{SyntaxKind, SyntaxNode};

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
            });
            self.by_fqn.entry(fqn.clone()).or_insert(id);
            next_enclosing = Some(fqn);
        }
        for child in node.children() {
            self.collect_types(file, &child, package, next_enclosing.as_deref());
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
}

/// The [`DefKind`] for a type-declaration node kind, or `None` if it is not a type declaration.
fn type_decl_kind(kind: SyntaxKind) -> Option<DefKind> {
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
