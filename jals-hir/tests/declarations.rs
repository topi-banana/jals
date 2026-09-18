//! A package's API **as data**, indexed through `ProjectIndexBuilder::with_declarations`.
//!
//! The structured counterpart of `native_packages.rs`: the same origin and the same fidelity
//! ranking, reached with no Java text at all. What the assertions are about is that a project file
//! resolves into a declaration unit exactly as it does into parsed Java — the names are fully
//! qualified, the implicit modifiers of an interface are folded in, and the model is explicit
//! about what it does not list.

use std::borrow::Cow;

use jals_hir::{
    DeclaredUnit, FileAnalysis, FileId, ItemOrigin, LibraryFidelity, ProjectIndex, TypeResolution,
};
use jals_native::{DeclaredType, Member, MemberKind, Modifiers, Param, TypeKind, TypeRef};
use jals_syntax::SyntaxNode;

/// A borrowed name, which is what a `'static` declaration table holds.
const fn name(text: &'static str) -> Cow<'static, str> {
    Cow::Borrowed(text)
}

/// The primitive `int`.
const fn int() -> TypeRef {
    TypeRef::Primitive {
        keyword: name("int"),
        dims: 0,
    }
}

/// A reference to the declared `demo.Container`.
const fn container() -> TypeRef {
    TypeRef::Named {
        fqn: name("demo.Container"),
        dims: 0,
        args: Vec::new(),
    }
}

/// One member with everything a fixture does not exercise left at its default.
fn member(name: &'static str, kind: MemberKind, ty: TypeRef) -> Member {
    Member {
        name: name.into(),
        kind,
        ty,
        params: Vec::new(),
        varargs: false,
        type_params: Vec::new(),
        throws: Vec::new(),
        annotations: Vec::new(),
        modifiers: Modifiers::default(),
    }
}

/// Two declared types: an interface and a class implementing it.
fn declarations() -> Vec<DeclaredType> {
    vec![
        DeclaredType {
            fqn: name("demo.Container"),
            package: name("demo"),
            kind: TypeKind::Interface,
            type_params: Vec::new(),
            supertypes: Vec::new(),
            members: vec![
                member("size", MemberKind::Method, int()),
                Member {
                    params: vec![Param {
                        name: Some(name("at")),
                        ty: int(),
                        annotations: Vec::new(),
                    }],
                    ..member("at", MemberKind::Method, int())
                },
            ],
            annotations: Vec::new(),
        },
        DeclaredType {
            fqn: name("demo.Box"),
            package: name("demo"),
            kind: TypeKind::Class,
            type_params: Vec::new(),
            supertypes: vec![container()],
            members: vec![
                member("Box", MemberKind::Constructor, TypeRef::Unknown),
                Member {
                    modifiers: Modifiers {
                        is_private: true,
                        ..Modifiers::default()
                    },
                    ..member("value", MemberKind::Field, container())
                },
                member("size", MemberKind::Method, int()),
            ],
            annotations: Vec::new(),
        },
    ]
}

/// A project file parsed under [`FileId(0)`], as every host hands one over.
fn parse(text: &str) -> (FileId, SyntaxNode) {
    (
        FileId(0),
        jals_exec::block_on_inline(jals_syntax::Parse::parse(text)).syntax(),
    )
}

/// One project file indexed against the declaration unit.
fn index_of(project: &str) -> (ProjectIndex, SyntaxNode) {
    let project = [parse(project)];
    let declared = declarations();
    let units = [DeclaredUnit {
        file: FileId::library(0),
        declarations: &declared,
        fidelity: LibraryFidelity::Complete,
    }];
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&project)
            .with_declarations(&units)
            .build(),
    );
    (index, project[0].1.clone())
}

/// The declared types are indexed under the library origin, with the members the model lists.
#[test]
fn a_declaration_unit_is_indexed_under_the_library_origin() {
    let (index, _) = index_of("class C {}");
    let item = index
        .item_by_fqn("demo.Box")
        .expect("the declared class is indexed");
    assert_eq!(
        index.item(item).origin,
        ItemOrigin::Library(LibraryFidelity::Complete)
    );
    let members: Vec<&str> = index
        .own_members(item)
        .iter()
        .map(|id| index.member(*id).name.as_str())
        .collect();
    assert_eq!(members, vec!["Box", "value", "size"]);
}

/// An interface's members carry the modifiers the language implies, exactly as parsed Java's do:
/// a method is `public` and `abstract` when the model does not say otherwise.
#[test]
fn an_interfaces_implied_modifiers_are_folded_in() {
    let (index, _) = index_of("class C {}");
    let item = index
        .item_by_fqn("demo.Container")
        .expect("the declared interface is indexed");
    let size = index
        .own_members(item)
        .iter()
        .copied()
        .find(|&id| index.member(id).name == "size")
        .expect("the interface declares `size`");
    let modifiers = index.member(size).modifiers;
    assert!(modifiers.is_public, "JLS §9.4");
}

/// A supertype written as a fully-qualified reference resolves to the declared item, so the class
/// and its interface are one hierarchy — which is what inherited lookup and dispatch read.
#[test]
fn a_qualified_supertype_resolves_within_the_unit() {
    let (index, _) = index_of("class C {}");
    let container = index
        .item_by_fqn("demo.Container")
        .expect("the declared interface is indexed");
    let boxed = index.item_by_fqn("demo.Box").expect("indexed");
    assert!(
        index.direct_interfaces(boxed).any(|id| id == container),
        "the declared `implements` edge resolved"
    );
}

/// A field's declared type resolves too, so `Box.value` is a `Container` and not an external name.
#[test]
fn a_members_declared_type_resolves() {
    let (index, _) = index_of("class C {}");
    let boxed = index.item_by_fqn("demo.Box").expect("indexed");
    let container = index.item_by_fqn("demo.Container").expect("indexed");
    let value = index
        .own_members(boxed)
        .iter()
        .copied()
        .find(|&id| index.member(id).name == "value")
        .expect("the class declares `value`");
    assert_eq!(
        index.resolve_type_name(FileId::library(0), "demo.Container", Some("demo.Container")),
        TypeResolution::Project(container)
    );
    // The same question asked through the member's captured type.
    assert_eq!(
        index.resolved_member_ty(value).project_id(),
        Some(container)
    );
}

/// A project file resolves a method on a declared type — the whole reason the analysis is given
/// the unit. Without it every name into the package is unresolved.
#[test]
fn a_project_file_resolves_a_name_into_the_declaration_unit() {
    let source = "import demo.Container;\n\
                  class C { int f(Container c) { return c.size(); } }\n";
    let (index, root) = index_of(source);
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    let unresolved =
        jals_exec::block_on_inline(analysis.in_project(&index, FileId(0)).unresolved_names());
    assert!(unresolved.is_empty(), "{unresolved:?}");

    let item = index
        .item_by_fqn("demo.Container")
        .expect("the declared interface is indexed");
    assert_eq!(
        index.resolve_type_name(FileId(0), "Container", None),
        TypeResolution::Project(item),
        "the single-type import reaches the declared type"
    );
}
