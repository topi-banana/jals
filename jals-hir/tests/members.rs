//! The facts a code generator reads off the index: which overload a call binds to, how a member is
//! reached, and what a constructor's parameters are.
//!
//! These are not checking questions. Nothing here decides whether a program is legal — it decides
//! which instruction and which descriptor a backend has to emit for a program already assumed to be.

use jals_hir::{FileAnalysis, FileId, FileSemantics, ProjectIndex};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::AstNode;

/// A single-file project with the embedded stdlib stubs folded in so `System.out.println`
/// resolves. Owns what a binding borrows.
struct Fixture {
    node: SyntaxNode,
    analysis: FileAnalysis,
    index: ProjectIndex,
}

impl Fixture {
    fn new(src: &str) -> Self {
        let node = jals_exec::block_on_inline(jals_syntax::Parse::parse(src)).syntax();
        let analysis = jals_exec::block_on_inline(FileAnalysis::of(&node));
        let index = jals_exec::block_on_inline(
            ProjectIndex::builder(&[(FileId(0), node.clone())])
                .with_stdlib()
                .build(),
        );
        Self {
            node,
            analysis,
            index,
        }
    }

    const fn semantics(&self) -> FileSemantics<'_> {
        self.analysis.in_project(&self.index, FileId(0))
    }
}

/// The member the (first) call whose source text is exactly `text` binds to, rendered as
/// `Owner.name(paramTypes)`.
fn call_target(src: &str, text: &str) -> String {
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let inference = jals_exec::block_on_inline(semantics.typed());
    let (node, index) = (&fixture.node, &fixture.index);
    let call = node
        .descendants()
        .filter_map(jals_syntax::ast::CallExpr::cast)
        // A CST node carries its leading trivia, so the raw text starts with the newline and
        // indentation before the call.
        .find(|call| call.syntax().text().to_string().trim() == text)
        .unwrap_or_else(|| panic!("no call spelled `{text}`"));
    let range = call.syntax().text_range();
    let id = inference
        .call_target_of(usize::from(range.start())..usize::from(range.end()))
        .unwrap_or_else(|| panic!("`{text}` bound to no member"));

    let member = index.member(id);
    let params: Vec<String> = member
        .params
        .iter()
        .map(|param| format!("{:?}", param.ty))
        .collect();
    format!(
        "{}.{}({})",
        index.item(member.owner).fqn,
        member.name,
        params.join(", ")
    )
}

/// The overload a call picks has to be the *most specific* applicable one, not merely the first
/// applicable one. `println` is the case that makes the difference visible: every argument is
/// assignable to some overload, so "first applicable" would depend on declaration order.
#[test]
fn a_call_binds_to_the_overload_its_arguments_select() {
    let src = r#"
        class Main {
            void run() {
                System.out.println("hello");
                System.out.println(1);
                System.out.println(1.5);
            }
        }
    "#;
    assert!(
        call_target(src, r#"System.out.println("hello")"#)
            .ends_with(r#"println(Named { name: "String", qualified: None, dims: 0, args: [] })"#),
        "a String argument must select println(String), got {}",
        call_target(src, r#"System.out.println("hello")"#)
    );
    assert!(
        call_target(src, "System.out.println(1)")
            .ends_with(r#"println(Primitive { keyword: "int", dims: 0 })"#),
        "an int argument must select println(int), got {}",
        call_target(src, "System.out.println(1)")
    );
    assert!(
        call_target(src, "System.out.println(1.5)")
            .ends_with(r#"println(Primitive { keyword: "double", dims: 0 })"#),
        "a double argument must select println(double), got {}",
        call_target(src, "System.out.println(1.5)")
    );
}

/// `System.out` is a `static` field and `println` an instance method — the distinction between
/// `getstatic` and `getfield`, and between `invokestatic` and `invokevirtual`.
#[test]
fn a_member_records_how_it_is_reached() {
    let src = r"
        class Main {
            static int counter;
            private final int value = 0;
            int plain() { return 0; }
        }
    ";
    let fixture = Fixture::new(src);
    let index = &fixture.index;
    let main = index
        .resolve_type_name(FileId(0), "Main", None)
        .project_id()
        .expect("Main");
    let member = |name: &str| {
        let id = index
            .resolve_member(main, name, jals_hir::Namespace::Value)
            .or_else(|| index.resolve_member(main, name, jals_hir::Namespace::Method))
            .unwrap_or_else(|| panic!("no member `{name}`"));
        index.member(id).modifiers
    };

    assert!(member("counter").is_static);
    assert!(!member("counter").is_private);

    assert!(member("value").is_private);
    assert!(!member("value").is_static);

    assert!(!member("plain").is_static);
    assert!(!member("plain").is_private);
}

/// An interface's members carry modifiers its source is allowed to leave unwritten: a field is
/// `static` however it is spelled, which is the difference between `getstatic` and `getfield`.
#[test]
fn implicit_modifiers_are_folded_in() {
    let src = r"
        interface Shape {
            int SIDES = 3;
            double area();
            static Shape unit() { return null; }
        }
    ";
    let fixture = Fixture::new(src);
    let index = &fixture.index;
    let shape = index
        .resolve_type_name(FileId(0), "Shape", None)
        .project_id()
        .expect("Shape");
    let member = |name: &str, namespace| {
        let id = index
            .resolve_member(shape, name, namespace)
            .unwrap_or_else(|| panic!("no member `{name}`"));
        index.member(id).modifiers
    };

    // An interface field is implicitly `public static final` (JLS §9.3).
    assert!(member("SIDES", jals_hir::Namespace::Value).is_static);

    // An instance method is reached through its receiver, however the interface spells it.
    assert!(!member("area", jals_hir::Namespace::Method).is_static);

    // A `static` interface method is reached through the interface itself.
    assert!(member("unit", jals_hir::Namespace::Method).is_static);
}

/// A constructor's parameters were previously never captured — its declaration is a
/// `CONSTRUCTOR_DECL`, and the collector cast to `MethodDecl` — leaving every constructor with no
/// descriptor information at all.
#[test]
fn a_constructor_records_its_parameters() {
    let src = r"
        class Point {
            Point(int x, int y) {}
        }
    ";
    let fixture = Fixture::new(src);
    let index = &fixture.index;
    let point = index
        .resolve_type_name(FileId(0), "Point", None)
        .project_id()
        .expect("Point");
    let id = index
        .resolve_member(point, "Point", jals_hir::Namespace::Method)
        .expect("the constructor");
    let constructor = index.member(id);

    assert_eq!(constructor.params.len(), 2);
    assert_eq!(
        constructor
            .params
            .iter()
            .map(|param| param.name.clone().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["x", "y"]
    );
}

/// The member a `new` binds to, rendered like [`call_target`].
///
/// Keyed by the `NEW_EXPR`'s own span, because that is what a code generator emitting the
/// allocation is looking at.
fn new_target(src: &str, text: &str) -> String {
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let inference = jals_exec::block_on_inline(semantics.typed());
    let (node, index) = (&fixture.node, &fixture.index);
    let new = node
        .descendants()
        .filter_map(jals_syntax::ast::NewExpr::cast)
        .find(|new| new.syntax().text().to_string().trim() == text)
        .unwrap_or_else(|| panic!("no `new` spelled `{text}`"));
    let range = new.syntax().text_range();
    let id = inference
        .call_target_of(usize::from(range.start())..usize::from(range.end()))
        .unwrap_or_else(|| panic!("`{text}` bound to no constructor"));

    let member = index.member(id);
    let params: Vec<String> = member
        .params
        .iter()
        .map(|param| format!("{:?}", param.ty))
        .collect();
    format!(
        "{}.{}({})",
        index.item(member.owner).fqn,
        member.name,
        params.join(", ")
    )
}

/// A `new` selects its constructor the same way a call selects its method: by the arguments, not by
/// how many there are. Picking the first same-arity candidate ran `Pair(int)` for `new Pair(1.5)`.
#[test]
fn a_new_binds_to_the_constructor_its_arguments_select() {
    let src = r"
        class Pair {
            Pair(int value) {}
            Pair(double value) {}

            void run() {
                Pair a = new Pair(1);
                Pair b = new Pair(1.5);
            }
        }
    ";
    assert!(
        new_target(src, "new Pair(1)").ends_with(r#"Pair(Primitive { keyword: "int", dims: 0 })"#),
        "got {}",
        new_target(src, "new Pair(1)")
    );
    assert!(
        new_target(src, "new Pair(1.5)")
            .ends_with(r#"Pair(Primitive { keyword: "double", dims: 0 })"#),
        "got {}",
        new_target(src, "new Pair(1.5)")
    );
}

/// Every reference type is a subtype of `java.lang.Object`, however it was declared.
///
/// Java writes the edge for you and the source never spells it, so there is no `extends` clause for
/// the index to have resolved — which left `class Foo {}` with an empty supertype chain and
/// `is_subtype(Foo, Object)` answering `false`. An interface is included deliberately: JLS §9.2
/// gives it no *superinterface*, but every interface-typed value is still an `Object`.
#[test]
fn every_reference_type_is_a_subtype_of_object() {
    let src = "
        class Plain {}
        interface Iface {}
        enum Color { RED }
        record Point(int x, int y) {}
        class Holder { Object make() { return new Object() {}; } }
    ";
    let fixture = Fixture::new(src);
    let index = &fixture.index;
    let object = index
        .item_by_fqn("java.lang.Object")
        .expect("the stubs declare java.lang.Object");
    for name in ["Plain", "Iface", "Color", "Point", "Holder"] {
        let id = index
            .item_by_fqn(name)
            .unwrap_or_else(|| panic!("`{name}` is indexed"));
        assert!(index.is_subtype(id, object), "`{name}` is not an Object");
    }
    // The anonymous class too — it has no name to look up, so find it by its enclosing declaration.
    let anonymous = index
        .items()
        .find(|(_, item)| {
            item.fqn.to_string().starts_with("Holder.") || item.fqn.to_string() == "Holder$1"
        })
        .map(|(id, _)| id);
    if let Some(id) = anonymous {
        assert!(
            index.is_subtype(id, object),
            "the anonymous class is not an Object"
        );
    }
}

/// `Object` does not extend itself, and a written `extends Object` is not doubled.
#[test]
fn the_implicit_object_edge_is_added_exactly_once() {
    let fixture = Fixture::new("class Written extends Object {}");
    let index = &fixture.index;
    let object = index
        .item_by_fqn("java.lang.Object")
        .expect("the stubs declare java.lang.Object");
    assert!(
        index.item(object).supertypes.is_empty(),
        "Object must not be its own supertype"
    );
    let written = index.item_by_fqn("Written").expect("Written is indexed");
    let to_object: Vec<bool> = index
        .item(written)
        .supertypes
        .iter()
        .filter(|sup| sup.id == object)
        .map(|sup| sup.implicit)
        .collect();
    assert_eq!(
        to_object,
        [false],
        "a written `extends Object` stays the one edge, and stays non-implicit"
    );
}

/// With no stubs and no classpath there is no `java.lang.Object` to point at, and the absence must
/// stay an absence: marking the type as having an *external* supertype instead would suppress every
/// "no member" conclusion in the workspace.
#[test]
fn the_implicit_object_edge_is_absent_without_an_indexed_object() {
    let node = jals_exec::block_on_inline(jals_syntax::Parse::parse("class Foo {}")).syntax();
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&[(FileId(0), node)]).build());
    let foo = index.item_by_fqn("Foo").expect("Foo is indexed");
    assert!(index.item(foo).supertypes.is_empty());
    assert!(
        index.method_set_complete(foo, "anything"),
        "an unindexed Object is not an external supertype"
    );
}

/// `super.f()` binds to the *overridden* member, not to the override.
///
/// That is the whole reason `super` is not given the enclosing type as its receiver: the enclosing
/// type's member set starts with the override, so answering the lookup from there would make
/// `super.f()` call itself. Its lookup starts at the superclass, and `resolve_member`'s walk begins
/// at the item it is handed — which is exactly right, because the superclass's own `f` is what
/// `super.f()` names.
#[test]
fn super_dot_method_binds_to_the_superclass_override() {
    let src = "
        class A { int f() { return 1; } }
        class B extends A { int f() { return 2; } int g() { return super.f(); } }
    ";
    let fixture = Fixture::new(src);
    let index = &fixture.index;
    let a = index.item_by_fqn("A").expect("A is indexed");
    let target = call_target(src, "super.f()");
    assert_eq!(target, "A.f()", "got {target}");
    let _ = a;
}

/// A field is *hidden* rather than overridden (JLS §15.11.2), and `super.x` names the hidden one.
#[test]
fn super_dot_field_binds_to_the_hidden_field() {
    let src = "
        class A { int x = 1; }
        class B extends A { int x = 2; int g() { return super.x; } }
    ";
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    let access = fixture
        .node
        .descendants()
        .filter_map(jals_syntax::ast::FieldAccess::cast)
        .find(|fa| fa.syntax().text().to_string().trim() == "super.x")
        .expect("super.x");
    let range = access.syntax().text_range();
    let id = typed
        .field_target_of(usize::from(range.start())..usize::from(range.end()))
        .expect("super.x binds to a field");
    let owner = fixture.index.item(fixture.index.member(id).owner);
    assert_eq!(owner.fqn.to_string(), "A", "super.x is A's hidden field");
}

/// The join between the implicit `java.lang.Object` edge and the `super` receiver: a class with no
/// `extends` still has a superclass to look `toString` up on.
#[test]
fn super_dot_object_method_resolves_after_the_implicit_edge() {
    let src = "class C { public String toString() { return super.toString(); } }";
    let target = call_target(src, "super.toString()");
    assert_eq!(target, "java.lang.Object.toString()", "got {target}");
}
