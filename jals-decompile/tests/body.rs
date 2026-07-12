//! Method-body decompilation (`MethodBody::decompile`) over a real compiled class. Uses the
//! `Consts` fixture from `jals-classpath` (compiled with `-parameters -g`) so the straight-line
//! reconstructions — a field-storing constructor, an arithmetic return, an empty `void`, a `throw` —
//! are checked against actual bytecode.

use jals_classfile::{ClassFile, MethodInfo};
use jals_decompile::MethodBody;

fn consts() -> ClassFile {
    ClassFile::read(include_bytes!(
        "../../jals-classpath/tests/fixtures/Consts.class"
    ))
    .expect("parse Consts.class")
}

fn branchy() -> ClassFile {
    ClassFile::read(include_bytes!(
        "../../jals-classpath/tests/fixtures/Branchy.class"
    ))
    .expect("parse Branchy.class")
}

fn locals() -> ClassFile {
    ClassFile::read(include_bytes!(
        "../../jals-classpath/tests/fixtures/Locals.class"
    ))
    .expect("parse Locals.class")
}

fn loops() -> ClassFile {
    ClassFile::read(include_bytes!(
        "../../jals-classpath/tests/fixtures/Loops.class"
    ))
    .expect("parse Loops.class")
}

/// The first method named `name`.
fn method<'a>(cf: &'a ClassFile, name: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| cf.constant_pool.utf8(m.name_index).as_deref() == Some(name))
        .expect("method present")
}

#[test]
fn decompiles_arithmetic_return() {
    let cf = consts();
    let body = MethodBody::decompile(method(&cf, "add"), &cf, &["delta".to_string()])
        .expect("add decompiles");
    assert_eq!(body, ["return this.count + delta;"]);
}

#[test]
fn decompiles_field_storing_constructor() {
    let cf = consts();
    let body = MethodBody::decompile(method(&cf, "<init>"), &cf, &["start".to_string()])
        .expect("constructor decompiles");
    // The implicit `super()` is omitted; only the field store remains.
    assert_eq!(body, ["this.count = start;"]);
}

#[test]
fn decompiles_throw_of_a_new_object() {
    let cf = consts();
    let body = MethodBody::decompile(method(&cf, "risky"), &cf, &["path".to_string()])
        .expect("risky decompiles");
    assert_eq!(body, ["throw new java.io.IOException(path);"]);
}

#[test]
fn empty_void_has_no_statements() {
    let cf = consts();
    let body = MethodBody::decompile(method(&cf, "reset"), &cf, &[]).expect("reset decompiles");
    assert!(body.is_empty(), "{body:?}");
}

#[test]
fn parameter_count_mismatch_bails() {
    // Passing the wrong number of names must yield no body — the body could otherwise reference a
    // parameter the signature does not declare (the enum-constructor safety net).
    let cf = consts();
    assert!(MethodBody::decompile(method(&cf, "add"), &cf, &[]).is_none());
}

#[test]
fn structures_a_guard_clause_if() {
    let cf = branchy();
    let names = ["a".to_string(), "b".to_string()];
    let body = MethodBody::decompile(method(&cf, "max"), &cf, &names).expect("max decompiles");
    assert_eq!(body, ["if (a > b) {", "    return a;", "}", "return b;"]);
}

#[test]
fn structures_an_if_else_with_a_join() {
    let cf = branchy();
    let body = MethodBody::decompile(method(&cf, "classify"), &cf, &["n".to_string()])
        .expect("classify decompiles");
    assert_eq!(
        body,
        [
            "if (n < 0) {",
            "    this.value = -1;",
            "} else {",
            "    this.value = 1;",
            "}",
            "this.value = this.value + 1;",
        ]
    );
}

#[test]
fn decompiles_straight_line_locals() {
    // Two temporaries, each hoisted to a typed declaration; the stores become plain assignments.
    let cf = locals();
    let names = ["n".to_string()];
    let body =
        MethodBody::decompile(method(&cf, "compute"), &cf, &names).expect("compute decompiles");
    assert_eq!(
        body,
        [
            "int doubled;",
            "int result;",
            "doubled = n * 2;",
            "result = doubled + 1;",
            "return result;",
        ]
    );
}

#[test]
fn hoists_a_local_across_an_if_else() {
    // `x` is written in both branches and read after the join — hoisting keeps it in scope.
    let cf = locals();
    let body = MethodBody::decompile(method(&cf, "pick"), &cf, &["c".to_string()])
        .expect("pick decompiles");
    assert_eq!(
        body,
        [
            "int x;",
            "if (c) {",
            "    x = 1;",
            "} else {",
            "    x = 2;",
            "}",
            "return x;",
        ]
    );
}

#[test]
fn decompiles_a_reference_typed_local() {
    let cf = locals();
    let body = MethodBody::decompile(method(&cf, "nameLength"), &cf, &["s".to_string()])
        .expect("nameLength decompiles");
    assert_eq!(
        body,
        ["java.lang.String t;", "t = s;", "return t.length();"]
    );
}

#[test]
fn structures_a_bottom_test_while() {
    // javac's default loop layout: a top-of-body condition test with a `goto` back-edge, recovered
    // as `while (i < n)`. The loop counter `i` and accumulator `total` are hoisted locals.
    let cf = loops();
    let body =
        MethodBody::decompile(method(&cf, "sum"), &cf, &["n".to_string()]).expect("sum decompiles");
    assert_eq!(
        body,
        [
            "int total;",
            "int i;",
            "total = 0;",
            "i = 0;",
            "while (i < n) {",
            "    total = total + i;",
            "    i = i + 1;",
            "}",
            "return total;",
        ]
    );
}

#[test]
fn structures_a_do_while() {
    // The condition is tested at the bottom (a conditional back-branch), recovered as
    // `do { ... } while (c < n);`.
    let cf = loops();
    let body = MethodBody::decompile(method(&cf, "count"), &cf, &["n".to_string()])
        .expect("count decompiles");
    assert_eq!(
        body,
        [
            "int c;",
            "c = 0;",
            "do {",
            "    c = c + 1;",
            "} while (c < n);",
            "return c;",
        ]
    );
}
