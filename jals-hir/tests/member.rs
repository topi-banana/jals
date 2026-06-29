//! Tests for the project member model: member indexing, declared-type capture, and member
//! resolution through project-internal inheritance.

use jals_hir::{DefKind, FileId, MemberType, Namespace, ProjectIndex, Supertype, TypeParamDecl};
use jals_syntax::SyntaxNode;

/// Parses each source (keeping the `SOURCE_FILE` nodes alive) and builds a [`ProjectIndex`].
fn build(sources: &[&str]) -> (Vec<(FileId, SyntaxNode)>, ProjectIndex) {
    let nodes: Vec<(FileId, SyntaxNode)> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| (FileId(i as u32), jals_syntax::parse(s).syntax()))
        .collect();
    let index = ProjectIndex::builder(&nodes).build();
    (nodes, index)
}

/// The [`ItemId`](jals_hir::ItemId) of the type declared as `decl_name` in `sources[file]`, found
/// via the declaration-name offset.
fn item(index: &ProjectIndex, sources: &[&str], file: u32, decl_name: &str) -> jals_hir::ItemId {
    let start = sources[file as usize]
        .find(decl_name)
        .expect("declaration name present in source");
    index
        .item_by_decl(FileId(file), start)
        .expect("a project item declared there")
}

#[test]
fn fields_and_methods_are_indexed_with_their_declared_type() {
    let sources = ["class T { int a; String s; long[] arr; void m() {} Foo f; }"];
    let (_nodes, index) = build(&sources);
    let t = item(&index, &sources, 0, "T");

    let field = |name: &str| index.member(index.resolve_member(t, name, Namespace::Value).unwrap());
    assert_eq!(
        field("a").ty,
        MemberType::Primitive {
            keyword: "int".into(),
            dims: 0
        }
    );
    assert_eq!(
        field("arr").ty,
        MemberType::Primitive {
            keyword: "long".into(),
            dims: 1
        }
    );
    assert_eq!(
        field("s").ty,
        MemberType::Named {
            name: "String".into(),
            qualified: None,
            dims: 0,
            args: Vec::new(),
        }
    );
    assert_eq!(field("f").kind, DefKind::Field);

    let m = index.member(index.resolve_member(t, "m", Namespace::Method).unwrap());
    assert_eq!(m.kind, DefKind::Method);
    assert_eq!(m.ty, MemberType::Void);
}

/// A named member type captures its type arguments (`List<String>`, `Map<String, Integer>`),
/// recursively, as data — the basis for later generic substitution.
#[test]
fn field_type_captures_type_arguments() {
    let sources = ["class T { List<String> xs; Map<String, Integer> m; }"];
    let (_nodes, index) = build(&sources);
    let t = item(&index, &sources, 0, "T");
    let field = |name: &str| {
        index
            .member(index.resolve_member(t, name, Namespace::Value).unwrap())
            .ty
            .clone()
    };

    let named = |n: &str| MemberType::Named {
        name: n.into(),
        qualified: None,
        dims: 0,
        args: Vec::new(),
    };
    assert_eq!(
        field("xs"),
        MemberType::Named {
            name: "List".into(),
            qualified: None,
            dims: 0,
            args: vec![named("String")],
        }
    );
    assert_eq!(
        field("m"),
        MemberType::Named {
            name: "Map".into(),
            qualified: None,
            dims: 0,
            args: vec![named("String"), named("Integer")],
        }
    );
}

/// A type's declared type parameters are recorded in order, each with its `extends` bounds.
#[test]
fn type_parameters_are_recorded_with_their_bounds() {
    let sources = ["class Box<T> { } class Holder<K extends Number, V> { }"];
    let (_nodes, index) = build(&sources);

    let box_ty = index.item(item(&index, &sources, 0, "Box"));
    assert_eq!(
        box_ty.type_params,
        vec![TypeParamDecl {
            name: "T".into(),
            bounds: Vec::new(),
        }]
    );

    let holder = index.item(item(&index, &sources, 0, "Holder"));
    assert_eq!(holder.type_params.len(), 2);
    assert_eq!(holder.type_params[0].name, "K");
    assert_eq!(
        holder.type_params[0].bounds,
        vec![MemberType::Named {
            name: "Number".into(),
            qualified: None,
            dims: 0,
            args: Vec::new(),
        }]
    );
    assert_eq!(holder.type_params[1].name, "V");
    assert!(holder.type_params[1].bounds.is_empty());
}

/// A project-internal supertype records the type arguments the clause supplies (`extends
/// Base<String>` → `[String]`), keyed to the resolved supertype item.
#[test]
fn supertype_arguments_are_recorded() {
    let sources = ["class Base<T> { } class Sub extends Base<String> { }"];
    let (_nodes, index) = build(&sources);
    let base_id = item(&index, &sources, 0, "Base");
    let sub = index.item(item(&index, &sources, 0, "Sub"));
    assert_eq!(
        sub.supertypes,
        vec![Supertype {
            id: base_id,
            args: vec![MemberType::Named {
                name: "String".into(),
                qualified: None,
                dims: 0,
                args: Vec::new(),
            }],
        }]
    );
}

#[test]
fn value_and_method_name_spaces_are_separate() {
    // `run` is both a field and a method; each resolves in its own name-space.
    let sources = ["class C { int run; int run() { return 0; } }"];
    let (_nodes, index) = build(&sources);
    let c = item(&index, &sources, 0, "C");

    let field = index.member(index.resolve_member(c, "run", Namespace::Value).unwrap());
    let method = index.member(index.resolve_member(c, "run", Namespace::Method).unwrap());
    assert_eq!(field.kind, DefKind::Field);
    assert_eq!(method.kind, DefKind::Method);
}

#[test]
fn members_are_inherited_through_a_project_superclass() {
    let sources = [
        "class Base { int shared; void greet() {} }",
        "class Sub extends Base { int own; }",
    ];
    let (_nodes, index) = build(&sources);
    let sub = item(&index, &sources, 1, "Sub");

    // Own and inherited members are both reachable from `Sub`.
    assert!(index.resolve_member(sub, "own", Namespace::Value).is_some());
    let shared = index.member(
        index
            .resolve_member(sub, "shared", Namespace::Value)
            .unwrap(),
    );
    assert_eq!(shared.kind, DefKind::Field);
    let greet = index.member(
        index
            .resolve_member(sub, "greet", Namespace::Method)
            .unwrap(),
    );
    assert_eq!(greet.kind, DefKind::Method);
}

#[test]
fn own_member_shadows_an_inherited_one() {
    let sources = ["class Base { int x; }", "class Sub extends Base { int x; }"];
    let (_nodes, index) = build(&sources);
    let base = item(&index, &sources, 0, "Base");
    let sub = item(&index, &sources, 1, "Sub");

    let resolved = index.member(index.resolve_member(sub, "x", Namespace::Value).unwrap());
    assert_eq!(
        resolved.owner, sub,
        "the subclass's own `x` wins over the inherited one"
    );
    assert_ne!(resolved.owner, base);
}

#[test]
fn an_external_supertype_stops_the_search_gracefully() {
    // `Object` is java.lang (external, not indexed): own members resolve, but an inherited member
    // from the external supertype is simply not found — no panic, no guess.
    let sources = ["class Sub extends Object { int own; }"];
    let (_nodes, index) = build(&sources);
    let sub = item(&index, &sources, 0, "Sub");

    assert!(index.resolve_member(sub, "own", Namespace::Value).is_some());
    assert!(
        index
            .resolve_member(sub, "toString", Namespace::Method)
            .is_none()
    );
}

#[test]
fn enum_constants_are_value_members() {
    let sources = ["enum Color { RED, GREEN; void paint() {} }"];
    let (_nodes, index) = build(&sources);
    let color = item(&index, &sources, 0, "Color");

    let red = index.member(
        index
            .resolve_member(color, "RED", Namespace::Value)
            .unwrap(),
    );
    assert_eq!(red.kind, DefKind::EnumConstant);
    assert!(
        index
            .resolve_member(color, "paint", Namespace::Method)
            .is_some()
    );
}

#[test]
fn an_unresolved_member_is_none() {
    let sources = ["class C { int a; }"];
    let (_nodes, index) = build(&sources);
    let c = item(&index, &sources, 0, "C");
    assert!(index.resolve_member(c, "nope", Namespace::Value).is_none());
    // `a` is a value, not a method.
    assert!(index.resolve_member(c, "a", Namespace::Method).is_none());
}

#[test]
fn build_never_panics_on_broken_or_cyclic_input() {
    // Mutually-referential supertypes (an illegal but possible parse) must not loop forever.
    let _ = build(&[
        "class A extends B { }",
        "class B extends A { }",
        "class",
        "class C extends C { int x; }",
    ]);
}
