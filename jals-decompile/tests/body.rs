//! Method-body decompilation (`MethodBody::decompile`) over real classes compiled with
//! `-parameters -g`, so reconstruction is checked against actual javac bytecode.

use jals_classfile::{ClassFile, MethodInfo};
use jals_decompile::MethodBody;
use jals_exec::block_on_inline;

fn fixture(bytes: &[u8]) -> ClassFile {
    block_on_inline(ClassFile::read(bytes)).expect("parse fixture class")
}

/// Synchronous test-side driver for the async [`MethodBody::decompile`].
fn decompile(method: &MethodInfo, cf: &ClassFile, param_names: &[String]) -> Option<Vec<String>> {
    block_on_inline(MethodBody::decompile(method, cf, param_names))
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

fn cmp() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Cmp.class"
    ))
}

fn int_carried() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/IntCarried.class"
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
    let body = decompile(method(&cf, "add"), &cf, &["delta".to_owned()]).expect("add decompiles");
    assert_eq!(body, ["return this.count + delta;"]);
}

#[test]
fn decompiles_field_storing_constructor() {
    let cf = consts();
    let body = decompile(method(&cf, "<init>"), &cf, &["start".to_owned()])
        .expect("constructor decompiles");
    // The implicit `super()` is omitted; only the field store remains.
    assert_eq!(body, ["this.count = start;"]);
}

#[test]
fn decompiles_throw_of_a_new_object() {
    let cf = consts();
    let body =
        decompile(method(&cf, "risky"), &cf, &["path".to_owned()]).expect("risky decompiles");
    assert_eq!(body, ["throw new java.io.IOException(path);"]);
}

#[test]
fn empty_void_has_no_statements() {
    let cf = consts();
    let body = decompile(method(&cf, "reset"), &cf, &[]).expect("reset decompiles");
    assert!(body.is_empty(), "{body:?}");
}

#[test]
fn parameter_count_mismatch_bails() {
    // Passing the wrong number of names must yield no body — the body could otherwise reference a
    // parameter the signature does not declare (the enum-constructor safety net).
    let cf = consts();
    assert!(decompile(method(&cf, "add"), &cf, &[]).is_none());
}

#[test]
fn structures_a_guard_clause_if() {
    let cf = branchy();
    let names = ["a".to_owned(), "b".to_owned()];
    let body = decompile(method(&cf, "max"), &cf, &names).expect("max decompiles");
    assert_eq!(body, ["if (a > b) {", "    return a;", "}", "return b;"]);
}

#[test]
fn structures_an_if_else_with_a_join() {
    let cf = branchy();
    let body =
        decompile(method(&cf, "classify"), &cf, &["n".to_owned()]).expect("classify decompiles");
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
    let body = decompile(method(&cf, "compute"), &cf, &names).expect("compute decompiles");
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
    let body = decompile(method(&cf, "pick"), &cf, &["c".to_owned()]).expect("pick decompiles");
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
    let body = decompile(method(&cf, "nameLength"), &cf, &["s".to_owned()])
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
    let body = decompile(method(&cf, "sum"), &cf, &["n".to_owned()]).expect("sum decompiles");
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
    let body = decompile(method(&cf, "count"), &cf, &["n".to_owned()]).expect("count decompiles");
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
    let body = decompile(method(&cf, "first"), &cf, &["xs".to_owned()]).expect("first decompiles");
    assert_eq!(body, ["return xs[0];"]);
}

#[test]
fn decompiles_array_element_write() {
    let cf = arrays();
    let names = ["xs".to_owned(), "i".to_owned(), "v".to_owned()];
    let body = decompile(method(&cf, "put"), &cf, &names).expect("put decompiles");
    assert_eq!(body, ["xs[i] = v;"]);
}

#[test]
fn decompiles_new_primitive_array() {
    let cf = arrays();
    let body = decompile(method(&cf, "fill"), &cf, &["n".to_owned()]).expect("fill decompiles");
    assert_eq!(body, ["return new int[n];"]);
}

#[test]
fn decompiles_new_object_array() {
    let cf = arrays();
    let body = decompile(method(&cf, "blank"), &cf, &["n".to_owned()]).expect("blank decompiles");
    assert_eq!(body, ["return new java.lang.String[n];"]);
}

#[test]
fn decompiles_zero_length_array() {
    // A constant length with no element stores finalizes as a plain sized creation.
    let cf = arrays();
    let body = decompile(method(&cf, "none"), &cf, &[]).expect("none decompiles");
    assert_eq!(body, ["return new int[0];"]);
}

#[test]
fn folds_int_array_initializer() {
    let cf = arrays();
    let body = decompile(method(&cf, "pair"), &cf, &[]).expect("pair decompiles");
    assert_eq!(body, ["return new int[]{1, 2};"]);
}

#[test]
fn folds_string_array_initializer() {
    let cf = arrays();
    let body = decompile(method(&cf, "tags"), &cf, &[]).expect("tags decompiles");
    assert_eq!(body, ["return new java.lang.String[]{\"x\", \"y\"};"]);
}

#[test]
fn folds_long_array_initializer() {
    // A category-2 element value is still a single expression on the simulated stack.
    let cf = arrays();
    let body = decompile(method(&cf, "wide"), &cf, &["v".to_owned()]).expect("wide decompiles");
    assert_eq!(body, ["return new long[]{v};"]);
}

#[test]
fn folds_boolean_array_initializer() {
    // `bastore` stores int constants; the boolean element type maps them back to true/false.
    let cf = arrays();
    let body = decompile(method(&cf, "flags"), &cf, &[]).expect("flags decompiles");
    assert_eq!(body, ["return new boolean[]{true, false};"]);
}

#[test]
fn folds_initializer_stored_to_local() {
    let cf = arrays();
    let body = decompile(method(&cf, "firstTwo"), &cf, &[]).expect("firstTwo decompiles");
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
    let body = decompile(method(&cf, "lenNew"), &cf, &[]).expect("lenNew decompiles");
    assert_eq!(body, ["return (new int[]{7}).length;"]);
}

#[test]
fn decompiles_arraylength() {
    let cf = arrays();
    let body = decompile(method(&cf, "len"), &cf, &["xs".to_owned()]).expect("len decompiles");
    assert_eq!(body, ["return xs.length;"]);
}

#[test]
fn decompiles_array_checkcast() {
    let cf = arrays();
    let body = decompile(method(&cf, "narrow"), &cf, &["o".to_owned()]).expect("narrow decompiles");
    assert_eq!(body, ["return (int[]) o;"]);
}

#[test]
fn decompiles_multidim_new() {
    let cf = arrays();
    let names = ["a".to_owned(), "b".to_owned()];
    let body = decompile(method(&cf, "grid"), &cf, &names).expect("grid decompiles");
    assert_eq!(body, ["return new int[a][b];"]);
}

#[test]
fn decompiles_new_array_of_arrays() {
    // `anewarray [I`: the element class is itself an array type — one sized, one empty dimension.
    let cf = arrays();
    let body = decompile(method(&cf, "rows"), &cf, &["n".to_owned()]).expect("rows decompiles");
    assert_eq!(body, ["return new int[n][];"]);
}

#[test]
fn folds_nested_array_initializer() {
    // The inner folded creations finalize as they are stored into the outer collection.
    let cf = arrays();
    let body = decompile(method(&cf, "nested"), &cf, &[]).expect("nested decompiles");
    assert_eq!(body, ["return new int[][]{new int[]{1}, new int[]{2}};"]);
}

#[test]
fn compound_element_store_bails() {
    // `xs[i]++` compiles to `dup2; iaload; iconst_1; iadd; iastore` — the stack shuffle is not
    // modelled, so the method must fall back rather than mis-render the store.
    let cf = arrays();
    let names = ["xs".to_owned(), "i".to_owned()];
    assert!(decompile(method(&cf, "bump"), &cf, &names).is_none());
}

// --- JVM int-carried boolean and char values ---

#[test]
fn recovers_boolean_and_char_returns() {
    let cf = int_carried();
    let boolean =
        decompile(method(&cf, "booleanReturn"), &cf, &[]).expect("booleanReturn decompiles");
    let character = decompile(method(&cf, "charReturn"), &cf, &[]).expect("charReturn decompiles");
    assert_eq!(boolean, ["return true;"]);
    assert_eq!(character, ["return 'A';"]);
}

#[test]
fn recovers_boolean_and_char_locals() {
    let cf = int_carried();
    let boolean =
        decompile(method(&cf, "booleanLocal"), &cf, &[]).expect("booleanLocal decompiles");
    let character = decompile(method(&cf, "charLocal"), &cf, &[]).expect("charLocal decompiles");
    assert_eq!(
        boolean,
        ["boolean value;", "value = true;", "return value;"]
    );
    assert_eq!(character, ["char value;", "value = 'B';", "return value;"]);
}

#[test]
fn recovers_boolean_and_char_fields() {
    let cf = int_carried();
    let stores = decompile(method(&cf, "storeFields"), &cf, &[]).expect("storeFields decompiles");
    let boolean = decompile(method(&cf, "readFlag"), &cf, &[]).expect("readFlag decompiles");
    let character = decompile(method(&cf, "readLetter"), &cf, &[]).expect("readLetter decompiles");
    assert_eq!(stores, ["this.flag = true;", "this.letter = 'C';"]);
    assert_eq!(boolean, ["return this.flag;"]);
    assert_eq!(character, ["return this.letter;"]);
}

#[test]
fn recovers_boolean_and_char_call_arguments_and_results() {
    let cf = int_carried();
    let boolean = decompile(method(&cf, "callBoolean"), &cf, &[]).expect("callBoolean decompiles");
    let character = decompile(method(&cf, "callChar"), &cf, &[]).expect("callChar decompiles");
    let result = decompile(method(&cf, "branchOnCall"), &cf, &["value".to_owned()])
        .expect("branchOnCall decompiles");
    assert_eq!(boolean, ["return this.passBoolean(true);"]);
    assert_eq!(character, ["return this.passChar('D');"]);
    assert_eq!(
        result,
        [
            "if (!this.passBoolean(value)) {",
            "    return 1;",
            "}",
            "return 2;"
        ]
    );
}

#[test]
fn preserves_char_to_int_widening() {
    let cf = int_carried();
    let names = ["value".to_owned()];
    let call =
        decompile(method(&cf, "widenedCharCall"), &cf, &names).expect("widenedCharCall decompiles");
    let concat = decompile(method(&cf, "widenedCharConcat"), &cf, &names)
        .expect("widenedCharConcat decompiles");
    assert_eq!(call, ["return this.charOrInt((int) value);"]);
    assert_eq!(concat, ["return \"\" + (int) value;"]);
}

#[test]
fn recovers_boolean_and_char_arrays() {
    let cf = int_carried();
    let booleans =
        decompile(method(&cf, "booleanArray"), &cf, &[]).expect("booleanArray decompiles");
    let characters = decompile(method(&cf, "charArray"), &cf, &[]).expect("charArray decompiles");
    let names = ["flags".to_owned(), "letters".to_owned()];
    let stores =
        decompile(method(&cf, "storeArrays"), &cf, &names).expect("storeArrays decompiles");
    let boolean_read = decompile(method(&cf, "readBoolean"), &cf, &["values".to_owned()])
        .expect("readBoolean decompiles");
    let char_read = decompile(method(&cf, "readChar"), &cf, &["values".to_owned()])
        .expect("readChar decompiles");
    assert_eq!(booleans, ["return new boolean[]{true, false};"]);
    assert_eq!(characters, ["return new char[]{'E', (char) 55296};"]);
    assert_eq!(stores, ["flags[0] = true;", "letters[0] = 'F';"]);
    assert_eq!(boolean_read, ["return values[0];"]);
    assert_eq!(char_read, ["return values[0];"]);
}

#[test]
fn distinguishes_integer_zero_from_boolean_negation() {
    // javac emits the same `iload; ifne` pair for both methods; the local's descriptor determines
    // whether the source condition is an integer comparison or boolean negation.
    let cf = int_carried();
    let names = ["value".to_owned()];
    let integer =
        decompile(method(&cf, "integerZero"), &cf, &names).expect("integerZero decompiles");
    let boolean =
        decompile(method(&cf, "booleanNegation"), &cf, &names).expect("booleanNegation decompiles");
    assert_eq!(
        integer,
        ["if (value == 0) {", "    return 1;", "}", "return 2;"]
    );
    assert_eq!(
        boolean,
        ["if (!value) {", "    return 1;", "}", "return 2;"]
    );
}

#[test]
fn recovers_char_casts_including_a_surrogate() {
    let cf = int_carried();
    let cast = decompile(method(&cf, "castChar"), &cf, &["value".to_owned()])
        .expect("castChar decompiles");
    let surrogate = decompile(method(&cf, "surrogate"), &cf, &[]).expect("surrogate decompiles");
    assert_eq!(cast, ["return (char) value;"]);
    // A lone UTF-16 surrogate is not a Unicode scalar, so preserve its code unit as a cast.
    assert_eq!(surrogate, ["return (char) 55296;"]);
}

// --- invokedynamic makeConcatWithConstants (javac's default string-concat lowering) ---

#[test]
fn folds_indy_concat_with_chunks() {
    // Recipe "Hello, \u{1}!" — literal chunks around one dynamic String operand.
    let cf = concat();
    let body =
        decompile(method(&cf, "greet"), &cf, &["name".to_owned()]).expect("greet decompiles");
    assert_eq!(body, ["return \"Hello, \" + name + \"!\";"]);
}

#[test]
fn folds_indy_concat_of_an_int() {
    let cf = concat();
    let body = decompile(method(&cf, "label"), &cf, &["n".to_owned()]).expect("label decompiles");
    assert_eq!(body, ["return \"n = \" + n;"]);
}

#[test]
fn string_typed_operand_anchors_the_chain() {
    // Recipe "\u{1}\u{1}" with a String first operand — no seed needed.
    let cf = concat();
    let names = ["a".to_owned(), "b".to_owned()];
    let body = decompile(method(&cf, "pair"), &cf, &names).expect("pair decompiles");
    assert_eq!(body, ["return a + b;"]);
}

#[test]
fn seeds_a_concat_with_no_string_operand() {
    // `a + "" + b` — the empty constant vanishes from the recipe, leaving two int operands;
    // rendering `a + b` would be integer addition, so the fold reintroduces the `""`.
    let cf = concat();
    let names = ["a".to_owned(), "b".to_owned()];
    let body = decompile(method(&cf, "bare"), &cf, &names).expect("bare decompiles");
    assert_eq!(body, ["return \"\" + a + b;"]);
}

#[test]
fn resolves_a_bootstrap_argument_constant() {
    // The "\u{1}" constant collides with the recipe's operand marker, so javac passes it as a
    // trailing bootstrap argument behind a "\u{2}" marker.
    let cf = concat();
    let body = decompile(method(&cf, "tagged"), &cf, &["n".to_owned()]).expect("tagged decompiles");
    assert_eq!(body, ["return \"\\u0001\" + n;"]);
}

#[test]
fn folds_indy_concat_of_a_char() {
    let cf = concat();
    let names = ["s".to_owned(), "c".to_owned()];
    let body = decompile(method(&cf, "glue"), &cf, &names).expect("glue decompiles");
    assert_eq!(body, ["return s + c;"]);
}

#[test]
fn folds_indy_concat_of_mixed_primitives() {
    let cf = concat();
    let names = ["d".to_owned(), "f".to_owned()];
    let body = decompile(method(&cf, "mix"), &cf, &names).expect("mix decompiles");
    assert_eq!(body, ["return d + \" & \" + f;"]);
}

#[test]
fn non_concat_invokedynamic_bails() {
    // A LambdaMetafactory call site is not modelled — the method must fall back.
    let cf = concat();
    assert!(decompile(method(&cf, "lazy"), &cf, &[]).is_none());
}

#[test]
fn discarded_object_creation_is_a_statement() {
    // `new Concat();` — the popped creation must survive as an expression statement.
    let cf = concat();
    let body = decompile(method(&cf, "ping"), &cf, &[]).expect("ping decompiles");
    assert_eq!(body, ["new demo.Concat();"]);
}

// --- StringBuilder append chains (javac -XDstringConcat=inline, and hand-written) ---

#[test]
fn folds_builder_chain_with_chunks() {
    let cf = sb();
    let body =
        decompile(method(&cf, "greet"), &cf, &["name".to_owned()]).expect("greet decompiles");
    assert_eq!(body, ["return \"Hello, \" + name + \"!\";"]);
}

#[test]
fn folds_builder_chain_of_an_int() {
    let cf = sb();
    let body = decompile(method(&cf, "label"), &cf, &["n".to_owned()]).expect("label decompiles");
    assert_eq!(body, ["return \"n = \" + n;"]);
}

#[test]
fn rerenders_an_appended_char_constant() {
    // `s + '!'` compiles to `bipush 33; append(C)` — the int constant must come back as a char.
    let cf = sb();
    let body = decompile(method(&cf, "excl"), &cf, &["s".to_owned()]).expect("excl decompiles");
    assert_eq!(body, ["return s + '!';"]);
}

#[test]
fn folds_builder_chain_of_a_boolean() {
    let cf = sb();
    let names = ["s".to_owned(), "b".to_owned()];
    let body = decompile(method(&cf, "flag"), &cf, &names).expect("flag decompiles");
    assert_eq!(body, ["return s + b;"]);
}

#[test]
fn empty_string_operand_survives_the_fold() {
    // `a + "" + b` — the appended `""` is the only String operand; dropping it would change the
    // chain to integer addition, so it must survive verbatim.
    let cf = sb();
    let names = ["a".to_owned(), "b".to_owned()];
    let body = decompile(method(&cf, "seeded"), &cf, &names).expect("seeded decompiles");
    assert_eq!(body, ["return a + \"\" + b;"]);
}

#[test]
fn unfinished_builder_chain_stays_calls() {
    // No toString() — the collecting chain re-renders as the original calls.
    let cf = sb();
    let body = decompile(method(&cf, "chain"), &cf, &["s".to_owned()]).expect("chain decompiles");
    assert_eq!(body, ["return new java.lang.StringBuilder().append(s);"]);
}

#[test]
fn builder_chain_consumed_by_another_call_stays_calls() {
    let cf = sb();
    let body = decompile(method(&cf, "len"), &cf, &["s".to_owned()]).expect("len decompiles");
    assert_eq!(
        body,
        ["return new java.lang.StringBuilder().append(s).length();"]
    );
}

#[test]
fn discarded_builder_chain_is_a_statement() {
    let cf = sb();
    let body = decompile(method(&cf, "drop"), &cf, &["s".to_owned()]).expect("drop decompiles");
    assert_eq!(body, ["new java.lang.StringBuilder().append(s);"]);
}

#[test]
fn append_on_a_parameter_stays_calls() {
    // The receiver is not a fresh `new StringBuilder()`, so nothing folds — including toString().
    let cf = sb();
    let body =
        decompile(method(&cf, "manual"), &cf, &["sb".to_owned()]).expect("manual decompiles");
    assert_eq!(body, ["return sb.append(\"x\").toString();"]);
}

#[test]
fn recovers_a_long_comparison() {
    // `lcmp; ifle` — the fall-through of the fused pair reads back as `a > b`.
    let cf = cmp();
    let names = ["a".to_owned(), "b".to_owned()];
    let body = decompile(method(&cf, "max"), &cf, &names).expect("max decompiles");
    assert_eq!(body, ["if (a > b) {", "    return a;", "}", "return b;"]);
}

#[test]
fn recovers_a_float_comparison_against_zero() {
    // `fcmpg; ifge` — NaN falls to the else side, so `<` is exact.
    let cf = cmp();
    let body = decompile(method(&cf, "floor"), &cf, &["f".to_owned()]).expect("floor decompiles");
    assert_eq!(body, ["if (f < 0f) {", "    return 0f;", "}", "return f;"]);
}

#[test]
fn recovers_a_cmpl_flavored_ge() {
    // `fcmpl; iflt` — the `*cmpl` flavor keeps `>=` exact on NaN.
    let cf = cmp();
    let body =
        decompile(method(&cf, "atLeast"), &cf, &["f".to_owned()]).expect("atLeast decompiles");
    assert_eq!(body, ["if (f >= 1f) {", "    return f;", "}", "return 1f;"]);
}

#[test]
fn recovers_a_double_le_comparison() {
    // `dcmpg; ifgt` — the fall-through reads back as `<=`.
    let cf = cmp();
    let body = decompile(method(&cf, "cap"), &cf, &["d".to_owned()]).expect("cap decompiles");
    assert_eq!(body, ["if (d <= 0d) {", "    return 0d;", "}", "return d;"]);
}

#[test]
fn recovers_double_equality() {
    // `dcmpl; ifne` — `==` is exact under either flavor (NaN's ±1 is never 0).
    let cf = cmp();
    let names = ["a".to_owned(), "b".to_owned()];
    let body = decompile(method(&cf, "same"), &cf, &names).expect("same decompiles");
    assert_eq!(
        body,
        ["if (a == b) {", "    return \"eq\";", "}", "return \"ne\";"]
    );
}

#[test]
fn recovers_float_inequality() {
    // `fcmpl; ifeq` — the fall-through reads back as `!=`.
    let cf = cmp();
    let names = ["a".to_owned(), "b".to_owned()];
    let body = decompile(method(&cf, "differ"), &cf, &names).expect("differ decompiles");
    assert_eq!(
        body,
        ["if (a != b) {", "    return \"ne\";", "}", "return \"eq\";"]
    );
}

#[test]
fn recovers_a_double_comparison_in_a_while() {
    // The loop header's `dcmpl; ifle` exit reads back as the `while (d > 1d)` condition.
    let cf = cmp();
    let body = decompile(method(&cf, "halve"), &cf, &["d".to_owned()]).expect("halve decompiles");
    assert_eq!(
        body,
        ["while (d > 1d) {", "    d = d / 2d;", "}", "return d;"]
    );
}

#[test]
fn recovers_a_float_comparison_in_a_do_while() {
    // The latch's `fcmpg; iflt` back-edge is the taken side: `while (f < 100f)`.
    let cf = cmp();
    let body = decompile(method(&cf, "grow"), &cf, &["f".to_owned()]).expect("grow decompiles");
    assert_eq!(
        body,
        [
            "do {",
            "    f = f * 2f;",
            "} while (f < 100f);",
            "return f;"
        ]
    );
}

#[test]
fn nan_inexact_flavor_bails() {
    // `if (!(f < g))` compiles to `fcmpg; iflt`, whose fall-through is true on NaN — no single
    // comparison operator renders it exactly, so the NaN guard bails the method.
    let cf = cmp();
    let names = ["f".to_owned(), "g".to_owned()];
    assert!(decompile(method(&cf, "pickGuard"), &cf, &names).is_none());
}

#[test]
fn cmp_feeding_a_ternary_still_bails() {
    // `a < b ? a : b` merges its value at the join with a leftover stack — not yet modelled.
    let cf = cmp();
    let names = ["a".to_owned(), "b".to_owned()];
    assert!(decompile(method(&cf, "least"), &cf, &names).is_none());
}
