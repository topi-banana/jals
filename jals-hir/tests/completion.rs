//! Tests for `jals_hir::member_completions`: anchoring on the `.` before the cursor, inferring the
//! receiver, and enumerating its fields and methods (own and inherited) on a project type.

use jals_hir::{
    Completion, DefKind, FileId, ProjectIndex, Resolved, at_member_access, member_completions,
    resolve_node, scope_completions,
};
use jals_syntax::SyntaxNode;

/// Build an index over `sources`, place the cursor at the `$0` marker in `sources[file]` (removed
/// before parsing), and run the completion function `run` there.
fn at(
    sources: &[&str],
    file: usize,
    run: impl Fn(&SyntaxNode, &Resolved, &ProjectIndex, FileId, usize) -> Vec<Completion>,
) -> Vec<Completion> {
    let mut texts: Vec<String> = sources.iter().map(|s| s.to_string()).collect();
    let offset = texts[file].find("$0").expect("a $0 cursor marker");
    texts[file].replace_range(offset..offset + 2, "");
    let nodes: Vec<(FileId, SyntaxNode)> = texts
        .iter()
        .enumerate()
        .map(|(i, s)| (FileId(i as u32), jals_syntax::parse(s).syntax()))
        .collect();
    let index = ProjectIndex::build(&nodes);
    let (fid, root) = &nodes[file];
    let resolved = resolve_node(root);
    run(root, &resolved, &index, *fid, offset)
}

/// Run member completion at the `$0` marker in `sources[file]`.
fn complete(sources: &[&str], file: usize) -> Vec<Completion> {
    at(sources, file, member_completions)
}

/// Run scope completion (a non-member-access position) at the `$0` marker in `sources[file]`.
fn scope(sources: &[&str], file: usize) -> Vec<Completion> {
    at(sources, file, scope_completions)
}

/// The labels of the scope completions at `$0`, sorted.
fn scope_labels(sources: &[&str], file: usize) -> Vec<String> {
    let mut out: Vec<String> = scope(sources, file).into_iter().map(|c| c.label).collect();
    out.sort();
    out
}

/// The `(label, kind, detail)` of each completion, sorted by label for order-independent assertions.
fn items(sources: &[&str], file: usize) -> Vec<(String, DefKind, String)> {
    let mut out: Vec<(String, DefKind, String)> = complete(sources, file)
        .into_iter()
        .map(|c| (c.label, c.kind, c.detail))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn labels(sources: &[&str], file: usize) -> Vec<String> {
    let mut out: Vec<String> = complete(sources, file)
        .into_iter()
        .map(|c| c.label)
        .collect();
    out.sort();
    out
}

#[test]
fn completes_fields_and_methods_with_detail() {
    let src = "class Box { int size; long id; int area(int w, int h) { return 0; } \
               void m(Box b) { b.$0 } }";
    // `m` is itself a member of `Box`, so `b.m()` is a valid member access and `m` completes too.
    assert_eq!(
        items(&[src], 0),
        [
            (
                "area".to_string(),
                DefKind::Method,
                "(int w, int h): int".to_string()
            ),
            ("id".to_string(), DefKind::Field, "long".to_string()),
            (
                "m".to_string(),
                DefKind::Method,
                "(Box b): void".to_string()
            ),
            ("size".to_string(), DefKind::Field, "int".to_string()),
        ]
    );
}

#[test]
fn completes_a_partially_typed_member() {
    // A partial member name parses as a field access; the receiver is still `b`, not `b.si`. The
    // server returns the full member set (the editor filters by the typed prefix `si`).
    let src = "class Box { int size; int area() { return 0; } void m(Box b) { b.si$0 } }";
    assert_eq!(labels(&[src], 0), ["area", "m", "size"]);
}

#[test]
fn includes_inherited_members_across_files() {
    let base = "package a; class Base { int inherited; }";
    let sub = "package a; class Sub extends Base { int own; void m(Sub s) { s.$0 } }";
    assert_eq!(labels(&[base, sub], 1), ["inherited", "m", "own"]);
}

#[test]
fn this_completes_the_enclosing_type() {
    let src = "class C { int field; void helper() {} void m() { this.$0 } }";
    assert_eq!(labels(&[src], 0), ["field", "helper", "m"]);
}

#[test]
fn an_override_and_overloads_collapse_to_one_entry() {
    // `Sub` overrides `name()` and declares two `f` overloads; each name appears once.
    let base = "package a; class Base { String name() { return null; } }";
    let sub = "package a; class Sub extends Base { \
               String name() { return null; } void f(int a) {} void f(String a) {} \
               void m(Sub s) { s.$0 } }";
    assert_eq!(labels(&[base, sub], 1), ["f", "m", "name"]);
}

#[test]
fn external_receiver_yields_nothing() {
    // `String` is not an indexed project type, so its members are not known here.
    let src = "class C { void m(String s) { s.$0 } }";
    assert!(complete(&[src], 0).is_empty());
}

#[test]
fn not_a_member_access_yields_nothing() {
    let src = "class C { int x = 0$0; }";
    assert!(complete(&[src], 0).is_empty());
}

#[test]
fn completes_on_a_chained_receiver() {
    // `b.self()` returns `Box`, so the second `.` completes `Box`'s members again.
    let src = "class Box { int size; Box self() { return this; } void m(Box b) { b.self().$0 } }";
    assert_eq!(labels(&[src], 0), ["m", "self", "size"]);
}

#[test]
fn scope_offers_locals_params_and_members() {
    // At `$0` the visible names are: the local `total` (declared before the cursor), the parameter
    // `n`, the field `count`, the method `helper`, and the enclosing class `C`.
    let src = "class C { int count; void helper() {} \
               void m(int n) { int total = 0; $0 } }";
    let labels = scope_labels(&[src], 0);
    for expected in ["C", "count", "helper", "m", "n", "total"] {
        assert!(
            labels.contains(&expected.to_string()),
            "missing `{expected}` in {labels:?}"
        );
    }
}

#[test]
fn scope_hides_a_local_declared_after_the_cursor() {
    // `early` is declared before `$0`; `late` after it — a block is sequential, so only `early` is
    // visible at the cursor.
    let src = "class C { void m() { int early = 1; $0 int late = 2; } }";
    let labels = scope_labels(&[src], 0);
    assert!(labels.contains(&"early".to_string()));
    assert!(!labels.contains(&"late".to_string()));
}

#[test]
fn scope_offers_project_types_from_other_files() {
    let other = "package a; class Helper { }";
    let main = "package a; class Main { void m() { $0 } }";
    let labels = scope_labels(&[other, main], 1);
    assert!(labels.contains(&"Helper".to_string()));
    assert!(labels.contains(&"Main".to_string()));
}

#[test]
fn at_member_access_distinguishes_the_two_contexts() {
    let src = "class C { void m(C c) { c. } }";
    let root = jals_syntax::parse(src).syntax();
    // Right after `c.`: a member-access position.
    let dot = src.find("c.").unwrap() + 2;
    assert!(at_member_access(&root, dot));
    // Inside the method body but not after a dot: a scope position.
    let body = src.find("{ c").unwrap() + 1;
    assert!(!at_member_access(&root, body));
}

#[test]
fn never_panics_on_broken_input() {
    for src in [
        "",
        "class",
        "class C { void m() { x. } }",
        "a.",
        ".",
        "class C { .$0 }",
    ] {
        let nodes = [(FileId(0), jals_syntax::parse(src).syntax())];
        let index = ProjectIndex::build(&nodes);
        let resolved = resolve_node(&nodes[0].1);
        for offset in [0, src.len(), src.len().saturating_sub(1)] {
            let _ = member_completions(&nodes[0].1, &resolved, &index, FileId(0), offset);
        }
    }
}
