//! Method-body decompilation (`body::decompile`) over real classes compiled with
//! `-parameters -g`, so reconstruction is checked against actual javac bytecode.

use jals_classfile::{ClassAccessFlags, ClassFile, MethodAccessFlags, MethodInfo};
use jals_decompile::ClassHierarchy;
use jals_decompile::body;
use jals_exec::block_on_inline;

fn fixture(bytes: &[u8]) -> ClassFile {
    block_on_inline(ClassFile::read(bytes)).expect("parse fixture class")
}

/// Synchronous test-side driver for the async [`body::decompile`].
fn decompile(method: &MethodInfo, cf: &ClassFile, param_names: &[String]) -> Option<Vec<String>> {
    let hierarchy = ClassHierarchy::new(core::slice::from_ref(cf));
    block_on_inline(body::decompile(method, cf, param_names, &hierarchy))
}

fn decompile_with_hierarchy(
    method: &MethodInfo,
    cf: &ClassFile,
    param_names: &[String],
    classes: &[ClassFile],
) -> Option<Vec<String>> {
    let hierarchy = ClassHierarchy::new(classes);
    block_on_inline(body::decompile(method, cf, param_names, &hierarchy))
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

fn fors() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Fors.class"
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

fn switches() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Switches.class"
    ))
}

fn switches_color() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Switches$Color.class"
    ))
}

fn fake_ordinal() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/FakeOrdinal.class"
    ))
}

fn tries() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/Tries.class"
    ))
}

fn int_carried() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/IntCarried.class"
    ))
}

fn invoke_special_calls() -> ClassFile {
    fixture(include_bytes!(
        "../../jals-classpath/tests/fixtures/InvokeSpecialCalls.class"
    ))
}

fn invoke_special_classes() -> [ClassFile; 3] {
    [
        invoke_special_calls(),
        fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/InvokeSpecialBase.class"
        )),
        fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/InvokeSpecialDefault.class"
        )),
    ]
}

fn hierarchy_evolution_v1() -> [ClassFile; 6] {
    [
        fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/hierarchy-evolution/v1/evolution/HierarchyEvolution.class"
        )),
        fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/hierarchy-evolution/v1/evolution/HierarchyBase.class"
        )),
        fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/hierarchy-evolution/v1/evolution/HierarchyDirect.class"
        )),
        fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/hierarchy-evolution/v1/evolution/HierarchyRoot.class"
        )),
        fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/hierarchy-evolution/v1/evolution/HierarchyLeft.class"
        )),
        fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/hierarchy-evolution/v1/evolution/HierarchyRight.class"
        )),
    ]
}

fn hierarchy_evolution_mixed() -> [ClassFile; 6] {
    let [client, _, direct, root, left, _] = hierarchy_evolution_v1();
    [
        client,
        fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/hierarchy-evolution/v2/evolution/HierarchyBase.class"
        )),
        direct,
        root,
        left,
        fixture(include_bytes!(
            "../../jals-classpath/tests/fixtures/hierarchy-evolution/v2/evolution/HierarchyRight.class"
        )),
    ]
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
fn decompiles_explicit_super_constructor_call() {
    let cf = invoke_special_calls();
    let body = decompile(method(&cf, "<init>"), &cf, &["seed".to_owned()])
        .expect("constructor decompiles");
    assert_eq!(body, ["super(seed);"]);
}

#[test]
fn preserves_superclass_invokespecial_dispatch() {
    let cf = invoke_special_calls();
    let body = decompile(method(&cf, "callSuperclass"), &cf, &["value".to_owned()])
        .expect("superclass call decompiles");
    assert_eq!(body, ["return super.classValue(value);"]);
}

#[test]
fn preserves_interface_default_invokespecial_dispatch() {
    let classes = invoke_special_classes();
    let cf = &classes[0];
    let body = decompile_with_hierarchy(
        method(cf, "callInterface"),
        cf,
        &["value".to_owned()],
        &classes,
    )
    .expect("interface default call decompiles");
    assert_eq!(
        body,
        ["return demo.InvokeSpecialDefault.super.interfaceValue(value);"]
    );
}

#[test]
fn preserves_diamond_interface_super_with_one_shared_declaration() {
    let classes = hierarchy_evolution_v1();
    let cf = &classes[0];
    let body =
        decompile_with_hierarchy(method(cf, "callLeft"), cf, &["value".to_owned()], &classes)
            .expect("shared ancestor default decompiles");
    assert_eq!(
        body,
        ["return evolution.HierarchyLeft.super.rootValue(value);"]
    );
}

#[test]
fn evolved_interface_super_hierarchy_bails() {
    let classes = hierarchy_evolution_mixed();
    let cf = &classes[0];
    for name in ["callDirect", "callLeft"] {
        assert!(
            decompile_with_hierarchy(method(cf, name), cf, &["value".to_owned()], &classes,)
                .is_none(),
            "{name} must fall back"
        );
    }
}

#[test]
fn incomplete_or_ambiguous_interface_hierarchy_bails() {
    let cf = invoke_special_calls();
    assert!(decompile(method(&cf, "callInterface"), &cf, &["value".to_owned()]).is_none());

    let mut classes = invoke_special_classes().to_vec();
    classes.push(classes[2].clone());
    let cf = &classes[0];
    assert!(
        decompile_with_hierarchy(
            method(cf, "callInterface"),
            cf,
            &["value".to_owned()],
            &classes,
        )
        .is_none()
    );
}

#[test]
fn malformed_interface_hierarchy_or_target_bails() {
    let mut classes = invoke_special_classes();
    classes[2].access_flags.0 &= !ClassAccessFlags::INTERFACE;
    let cf = &classes[0];
    assert!(
        decompile_with_hierarchy(
            method(cf, "callInterface"),
            cf,
            &["value".to_owned()],
            &classes,
        )
        .is_none()
    );

    let mut classes = invoke_special_classes();
    let interface = &mut classes[2];
    interface.interfaces.push(interface.this_class);
    let cf = &classes[0];
    assert!(
        decompile_with_hierarchy(
            method(cf, "callInterface"),
            cf,
            &["value".to_owned()],
            &classes,
        )
        .is_none()
    );

    for flag in [MethodAccessFlags::ABSTRACT, MethodAccessFlags::STATIC] {
        let mut classes = invoke_special_classes();
        let interface = &mut classes[2];
        let target = interface
            .methods
            .iter_mut()
            .find(|method| {
                interface.constant_pool.utf8(method.name_index).as_deref() == Some("interfaceValue")
            })
            .expect("default method");
        target.access_flags.0 |= flag;
        let cf = &classes[0];
        assert!(
            decompile_with_hierarchy(
                method(cf, "callInterface"),
                cf,
                &["value".to_owned()],
                &classes,
            )
            .is_none()
        );
    }
}

#[test]
fn non_direct_invokespecial_targets_bail() {
    let mut cf = invoke_special_calls();
    cf.super_class = 0;
    assert!(decompile(method(&cf, "callSuperclass"), &cf, &["value".to_owned()]).is_none());

    let mut cf = invoke_special_calls();
    cf.interfaces.clear();
    assert!(decompile(method(&cf, "callInterface"), &cf, &["value".to_owned()]).is_none());
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
fn decompiles_array_class_literals() {
    let cf = arrays();
    for (name, expected) in [
        ("primitiveArrayClass", "return int[].class;"),
        ("referenceArrayClass", "return java.lang.String[].class;"),
        (
            "multidimensionalArrayClass",
            "return java.lang.String[][].class;",
        ),
    ] {
        let body = decompile(method(&cf, name), &cf, &[]).expect("class literal decompiles");
        assert_eq!(body, [expected], "{name}");
    }
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

#[test]
fn structures_a_dense_switch_with_breaks_and_no_default() {
    let cf = switches();
    let body = decompile(method(&cf, "dense"), &cf, &["x".to_owned()]).expect("dense decompiles");
    // The last arm needs no `break;` — falling out of the switch is the same thing — and javac
    // aims the default offset at the fall-out, so the join comes from the arms' break edges.
    assert_eq!(
        body,
        [
            "switch (x) {",
            "    case 0:",
            "        this.value = 10;",
            "        break;",
            "    case 1:",
            "        this.value = 11;",
            "        break;",
            "    case 2:",
            "        this.value = 12;",
            "}",
            "this.value = this.value + 1;",
        ]
    );
}

#[test]
fn structures_a_sparse_lookupswitch_with_a_default() {
    let cf = switches();
    let body = decompile(method(&cf, "sparse"), &cf, &["x".to_owned()]).expect("sparse decompiles");
    assert_eq!(
        body,
        [
            "int r;",
            "switch (x) {",
            "    case 1:",
            "        r = 100;",
            "        break;",
            "    case 100:",
            "        r = 1;",
            "        break;",
            "    default:",
            "        r = -1;",
            "}",
            "return r;",
        ]
    );
}

#[test]
fn keys_sharing_an_arm_become_stacked_labels() {
    let cf = switches();
    let body =
        decompile(method(&cf, "stacked"), &cf, &["x".to_owned()]).expect("stacked decompiles");
    // `case 1:` and `case 2:` share one body, and the `tableswitch` gap key 3 — which only
    // `default` covers — never becomes a label. The trailing `default: return 0;` is bytecode-
    // identical to the same `return` sitting after the switch, so it reads back as the latter.
    assert_eq!(
        body,
        [
            "switch (x) {",
            "    case 1:",
            "    case 2:",
            "        return 12;",
            "    case 4:",
            "        return 4;",
            "}",
            "return 0;",
        ]
    );
}

#[test]
fn an_arm_running_into_the_next_stays_a_fall_through() {
    let cf = switches();
    let body = decompile(method(&cf, "fallThrough"), &cf, &["x".to_owned()])
        .expect("fallThrough decompiles");
    // `case 1` gets no `break;` — it really does run on into `case 2`.
    assert_eq!(
        body,
        [
            "int r;",
            "r = 0;",
            "switch (x) {",
            "    case 1:",
            "        r = r + 1;",
            "    case 2:",
            "        r = r + 2;",
            "        break;",
            "    case 3:",
            "        r = r + 3;",
            "}",
            "return r;",
        ]
    );
}

#[test]
fn structures_a_switch_whose_arms_all_return() {
    let cf = switches();
    let body =
        decompile(method(&cf, "allReturn"), &cf, &["x".to_owned()]).expect("allReturn decompiles");
    // No arm can reach the join, so none gets a `break;`, and the join is named by the default
    // offset rather than by any edge out of an arm.
    assert_eq!(
        body,
        [
            "switch (x) {",
            "    case 0:",
            "        return 10;",
            "    case 1:",
            "        return 11;",
            "}",
            "return -1;",
        ]
    );
}

#[test]
fn keeps_a_default_written_between_two_cases_in_place() {
    let cf = switches();
    let body = decompile(method(&cf, "defaultMiddle"), &cf, &["x".to_owned()])
        .expect("defaultMiddle decompiles");
    assert_eq!(
        body,
        [
            "switch (x) {",
            "    case 1:",
            "        return 1;",
            "    default:",
            "        return 0;",
            "    case 5:",
            "        return 5;",
            "}",
        ]
    );
}

#[test]
fn structures_a_plain_if_inside_an_arm() {
    let cf = switches();
    let body = decompile(
        method(&cf, "ifInArm"),
        &cf,
        &["x".to_owned(), "y".to_owned()],
    )
    .expect("ifInArm decompiles");
    // The `if` is the tail of its arm, so its skip edge lands on the switch's join rather than
    // inside the arm.
    assert_eq!(
        body,
        [
            "switch (x) {",
            "    case 1:",
            "        if (y) {",
            "            this.value = 1;",
            "        }",
            "        break;",
            "    case 2:",
            "        this.value = 2;",
            "}",
            "this.value = this.value + 1;",
        ]
    );
}

#[test]
fn structures_an_if_else_inside_an_arm_as_a_guard_and_a_tail() {
    let cf = switches();
    let body = decompile(
        method(&cf, "ifElseInArm"),
        &cf,
        &["x".to_owned(), "y".to_owned()],
    )
    .expect("ifElseInArm decompiles");
    // javac rewrites the then-branch's exit `goto` straight to the switch join, past the arm, so
    // the recovered shape is a guard that breaks rather than a symmetric if/else. Wordier than the
    // source, but exactly what the bytecode does.
    assert_eq!(
        body,
        [
            "switch (x) {",
            "    case 1:",
            "        if (y) {",
            "            this.value = 1;",
            "            break;",
            "        }",
            "        this.value = 2;",
            "        break;",
            "    case 2:",
            "        this.value = 3;",
            "}",
            "this.value = this.value + 1;",
        ]
    );
}

#[test]
fn an_arm_that_breaks_from_an_if_but_returns_at_its_tail_gets_no_trailing_break() {
    let cf = switches();
    let body = decompile(
        method(&cf, "breakThenReturnInArm"),
        &cf,
        &["x".to_owned(), "y".to_owned()],
    )
    .expect("breakThenReturnInArm decompiles");
    // The arm reaches the join through the inner `if`, but its *tail* returns. A `break;` after
    // that `return` would be an unreachable statement (JLS 14.21) and would not compile.
    assert_eq!(
        body,
        [
            "int r;",
            "r = 0;",
            "switch (x) {",
            "    case 1:",
            "        if (y) {",
            "            r = 1;",
            "            break;",
            "        }",
            "        return -1;",
            "    case 2:",
            "        r = 2;",
            "}",
            "return r;",
        ]
    );
}

#[test]
fn a_char_switch_recovers_character_labels() {
    let cf = switches();
    let body = decompile(method(&cf, "vowel"), &cf, &["c".to_owned()]).expect("vowel decompiles");
    assert_eq!(
        body,
        [
            "switch (c) {",
            "    case 'a':",
            "    case 'e':",
            "        return 1;",
            "}",
            "return 0;",
        ]
    );
}

#[test]
fn an_enum_switch_recovers_constant_labels() {
    let classes = [switches(), switches_color()];
    let cf = &classes[0];
    let body = decompile_with_hierarchy(method(cf, "onEnum"), cf, &["c".to_owned()], &classes)
        .expect("onEnum decompiles");
    // javac switches on `ordinal()`; the constant names come back out of the enum's `<clinit>`.
    assert_eq!(
        body,
        [
            "switch (c) {",
            "    case RED:",
            "        return 1;",
            "    case GREEN:",
            "        return 2;",
            "}",
            "return 0;",
        ]
    );
}

#[test]
fn an_enum_switch_bails_without_the_enum_class() {
    let cf = switches();
    // Without the enum in the index the ordinals cannot be named, and `switch (c.ordinal())` with
    // numeric labels is not what the source said — fall back instead.
    assert!(decompile(method(&cf, "onEnum"), &cf, &["c".to_owned()]).is_none());
}

#[test]
fn a_non_enum_ordinal_switch_keeps_the_plain_int_reading() {
    let cf = fake_ordinal();
    // `ordinal()` on a class that resolves and is not an enum is just a method — the switch is an
    // ordinary `int` one, not a lowering, so it must recover rather than decline.
    let body = decompile(method(&cf, "onFake"), &cf, &["f".to_owned()]).expect("onFake decompiles");
    assert_eq!(
        body,
        [
            "switch (f.ordinal()) {",
            "    case 1:",
            "        return 1;",
            "    case 2:",
            "        return 2;",
            "}",
            "return 0;",
        ]
    );
}

#[test]
fn a_string_switch_bails() {
    let cf = switches();
    // javac lowers it to a hashCode()/equals() pre-dispatch through two synthetic locals that the
    // `LocalVariableTable` does not name, so the method has no confident reading.
    assert!(decompile(method(&cf, "onString"), &cf, &["s".to_owned()]).is_none());
}

#[test]
fn recovers_a_for_that_declares_its_counter() {
    // Byte-identical to `Loops.sum`'s `while` — only the `LineNumberTable` says this one was a
    // `for`, by putting the update on the header's line. The counter dies with the loop, so the
    // header declares it and no `int i;` is hoisted.
    let cf = fors();
    let body = decompile(method(&cf, "sum"), &cf, &["n".to_owned()]).expect("sum decompiles");
    assert_eq!(
        body,
        [
            "int total;",
            "total = 0;",
            "for (int i = 0; i < n; i++) {",
            "    total = total + i;",
            "}",
            "return total;",
        ]
    );
}

#[test]
fn a_for_lifts_only_the_last_statement_of_its_latch() {
    // The latch block holds both body statements and the update; only the update may move.
    let cf = fors();
    let body = decompile(method(&cf, "twoStmtBody"), &cf, &["n".to_owned()])
        .expect("twoStmtBody decompiles");
    assert_eq!(
        body,
        [
            "int total;",
            "total = 0;",
            "for (int i = 0; i < n; i++) {",
            "    total = total + i;",
            "    total = total + 1;",
            "}",
            "return total;",
        ]
    );
}

#[test]
fn a_for_body_may_branch() {
    // The latch is reached from both sides of the `if`, so splitting it out of the region must
    // leave the `if` structuring untouched.
    let cf = fors();
    let body = decompile(method(&cf, "withIf"), &cf, &["n".to_owned()]).expect("withIf decompiles");
    assert_eq!(
        body,
        [
            "int total;",
            "total = 0;",
            "for (int i = 0; i < n; i++) {",
            "    if (i > 0) {",
            "        total = total + i;",
            "    }",
            "}",
            "return total;",
        ]
    );
}

#[test]
fn a_counter_that_outlives_its_loop_keeps_its_hoisted_declaration() {
    // The update folds, but the initializer sits on its own source line, so the header takes no
    // init clause and the declaration stays where it is.
    let cf = fors();
    let body =
        decompile(method(&cf, "outlives"), &cf, &["n".to_owned()]).expect("outlives decompiles");
    assert_eq!(
        body,
        ["int i;", "i = 0;", "for (; i < n; i++) {", "}", "return i;"]
    );
}

#[test]
fn a_while_whose_update_has_its_own_line_stays_a_while() {
    let cf = fors();
    let body =
        decompile(method(&cf, "whileLoop"), &cf, &["n".to_owned()]).expect("whileLoop decompiles");
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
fn a_one_line_while_folds_but_may_not_absorb_its_declaration() {
    // Written on one line, so every part of the loop shares the header's line and the line evidence
    // cannot tell it from a `for` — it renders as one, which is valid and means the same thing.
    // `i` is read after the loop, though, so only the `LocalVariableTable`'s live range keeps the
    // declaration out of the header; absorbing it would not compile.
    let cf = fors();
    let body = decompile(method(&cf, "inlineWhile"), &cf, &["n".to_owned()])
        .expect("inlineWhile decompiles");
    assert_eq!(
        body,
        [
            "int total;",
            "int i;",
            "total = 0;",
            "for (i = 0; i < n; i++) {",
            "    total = total + i;",
            "}",
            "return total + i;",
        ]
    );
}

#[test]
fn two_loops_sharing_a_slot_both_keep_the_shared_declaration() {
    // One hoisted `int i;` serves both loops, so neither may absorb it — the slot's live ranges
    // cover both loops, which fails the confinement check on each. Absorbing in one would leave the
    // other referencing an undeclared name.
    let cf = fors();
    let body = decompile(method(&cf, "twice"), &cf, &["n".to_owned()]).expect("twice decompiles");
    assert_eq!(
        body,
        [
            "int total;",
            "int i;",
            "total = 0;",
            "for (i = 0; i < n; i++) {",
            "    total = total + 1;",
            "}",
            "for (i = 0; i < n; i++) {",
            "    total = total + 1;",
            "}",
            "return total;",
        ]
    );
}

#[test]
fn a_parameter_used_as_a_counter_is_never_re_declared() {
    // Declaring the counter in the header would shadow the signature's own `int n`. A parameter's
    // slot is live from offset 0 to the end of the method, so the confinement check can never
    // succeed for one — no separate guard is needed.
    let cf = fors();
    let body = decompile(method(&cf, "reuse"), &cf, &["n".to_owned()]).expect("reuse decompiles");
    assert_eq!(
        body,
        [
            "int total;",
            "total = 0;",
            "for (n = 0; n < 10; n++) {",
            "    total = total + n;",
            "}",
            "return total;",
        ]
    );
}

#[test]
fn a_header_with_two_updates_keeps_the_extra_one_in_the_body() {
    // `for (int i = 0, j = n; i < j; i++, j--)`. Only the latch's last statement can become the
    // update clause, so `i++` stays at the end of the body and only `j` moves into the header.
    // A different shape from the source, but the same meaning and still valid Java.
    let cf = fors();
    let body = decompile(method(&cf, "twoUpdates"), &cf, &["n".to_owned()])
        .expect("twoUpdates decompiles");
    assert_eq!(
        body,
        [
            "int total;",
            "int i;",
            "total = 0;",
            "i = 0;",
            "for (int j = n; i < j; j--) {",
            "    total = total + 1;",
            "    i = i + 1;",
            "}",
            "return total;",
        ]
    );
}

#[test]
fn structures_a_try_whose_body_and_handler_both_return() {
    // Neither part reaches a join, so no edge names one. Recovered only because the region itself
    // ends by exiting, which says nothing after the statement is reachable.
    let cf = tries();
    let body = decompile(method(&cf, "catchAndReturn"), &cf, &["s".to_owned()])
        .expect("catchAndReturn decompiles");
    assert_eq!(
        body,
        [
            "try {",
            "    return this.parse(s);",
            "} catch (java.io.IOException e) {",
            "    return -1;",
            "}",
        ]
    );
}

#[test]
fn structures_a_try_whose_handler_falls_into_the_join() {
    let cf = tries();
    let body = decompile(method(&cf, "catchAndFallThrough"), &cf, &["s".to_owned()])
        .expect("catchAndFallThrough decompiles");
    assert_eq!(
        body,
        [
            "try {",
            "    this.value = this.parse(s);",
            "} catch (java.io.IOException e) {",
            "    this.value = -1;",
            "}",
        ]
    );
}

#[test]
fn statements_after_a_try_follow_it() {
    let cf = tries();
    let body =
        decompile(method(&cf, "trailing"), &cf, &["s".to_owned()]).expect("trailing decompiles");
    assert_eq!(
        body,
        [
            "int r;",
            "r = 0;",
            "try {",
            "    r = this.parse(s);",
            "} catch (java.io.IOException e) {",
            "    r = -1;",
            "}",
            "return r + 1;",
        ]
    );
}

#[test]
fn sibling_catch_clauses_sharing_a_slot_each_get_their_own_type() {
    // `javac` gives both parameters slot 2, with one `LocalVariableTable` entry each. Resolving by
    // slot alone reports an ambiguous reuse; each clause is resolved by the offset its parameter is
    // born at instead.
    let cf = tries();
    let body = decompile(method(&cf, "twoCatches"), &cf, &["s".to_owned()])
        .expect("twoCatches decompiles");
    assert_eq!(
        body,
        [
            "try {",
            "    return this.parse(s);",
            "} catch (java.io.IOException e) {",
            "    return -1;",
            "} catch (java.lang.RuntimeException e) {",
            "    return -2;",
            "}",
        ]
    );
}

#[test]
fn a_multi_catch_recovers_its_alternatives_from_the_exception_table() {
    // The `LocalVariableTable` types this parameter as `RuntimeException` — the least upper bound
    // `javac` computed — so the alternatives can only come from the two exception-table rows.
    let cf = tries();
    let body = decompile(method(&cf, "multiCatch"), &cf, &["s".to_owned()])
        .expect("multiCatch decompiles");
    assert_eq!(
        body,
        [
            "try {",
            "    this.value = java.lang.Integer.parseInt(s);",
            "} catch (java.lang.NumberFormatException | java.lang.NullPointerException e) {",
            "    this.value = -1;",
            "}",
        ]
    );
}

#[test]
fn an_unused_catch_parameter_is_named_even_without_a_table_entry() {
    // An unused parameter gets no `LocalVariableTable` entry even under `-g`. Nothing loads the
    // slot, so a synthesised name is safe — the same fallback an unnamed method parameter takes.
    let cf = tries();
    let body = decompile(method(&cf, "emptyCatch"), &cf, &["s".to_owned()])
        .expect("emptyCatch decompiles");
    assert_eq!(
        body,
        [
            "try {",
            "    this.value = java.lang.Integer.parseInt(s);",
            "} catch (java.lang.RuntimeException e) {",
            "}",
        ]
    );
}

#[test]
fn a_nested_try_is_read_from_the_nesting_of_the_protected_ranges() {
    let cf = tries();
    let body =
        decompile(method(&cf, "nestedTry"), &cf, &["s".to_owned()]).expect("nestedTry decompiles");
    assert_eq!(
        body,
        [
            "try {",
            "    try {",
            "        this.value = java.lang.Integer.parseInt(s);",
            "    } catch (java.lang.NumberFormatException e) {",
            "        this.value = 1;",
            "    }",
            "} catch (java.lang.RuntimeException e) {",
            "    this.value = 2;",
            "}",
        ]
    );
}

#[test]
fn a_finally_folds_its_two_duplicates_into_one_clause() {
    let cf = tries();
    let body = decompile(method(&cf, "tryFinally"), &cf, &[]).expect("tryFinally decompiles");
    assert_eq!(
        body,
        [
            "try {",
            "    this.mayThrow();",
            "} finally {",
            "    this.value = 9;",
            "}",
        ]
    );
}

#[test]
fn a_finally_beside_a_catch_folds_three_duplicates() {
    // One copy per exit that completes normally — the try body's and the clause's — plus the
    // handler's own, which rethrows.
    let cf = tries();
    let body =
        decompile(method(&cf, "tryCatchFinally"), &cf, &[]).expect("tryCatchFinally decompiles");
    assert_eq!(
        body,
        [
            "try {",
            "    this.mayThrow();",
            "} catch (java.io.IOException e) {",
            "    this.value = -1;",
            "} finally {",
            "    this.value = 9;",
            "}",
        ]
    );
}

#[test]
fn a_clause_that_throws_gets_no_duplicate_of_its_own() {
    // The catch-all row covering the clause stops inside the handler's entry rather than before it,
    // which is how the table says that clause leaves abruptly.
    let cf = tries();
    let body = decompile(method(&cf, "tryCatchFinallyThrowing"), &cf, &[])
        .expect("tryCatchFinallyThrowing decompiles");
    assert_eq!(
        body,
        [
            "try {",
            "    this.mayThrow();",
            "} catch (java.io.IOException e) {",
            "    throw new java.lang.IllegalStateException(\"boom\");",
            "} finally {",
            "    this.value = 9;",
            "}",
        ]
    );
}

#[test]
fn a_multi_statement_finalizer_folds() {
    let cf = tries();
    let body =
        decompile(method(&cf, "tryFinallyBlock"), &cf, &[]).expect("tryFinallyBlock decompiles");
    assert_eq!(
        body,
        [
            "try {",
            "    this.mayThrow();",
            "} finally {",
            "    this.value = 9;",
            "    this.other = 10;",
            "}",
        ]
    );
}

#[test]
fn a_branching_finalizer_bails_because_its_copies_differ() {
    // What follows a copy differs by copy — the normal exit continues past the statement, the
    // handler rethrows — so the `if`'s arms jump to different places and the byte-level equality
    // the fold rests on does not hold.
    let cf = tries();
    assert!(decompile(method(&cf, "finallyWithBranch"), &cf, &["n".to_owned()]).is_none());
}

#[test]
fn a_return_inside_a_try_with_a_finally_bails() {
    // The return value is spilled to a slot the `LocalVariableTable` never names.
    let cf = tries();
    assert!(
        decompile(
            method(&cf, "returnInsideTryFinally"),
            &cf,
            &["n".to_owned()]
        )
        .is_none()
    );
}

#[test]
fn a_catch_parameter_sharing_a_slot_with_a_local_bails() {
    let cf = tries();
    assert!(decompile(method(&cf, "sharedCatchSlot"), &cf, &["n".to_owned()]).is_none());
}

#[test]
fn a_for_inside_a_try_bails_on_the_shared_counter_slot() {
    // The counter dies at the closing brace and the catch parameter is born right after it, so they
    // share a slot — the reuse M3 declines to split.
    let cf = tries();
    assert!(decompile(method(&cf, "loopInTry"), &cf, &["n".to_owned()]).is_none());
}

#[test]
fn a_synchronized_block_is_not_read_as_a_finally() {
    // It compiles to a catch-all handler that looks like one. The `monitorenter` / `monitorexit`
    // pair is what separates them, and the simulator models neither.
    let cf = tries();
    assert!(decompile(method(&cf, "synchronizedBlock"), &cf, &["lock".to_owned()]).is_none());
}

#[test]
fn a_finalizer_that_returns_bails() {
    let cf = tries();
    assert!(decompile(method(&cf, "finallyWithReturn"), &cf, &["n".to_owned()]).is_none());
}

#[test]
fn a_try_inside_a_finalizer_bails() {
    // Its handlers sit inside every copy, so folding the copies away would drop them.
    let cf = tries();
    assert!(decompile(method(&cf, "tryInsideFinally"), &cf, &[]).is_none());
}

#[test]
fn two_returns_inside_one_try_split_the_range_and_bail() {
    // Two rows share a handler but disagree on the range: a hole meaning "control left here" is not
    // the same as one meaning "this is not protected".
    let cf = tries();
    assert!(decompile(method(&cf, "twoReturnsInTry"), &cf, &["s".to_owned()]).is_none());
}

#[test]
fn try_with_resources_bails() {
    let cf = tries();
    assert!(decompile(method(&cf, "tryWithResources"), &cf, &["r".to_owned()]).is_none());
}

#[test]
fn a_loop_heading_a_protected_range_stays_inside_the_try() {
    // The loop header *is* the try's entry block, so the structurer must look for a `try` there
    // before it looks for a loop — the other order lets the loop swallow the statement. The counter
    // is a parameter, which is what keeps a fresh slot from competing with the catch parameter.
    let cf = tries();
    let body = decompile(method(&cf, "loopInTryBody"), &cf, &["n".to_owned()])
        .expect("loopInTryBody decompiles");
    assert_eq!(
        body,
        [
            "try {",
            "    while (n > 0) {",
            "        n = n - 1;",
            "    }",
            "} catch (java.lang.RuntimeException e) {",
            "    this.value = -1;",
            "}",
        ]
    );
}

#[test]
fn a_for_inside_a_try_drops_the_hoisted_declaration_it_absorbed() {
    // A finalizer has no catch parameter, so the counter keeps a slot of its own and its
    // declaration moves into the header (M9). The hoisted one must go with it: keeping both is a
    // duplicate declaration, which parses cleanly and only `javac` rejects.
    let cf = tries();
    let body = decompile(method(&cf, "loopInTryFinally"), &cf, &["n".to_owned()])
        .expect("loopInTryFinally decompiles");
    assert_eq!(
        body,
        [
            "int total;",
            "total = 0;",
            "try {",
            "    for (int i = 0; i < n; i++) {",
            "        total = total + i;",
            "    }",
            "} finally {",
            "    this.value = 9;",
            "}",
            "return total;",
        ]
    );
}

#[test]
fn an_empty_finally_leaves_no_handler_to_structure() {
    // `javac` drops the whole handler for a `finally { }`, so what reaches the decompiler is a
    // method with no exception table at all.
    let cf = tries();
    let body = decompile(method(&cf, "emptyFinally"), &cf, &[]).expect("emptyFinally decompiles");
    assert_eq!(body, ["this.mayThrow();"]);
}
