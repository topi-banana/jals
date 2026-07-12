//! Tests for assignment conversion (`Ty::is_assignable_to`) over the project class hierarchy: the
//! reference-subtyping cases that need a [`ProjectIndex`] to walk `extends` / `implements`.

use jals_hir::{ClassTy, FileId, Primitive, ProjectIndex, Ty};
use jals_syntax::SyntaxNode;

/// Parses each source (keeping the `SOURCE_FILE` nodes alive) and builds a [`ProjectIndex`].
fn build(sources: &[&str]) -> (Vec<(FileId, SyntaxNode)>, ProjectIndex) {
    let nodes: Vec<(FileId, SyntaxNode)> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                FileId(u32::try_from(i).unwrap()),
                jals_syntax::Parse::parse(s).syntax(),
            )
        })
        .collect();
    let index = ProjectIndex::builder(&nodes).build();
    (nodes, index)
}

/// The project [`Ty`] of the type declared as `decl_name` in `sources[file]`, found via the
/// declaration-name offset.
fn project_ty(index: &ProjectIndex, sources: &[&str], file: u32, decl_name: &str) -> Ty {
    let start = sources[file as usize]
        .find(decl_name)
        .expect("declaration name present in source");
    let id = index
        .item_by_decl(FileId(file), start)
        .expect("a project item declared there");
    Ty::Class(ClassTy::Project {
        id,
        name: decl_name.to_owned(),
        args: Vec::new(),
    })
}

/// An external (JDK / unindexed) reference type, by name.
fn external(name: &str) -> Ty {
    Ty::Class(ClassTy::external(name))
}

#[test]
fn subclass_is_assignable_to_its_project_superclass() {
    let sources = ["class Base {}", "class Sub extends Base { }"];
    let (_nodes, index) = build(&sources);
    let base = project_ty(&index, &sources, 0, "Base");
    let sub = project_ty(&index, &sources, 1, "Sub");

    assert!(sub.is_assignable_to(&base, Some(&index)), "Sub <: Base");
    assert!(sub.is_assignable_to(&sub, Some(&index)), "reflexive");
    // Not symmetric: a superclass value is not assignable to a subclass slot.
    assert!(!base.is_assignable_to(&sub, Some(&index)));
}

#[test]
fn unrelated_project_types_are_not_assignable() {
    let sources = ["class A {}", "class B {}"];
    let (_nodes, index) = build(&sources);
    let a = project_ty(&index, &sources, 0, "A");
    let b = project_ty(&index, &sources, 1, "B");

    assert!(!a.is_assignable_to(&b, Some(&index)));
    assert!(!b.is_assignable_to(&a, Some(&index)));
}

#[test]
fn assignability_follows_interfaces_and_is_transitive() {
    let sources = [
        "interface C {}",
        "interface B extends C {}",
        "class A implements B {}",
    ];
    let (_nodes, index) = build(&sources);
    let a = project_ty(&index, &sources, 2, "A");
    let b = project_ty(&index, &sources, 1, "B");
    let c = project_ty(&index, &sources, 0, "C");

    assert!(a.is_assignable_to(&b, Some(&index)), "A implements B");
    assert!(b.is_assignable_to(&c, Some(&index)), "B extends C");
    assert!(
        a.is_assignable_to(&c, Some(&index)),
        "A <: C transitively through B"
    );
}

#[test]
fn an_external_supertype_is_lenient_but_an_unrelated_project_type_is_not() {
    // `Object` is java.lang (external, not indexed), so it is filtered out of `Sub`'s project
    // supertypes — leaving `Sub` with no indexed supertypes at all.
    let sources = ["class Sub extends Object {}", "class Other {}"];
    let (_nodes, index) = build(&sources);
    let sub = project_ty(&index, &sources, 0, "Sub");
    let other = project_ty(&index, &sources, 1, "Other");

    // A project type widens to any external type conservatively.
    assert!(sub.is_assignable_to(&external("Object"), Some(&index)));
    // But to an unrelated indexed project type it is a confident mismatch.
    assert!(!sub.is_assignable_to(&other, Some(&index)));
}

#[test]
fn primitives_and_project_types_do_not_mix_while_externals_stay_lenient() {
    let sources = ["class Foo {}"];
    let (_nodes, index) = build(&sources);
    let foo = project_ty(&index, &sources, 0, "Foo");
    let int = Ty::Primitive(Primitive::Int);

    // A primitive never boxes to a user type, in either direction.
    assert!(!int.is_assignable_to(&foo, Some(&index)));
    assert!(!foo.is_assignable_to(&int, Some(&index)));
    // A project type and an external reference stay mutually lenient (no false mismatch).
    assert!(foo.is_assignable_to(&external("Object"), Some(&index)));
    assert!(external("Object").is_assignable_to(&foo, Some(&index)));
}

#[test]
fn a_cyclic_hierarchy_terminates() {
    // Mutually-referential and self-referential supertypes (illegal but parseable) must not loop.
    let sources = [
        "class A extends B {}",
        "class B extends A {}",
        "class C extends C { }",
    ];
    let (_nodes, index) = build(&sources);
    let a = project_ty(&index, &sources, 0, "A");
    let b = project_ty(&index, &sources, 1, "B");
    let c = project_ty(&index, &sources, 2, "C");

    // The values are whatever the (cycle-guarded) walk yields; the point is that each returns.
    let _ = a.is_assignable_to(&b, Some(&index));
    let _ = b.is_assignable_to(&a, Some(&index));
    let _ = c.is_assignable_to(&a, Some(&index));
    let _ = a.is_assignable_to(&Ty::Primitive(Primitive::Int), Some(&index));
}
