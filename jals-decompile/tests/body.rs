//! Method-body decompilation (`MethodBody::decompile`) over a real compiled class. Uses the
//! `Consts` fixture from `jals-classpath` (compiled with `-parameters -g`) so the straight-line
//! reconstructions — a field-storing constructor, an arithmetic return, an empty `void`, a `throw` —
//! are checked against actual bytecode.

use jals_classfile::{ClassFile, MethodInfo};
use jals_decompile::MethodBody;

fn fixture(bytes: &[u8]) -> ClassFile {
    ClassFile::read(bytes).expect("parse fixture class")
}

fn consts() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Consts.class"
    ))
}

fn branchy() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Branchy.class"
    ))
}

fn locals() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Locals.class"
    ))
}

fn loops() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Loops.class"
    ))
}

fn arrays() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Arrays.class"
    ))
}

fn concat() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Concat.class"
    ))
}

fn sb() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Sb.class"
    ))
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
    let body = MethodBody::decompile(method(&cf, "add"), &cf, &["delta".to_owned()])
        .expect("add decompiles");
    assert_eq!(body, ["return this.count + delta;"]);
}

#[test]
fn decompiles_field_storing_constructor() {
    let cf = consts();
    let body = MethodBody::decompile(method(&cf, "<init>"), &cf, &["start".to_owned()])
        .expect("constructor decompiles");
    // The implicit `super()` is omitted; only the field store remains.
    assert_eq!(body, ["this.count = start;"]);
}

#[test]
fn decompiles_throw_of_a_new_object() {
    let cf = consts();
    let body = MethodBody::decompile(method(&cf, "risky"), &cf, &["path".to_owned()])
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
    let names = ["a".to_owned(), "b".to_owned()];
    let body = MethodBody::decompile(method(&cf, "max"), &cf, &names).expect("max decompiles");
    assert_eq!(body, ["if (a > b) {", "    return a;", "}", "return b;"]);
}

#[test]
fn structures_an_if_else_with_a_join() {
    let cf = branchy();
    let body = MethodBody::decompile(method(&cf, "classify"), &cf, &["n".to_owned()])
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
    let names = ["n".to_owned()];
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
    let body = MethodBody::decompile(method(&cf, "pick"), &cf, &["c".to_owned()])
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
    let body = MethodBody::decompile(method(&cf, "nameLength"), &cf, &["s".to_owned()])
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
        MethodBody::decompile(method(&cf, "sum"), &cf, &["n".to_owned()]).expect("sum decompiles");
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
    let body = MethodBody::decompile(method(&cf, "count"), &cf, &["n".to_owned()])
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

#[test]
fn decompiles_array_element_read() {
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "first"), &cf, &["xs".to_owned()])
        .expect("first decompiles");
    assert_eq!(body, ["return xs[0];"]);
}

#[test]
fn decompiles_array_element_write() {
    let cf = arrays();
    let names = ["xs".to_owned(), "i".to_owned(), "v".to_owned()];
    let body = MethodBody::decompile(method(&cf, "put"), &cf, &names).expect("put decompiles");
    assert_eq!(body, ["xs[i] = v;"]);
}

#[test]
fn decompiles_new_primitive_array() {
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "fill"), &cf, &["n".to_owned()])
        .expect("fill decompiles");
    assert_eq!(body, ["return new int[n];"]);
}

#[test]
fn decompiles_new_object_array() {
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "blank"), &cf, &["n".to_owned()])
        .expect("blank decompiles");
    assert_eq!(body, ["return new java.lang.String[n];"]);
}

#[test]
fn decompiles_zero_length_array() {
    // A constant length with no element stores finalizes as a plain sized creation.
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "none"), &cf, &[]).expect("none decompiles");
    assert_eq!(body, ["return new int[0];"]);
}

#[test]
fn folds_int_array_initializer() {
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "pair"), &cf, &[]).expect("pair decompiles");
    assert_eq!(body, ["return new int[]{1, 2};"]);
}

#[test]
fn folds_string_array_initializer() {
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "tags"), &cf, &[]).expect("tags decompiles");
    assert_eq!(body, ["return new java.lang.String[]{\"x\", \"y\"};"]);
}

#[test]
fn folds_long_array_initializer() {
    // A category-2 element value is still a single expression on the simulated stack.
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "wide"), &cf, &["v".to_owned()])
        .expect("wide decompiles");
    assert_eq!(body, ["return new long[]{v};"]);
}

#[test]
fn folds_boolean_array_initializer() {
    // `bastore` stores int constants; the boolean element type maps them back to true/false.
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "flags"), &cf, &[]).expect("flags decompiles");
    assert_eq!(body, ["return new boolean[]{true, false};"]);
}

#[test]
fn folds_initializer_stored_to_local() {
    let cf = arrays();
    let body =
        MethodBody::decompile(method(&cf, "firstTwo"), &cf, &[]).expect("firstTwo decompiles");
    assert_eq!(
        body,
        [
            "int[] xs;",
            "xs = new int[]{3, 4};",
            "return xs[0] + xs[1];"
        ]
    );
}

#[test]
fn parenthesizes_new_array_receiver() {
    // A bare `new int[]{7}.length` is grammatical, but the creation is wrapped conservatively.
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "lenNew"), &cf, &[]).expect("lenNew decompiles");
    assert_eq!(body, ["return (new int[]{7}).length;"]);
}

#[test]
fn decompiles_arraylength() {
    let cf = arrays();
    let body =
        MethodBody::decompile(method(&cf, "len"), &cf, &["xs".to_owned()]).expect("len decompiles");
    assert_eq!(body, ["return xs.length;"]);
}

#[test]
fn decompiles_array_checkcast() {
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "narrow"), &cf, &["o".to_owned()])
        .expect("narrow decompiles");
    assert_eq!(body, ["return (int[]) o;"]);
}

#[test]
fn decompiles_multidim_new() {
    let cf = arrays();
    let names = ["a".to_owned(), "b".to_owned()];
    let body = MethodBody::decompile(method(&cf, "grid"), &cf, &names).expect("grid decompiles");
    assert_eq!(body, ["return new int[a][b];"]);
}

#[test]
fn decompiles_new_array_of_arrays() {
    // `anewarray [I`: the element class is itself an array type — one sized, one empty dimension.
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "rows"), &cf, &["n".to_owned()])
        .expect("rows decompiles");
    assert_eq!(body, ["return new int[n][];"]);
}

#[test]
fn folds_nested_array_initializer() {
    // The inner folded creations finalize as they are stored into the outer collection.
    let cf = arrays();
    let body = MethodBody::decompile(method(&cf, "nested"), &cf, &[]).expect("nested decompiles");
    assert_eq!(body, ["return new int[][]{new int[]{1}, new int[]{2}};"]);
}

#[test]
fn compound_element_store_bails() {
    // `xs[i]++` compiles to `dup2; iaload; iconst_1; iadd; iastore` — the stack shuffle is not
    // modelled, so the method must fall back rather than mis-render the store.
    let cf = arrays();
    let names = ["xs".to_owned(), "i".to_owned()];
    assert!(MethodBody::decompile(method(&cf, "bump"), &cf, &names).is_none());
}

// --- invokedynamic makeConcatWithConstants (javac's default string-concat lowering) ---

#[test]
fn folds_indy_concat_with_chunks() {
    // Recipe "Hello, \u{1}!" — literal chunks around one dynamic String operand.
    let cf = concat();
    let body = MethodBody::decompile(method(&cf, "greet"), &cf, &["name".to_owned()])
        .expect("greet decompiles");
    assert_eq!(body, ["return \"Hello, \" + name + \"!\";"]);
}

#[test]
fn folds_indy_concat_of_an_int() {
    let cf = concat();
    let body = MethodBody::decompile(method(&cf, "label"), &cf, &["n".to_owned()])
        .expect("label decompiles");
    assert_eq!(body, ["return \"n = \" + n;"]);
}

#[test]
fn string_typed_operand_anchors_the_chain() {
    // Recipe "\u{1}\u{1}" with a String first operand — no seed needed.
    let cf = concat();
    let names = ["a".to_owned(), "b".to_owned()];
    let body = MethodBody::decompile(method(&cf, "pair"), &cf, &names).expect("pair decompiles");
    assert_eq!(body, ["return a + b;"]);
}

#[test]
fn seeds_a_concat_with_no_string_operand() {
    // `a + "" + b` — the empty constant vanishes from the recipe, leaving two int operands;
    // rendering `a + b` would be integer addition, so the fold reintroduces the `""`.
    let cf = concat();
    let names = ["a".to_owned(), "b".to_owned()];
    let body = MethodBody::decompile(method(&cf, "bare"), &cf, &names).expect("bare decompiles");
    assert_eq!(body, ["return \"\" + a + b;"]);
}

#[test]
fn resolves_a_bootstrap_argument_constant() {
    // The "\u{1}" constant collides with the recipe's operand marker, so javac passes it as a
    // trailing bootstrap argument behind a "\u{2}" marker.
    let cf = concat();
    let body = MethodBody::decompile(method(&cf, "tagged"), &cf, &["n".to_owned()])
        .expect("tagged decompiles");
    assert_eq!(body, ["return \"\\u0001\" + n;"]);
}

#[test]
fn folds_indy_concat_of_a_char() {
    let cf = concat();
    let names = ["s".to_owned(), "c".to_owned()];
    let body = MethodBody::decompile(method(&cf, "glue"), &cf, &names).expect("glue decompiles");
    assert_eq!(body, ["return s + c;"]);
}

#[test]
fn folds_indy_concat_of_mixed_primitives() {
    let cf = concat();
    let names = ["d".to_owned(), "f".to_owned()];
    let body = MethodBody::decompile(method(&cf, "mix"), &cf, &names).expect("mix decompiles");
    assert_eq!(body, ["return d + \" & \" + f;"]);
}

#[test]
fn non_concat_invokedynamic_bails() {
    // A LambdaMetafactory call site is not modelled — the method must fall back.
    let cf = concat();
    assert!(MethodBody::decompile(method(&cf, "lazy"), &cf, &[]).is_none());
}

#[test]
fn discarded_object_creation_is_a_statement() {
    // `new Concat();` — the popped creation must survive as an expression statement.
    let cf = concat();
    let body = MethodBody::decompile(method(&cf, "ping"), &cf, &[]).expect("ping decompiles");
    assert_eq!(body, ["new demo.Concat();"]);
}

// --- StringBuilder append chains (javac -XDstringConcat=inline, and hand-written) ---

#[test]
fn folds_builder_chain_with_chunks() {
    let cf = sb();
    let body = MethodBody::decompile(method(&cf, "greet"), &cf, &["name".to_owned()])
        .expect("greet decompiles");
    assert_eq!(body, ["return \"Hello, \" + name + \"!\";"]);
}

#[test]
fn folds_builder_chain_of_an_int() {
    let cf = sb();
    let body = MethodBody::decompile(method(&cf, "label"), &cf, &["n".to_owned()])
        .expect("label decompiles");
    assert_eq!(body, ["return \"n = \" + n;"]);
}

#[test]
fn rerenders_an_appended_char_constant() {
    // `s + '!'` compiles to `bipush 33; append(C)` — the int constant must come back as a char.
    let cf = sb();
    let body = MethodBody::decompile(method(&cf, "excl"), &cf, &["s".to_owned()])
        .expect("excl decompiles");
    assert_eq!(body, ["return s + '!';"]);
}

#[test]
fn folds_builder_chain_of_a_boolean() {
    let cf = sb();
    let names = ["s".to_owned(), "b".to_owned()];
    let body = MethodBody::decompile(method(&cf, "flag"), &cf, &names).expect("flag decompiles");
    assert_eq!(body, ["return s + b;"]);
}

#[test]
fn empty_string_operand_survives_the_fold() {
    // `a + "" + b` — the appended `""` is the only String operand; dropping it would change the
    // chain to integer addition, so it must survive verbatim.
    let cf = sb();
    let names = ["a".to_owned(), "b".to_owned()];
    let body =
        MethodBody::decompile(method(&cf, "seeded"), &cf, &names).expect("seeded decompiles");
    assert_eq!(body, ["return a + \"\" + b;"]);
}

#[test]
fn unfinished_builder_chain_stays_calls() {
    // No toString() — the collecting chain re-renders as the original calls.
    let cf = sb();
    let body = MethodBody::decompile(method(&cf, "chain"), &cf, &["s".to_owned()])
        .expect("chain decompiles");
    assert_eq!(body, ["return new java.lang.StringBuilder().append(s);"]);
}

#[test]
fn builder_chain_consumed_by_another_call_stays_calls() {
    let cf = sb();
    let body =
        MethodBody::decompile(method(&cf, "len"), &cf, &["s".to_owned()]).expect("len decompiles");
    assert_eq!(
        body,
        ["return new java.lang.StringBuilder().append(s).length();"]
    );
}

#[test]
fn discarded_builder_chain_is_a_statement() {
    let cf = sb();
    let body = MethodBody::decompile(method(&cf, "drop"), &cf, &["s".to_owned()])
        .expect("drop decompiles");
    assert_eq!(body, ["new java.lang.StringBuilder().append(s);"]);
}

#[test]
fn append_on_a_parameter_stays_calls() {
    // The receiver is not a fresh `new StringBuilder()`, so nothing folds — including toString().
    let cf = sb();
    let body = MethodBody::decompile(method(&cf, "manual"), &cf, &["sb".to_owned()])
        .expect("manual decompiles");
    assert_eq!(body, ["return sb.append(\"x\").toString();"]);
}
