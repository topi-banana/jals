//! Method-body decompilation (`decompile_method_body`) over a real compiled class. Uses the
//! `Consts` fixture from `jals-classpath` (compiled with `-parameters -g`) so the straight-line
//! reconstructions — a field-storing constructor, an arithmetic return, an empty `void`, a `throw` —
//! are checked against actual bytecode.

use jals_classfile::{ClassFile, MethodInfo};
use jals_decompile::decompile_method_body;

fn consts() -> ClassFile {
    jals_classfile::read(include_bytes!(
        "../../jals-classpath/tests/fixtures/Consts.class"
    ))
    .expect("parse Consts.class")
}

fn branchy() -> ClassFile {
    jals_classfile::read(include_bytes!(
        "../../jals-classpath/tests/fixtures/Branchy.class"
    ))
    .expect("parse Branchy.class")
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
    let body = decompile_method_body(method(&cf, "add"), &cf, &["delta".to_string()])
        .expect("add decompiles");
    assert_eq!(body, ["return this.count + delta;"]);
}

#[test]
fn decompiles_field_storing_constructor() {
    let cf = consts();
    let body = decompile_method_body(method(&cf, "<init>"), &cf, &["start".to_string()])
        .expect("constructor decompiles");
    // The implicit `super()` is omitted; only the field store remains.
    assert_eq!(body, ["this.count = start;"]);
}

#[test]
fn decompiles_throw_of_a_new_object() {
    let cf = consts();
    let body = decompile_method_body(method(&cf, "risky"), &cf, &["path".to_string()])
        .expect("risky decompiles");
    assert_eq!(body, ["throw new java.io.IOException(path);"]);
}

#[test]
fn empty_void_has_no_statements() {
    let cf = consts();
    let body = decompile_method_body(method(&cf, "reset"), &cf, &[]).expect("reset decompiles");
    assert!(body.is_empty(), "{body:?}");
}

#[test]
fn parameter_count_mismatch_bails() {
    // Passing the wrong number of names must yield no body — the body could otherwise reference a
    // parameter the signature does not declare (the enum-constructor safety net).
    let cf = consts();
    assert!(decompile_method_body(method(&cf, "add"), &cf, &[]).is_none());
}

#[test]
fn structures_a_guard_clause_if() {
    let cf = branchy();
    let names = ["a".to_string(), "b".to_string()];
    let body = decompile_method_body(method(&cf, "max"), &cf, &names).expect("max decompiles");
    assert_eq!(body, ["if (a > b) {", "    return a;", "}", "return b;"]);
}

#[test]
fn structures_an_if_else_with_a_join() {
    let cf = branchy();
    let body = decompile_method_body(method(&cf, "classify"), &cf, &["n".to_string()])
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
