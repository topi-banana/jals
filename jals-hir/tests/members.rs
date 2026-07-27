//! The facts a code generator reads off the index: which overload a call binds to, how a member is
//! reached, and what a constructor's parameters are.
//!
//! These are not checking questions. Nothing here decides whether a program is legal — it decides
//! which instruction and which descriptor a backend has to emit for a program already assumed to be.

use jals_hir::{FileId, ProjectIndex, Resolved, TypeInference};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::AstNode;

/// Parses `src` and indexes it as a single-file project, with the embedded stdlib stubs folded in
/// so `System.out.println` resolves.
fn analyse(src: &str) -> (SyntaxNode, ProjectIndex, TypeInference) {
    let node = jals_exec::block_on_inline(jals_syntax::Parse::parse(src)).syntax();
    let resolved = jals_exec::block_on_inline(Resolved::resolve_node(&node));
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), node.clone())])
            .with_stdlib()
            .build(),
    );
    let inference =
        jals_exec::block_on_inline(TypeInference::infer(&node, &resolved, &index, FileId(0)));
    (node, index, inference)
}

/// The member the (first) call whose source text is exactly `text` binds to, rendered as
/// `Owner.name(paramTypes)`.
fn call_target(src: &str, text: &str) -> String {
    let (node, index, inference) = analyse(src);
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
    let (_, index, _) = analyse(src);
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
    let (_, index, _) = analyse(src);
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
    let (_, index, _) = analyse(src);
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
    let (node, index, inference) = analyse(src);
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
