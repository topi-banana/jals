//! The wasm assembler's acceptance tests: a module it built is validated by `wasm-tools`.
//!
//! Nothing here goes through a Java parser — bodies are emitted directly against [`Insn`] and the
//! types are declared directly against [`Module`]. That is deliberate, and it is the same split
//! `asm.rs` makes on the JVM side: it isolates the encoding — LEB128 lengths, the single recursive
//! type group, declared subtyping, the code section's local runs — from the lowering that will
//! later feed it.
//!
//! These tests used to live in `src/wasm/mod.rs`, in the *parent* of the two private modules they
//! reach into. Their placement was the symptom; the seam is the fix.

use std::io::Write as _;
use std::process::{Command, Stdio};

use jals_javac::wasm::{
    CompType, ExportKind, FieldType, Func, Global, HeapType, Insn, Instr, Module, NumOp, Numeric,
    RefType, StorageType, SubType, ValType,
};

/// Whether a tool that understands WebAssembly 3.0 is on this host. Like the JVM-backed tests, a
/// missing one stands the test down — loudly, because it is the only authority on whether an
/// emitted module is well-formed and a quiet stand-down reads as a pass.
fn tool(name: &str) -> bool {
    let present = Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !present {
        eprintln!("note: `{name}` is not installed; this test is checking less than it looks like");
    }
    present
}

/// Hand `bytes` to `wasm-tools validate`, which is the specification's own answer to whether a
/// module is well-formed — the wasm counterpart of letting a real JVM verify a class file.
fn validate(bytes: &[u8]) {
    if !tool("wasm-tools") {
        return;
    }
    let mut child = Command::new("wasm-tools")
        .arg("validate")
        .arg("-")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wasm-tools");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(bytes)
        .expect("write module");
    let output = child.wait_with_output().expect("wasm-tools");
    assert!(
        output.status.success(),
        "wasm-tools rejected the module:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A module whose types form a two-level hierarchy, plus a function that allocates the subtype and
/// reads a field *through the supertype's* accessor — which only validates because the subtyping is
/// declared.
fn hierarchy_module() -> Module {
    let mut module = Module::new();
    let mutable_i32 = FieldType {
        storage: StorageType::Val(ValType::I32),
        mutable: true,
    };
    let mutable_i64 = FieldType {
        storage: StorageType::Val(ValType::I64),
        mutable: true,
    };

    let base = module.add_type(SubType {
        is_final: false,
        supertype: None,
        comp: CompType::Struct(vec![mutable_i32]),
    });
    let sub = module.add_type(SubType {
        is_final: false,
        supertype: Some(base),
        // A subtype's fields start with the supertype's, in order: that prefix is what makes a
        // `(ref $Sub)` readable as a `(ref $Base)` without a conversion.
        comp: CompType::Struct(vec![mutable_i32, mutable_i64]),
    });
    let signature = module.add_type(SubType::plain(CompType::Func {
        params: vec![ValType::I32],
        results: vec![ValType::I32],
    }));

    let mut body = Insn::new();
    // Allocate the subtype, write the parameter plus one into its first field, then read that
    // field back *through the supertype's* accessor — which only validates because the subtyping
    // is declared.
    body.struct_new_default(sub)
        .local_tee(1)
        .local_get(0)
        .i32_const(1)
        .numeric(NumOp::Add, ValType::I32)
        .expect("i32.add")
        .struct_set(sub, 0)
        .local_get(1)
        .struct_get(base, 0);
    module.funcs.push(Func {
        type_index: signature,
        locals: vec![ValType::Ref(RefType::nullable(HeapType::Concrete(sub)))],
        body: body.into_body(),
    });
    let index = Module::func_index(0);
    module
        .exports
        .push(("bump".to_owned(), ExportKind::Func, index));
    module
}

#[test]
fn a_hand_encoded_gc_module_validates() {
    validate(
        &hierarchy_module()
            .finish()
            .expect("a module whose lengths all fit"),
    );
}

/// The recorded stream is the assertion surface this backend did not have: before, the only thing a
/// built body could be asked was its byte length.
#[test]
fn a_built_body_can_be_read_back_as_instructions() {
    let module = hierarchy_module();
    let body = &module.funcs[0].body;
    assert_eq!(
        body,
        &[
            Instr::StructNewDefault(1),
            Instr::LocalTee(1),
            Instr::LocalGet(0),
            Instr::I32Const(1),
            Instr::Numeric(NumOp::Add, ValType::I32),
            Instr::StructSet(1, 0),
            Instr::LocalGet(1),
            Instr::StructGet(0, 0),
        ]
    );
}

/// Encoding does not consume the module: `finish` takes `&self`, so the same module answers twice
/// and answers identically. A caller that inspects a module and then ships it is reading the bytes
/// it shipped.
#[test]
fn encoding_a_module_leaves_it_askable() {
    let module = hierarchy_module();
    let first = module.finish().expect("a module whose lengths all fit");
    let second = module.finish().expect("a module whose lengths all fit");
    assert_eq!(first, second);
    assert_eq!(module.funcs[0].body.len(), 8);
}

/// Structured control is counted as it is opened, because a `br` names a *relative* depth: nothing
/// in Java source says how many blocks sit between a `continue` and its loop.
#[test]
fn opening_and_closing_structures_tracks_the_branch_depth() {
    let mut body = Insn::new();
    assert_eq!(body.depth(), 0);
    body.block();
    body.loop_();
    assert_eq!(body.depth(), 2, "a `while` is two nested labels");
    body.if_();
    assert_eq!(body.depth(), 3);
    body.end();
    assert_eq!(body.depth(), 2, "the `if` closed, so a `br 0` is the loop");
    body.end();
    body.end();
    assert_eq!(body.depth(), 0);
    // A stray `end` saturates rather than wrapping to `u32::MAX`, which would make every following
    // branch name a depth no engine could resolve.
    body.end();
    assert_eq!(body.depth(), 0);
}

/// `%` on a float is `f32.rem` in Java and has no wasm instruction at all. The pair is refused
/// where it would be *recorded*, so an unrepresentable operation never reaches the stream — the
/// wasm counterpart of the JVM assembler rejecting a branch whose operand is not an `int`.
#[test]
fn an_operation_with_no_instruction_is_refused_rather_than_recorded() {
    let mut body = Insn::new();
    assert!(
        body.numeric(NumOp::Rem, ValType::F32).is_none(),
        "wasm has no `f32.rem`"
    );
    assert!(
        body.numeric(NumOp::Shl, ValType::F64).is_none(),
        "the shift family exists only over the integer types"
    );
    assert!(body.numeric(NumOp::Add, ValType::F32).is_some());
    assert_eq!(
        body.code(),
        [Instr::Numeric(NumOp::Add, ValType::F32)],
        "a refused pair leaves nothing behind"
    );
}

/// wasm has no integer negation, so `neg` refuses one rather than inventing an opcode; the lowering
/// emits `0 - x` instead.
#[test]
fn integer_negation_is_refused_because_wasm_has_none() {
    let mut body = Insn::new();
    assert!(body.neg(ValType::I32).is_none());
    assert!(body.neg(ValType::I64).is_none());
    assert!(body.neg(ValType::F32).is_some());
    assert!(body.neg(ValType::F64).is_some());
    assert_eq!(body.code(), [Instr::F32Neg, Instr::F64Neg]);
}

/// JLS §5.1.3's narrowing is two steps, and the *first* one is the saturating truncation rather
/// than the trapping one: `(int) (0.0 / 0.0)` is 0 in Java and a crash with `i32.trunc_f32_s`.
///
/// Asserted here rather than only by running a module, because the difference between the two
/// opcodes is invisible in any program that never divides by zero.
#[test]
fn a_narrowing_cast_records_both_of_its_steps() {
    let mut body = Insn::new();
    body.convert(Numeric::Float, Numeric::Byte)
        .expect("float to byte");
    assert_eq!(
        body.code(),
        [Instr::I32TruncSatF32S, Instr::I32Extend8S],
        "the truncation saturates, then the result is sign-extended to `byte`"
    );

    // `char` is the one unsigned integral type, so its second step masks where the others extend.
    let mut to_char = Insn::new();
    to_char
        .convert(Numeric::Int, Numeric::Char)
        .expect("int to char");
    assert_eq!(
        to_char.code(),
        [
            Instr::I32Const(0xFFFF),
            Instr::Numeric(NumOp::And, ValType::I32)
        ],
        "no `extend16_u` exists, so the mask is the conversion"
    );

    // A target whose range already holds the source needs no second step at all.
    let mut widening = Insn::new();
    widening
        .convert(Numeric::Byte, Numeric::Short)
        .expect("byte to short");
    assert_eq!(widening.code(), [], "a `byte` is already a `short`");
}

/// A conversion between two Java types that share a wasm value type emits nothing, which is what
/// keeps `int` to `int` from costing an instruction.
#[test]
fn a_conversion_within_one_value_type_emits_nothing() {
    let mut body = Insn::new();
    body.convert(Numeric::Int, Numeric::Int)
        .expect("int to int");
    assert_eq!(body.code(), []);
}

// --- the rest of the vocabulary ----------------------------------------------------------------
//
// A seam nothing drives is not a test surface. Each module below is hand-built against the
// instructions the lowering happens to reach for less often, and each is handed to `wasm-tools` —
// so the encoder is checked against the specification for the whole vocabulary rather than for the
// corner of it one Java program touches.

/// A function type taking `params` and returning `results`.
fn signature(module: &mut Module, params: Vec<ValType>, results: Vec<ValType>) -> u32 {
    module.add_type(SubType::plain(CompType::Func { params, results }))
}

/// Export the `defined`-th function under `name`.
fn export(module: &mut Module, name: &str, defined: usize) {
    let index = Module::func_index(defined);
    module
        .exports
        .push((name.to_owned(), ExportKind::Func, index));
}

/// `sum(n)` — the counting loop, which is what a `while` lowers to: a `block` to `break` out of
/// around a `loop` to `continue` back to.
fn loop_module() -> Module {
    let mut module = Module::new();
    let ty = signature(&mut module, vec![ValType::I32], vec![ValType::I32]);

    let mut body = Insn::new();
    body.i32_const(0).local_set(1).i32_const(0).local_set(2);
    body.block().loop_();
    // `i < n`, negated, so the branch is the exit rather than the continuation.
    body.local_get(1)
        .local_get(0)
        .numeric(NumOp::Lt, ValType::I32)
        .expect("i32.lt_s")
        .i32_eqz()
        .br_if(1);
    body.local_get(2)
        .local_get(1)
        .numeric(NumOp::Add, ValType::I32)
        .expect("i32.add")
        .local_set(2);
    body.local_get(1)
        .i32_const(1)
        .numeric(NumOp::Add, ValType::I32)
        .expect("i32.add")
        .local_set(1);
    body.br(0).end().end();
    body.local_get(2).return_();

    module.funcs.push(Func {
        type_index: ty,
        locals: vec![ValType::I32, ValType::I32],
        body: body.into_body(),
    });
    export(&mut module, "sum", 0);
    module
}

#[test]
fn a_hand_built_loop_validates() {
    validate(
        &loop_module()
            .finish()
            .expect("a module whose lengths all fit"),
    );
}

/// Arrays are a declared type of their own, and `length` is `array.len` rather than a field —
/// `array.new_fixed` writes its elements where `array.new_default` leaves them at zero.
fn array_module() -> Module {
    let mut module = Module::new();
    let element = FieldType {
        storage: StorageType::Val(ValType::I32),
        mutable: true,
    };
    let array = module.add_type(SubType::plain(CompType::Array(element)));
    let ty = signature(&mut module, Vec::new(), vec![ValType::I32]);

    let mut body = Insn::new();
    body.i32_const(3).array_new_default(array).local_set(0);
    body.local_get(0).i32_const(1).i32_const(5).array_set(array);
    // Built from the stack rather than defaulted, then thrown away: the point is that the
    // instruction encodes its element count.
    body.i32_const(7)
        .i32_const(8)
        .array_new_fixed(array, 2)
        .drop();
    body.local_get(0).i32_const(1).array_get(array);
    body.local_get(0).array_len();
    body.numeric(NumOp::Add, ValType::I32).expect("i32.add");

    module.funcs.push(Func {
        type_index: ty,
        locals: vec![ValType::Ref(RefType::nullable(HeapType::Concrete(array)))],
        body: body.into_body(),
    });
    export(&mut module, "arrays", 0);
    module
}

#[test]
fn a_hand_built_array_module_validates() {
    validate(
        &array_module()
            .finish()
            .expect("a module whose lengths all fit"),
    );
}

/// The reference instructions, and the one place a *non-nullable* reference type is written out:
/// `struct.new_default` produces one, so a function may declare it as its result.
fn reference_module() -> Module {
    let mut module = Module::new();
    let base = module.add_type(SubType {
        is_final: false,
        supertype: None,
        comp: CompType::Struct(vec![FieldType {
            storage: StorageType::Val(ValType::I32),
            mutable: true,
        }]),
    });
    let non_null = ValType::Ref(RefType {
        nullable: false,
        heap: HeapType::Concrete(base),
    });
    let make = signature(&mut module, Vec::new(), vec![non_null]);
    let ask = signature(&mut module, Vec::new(), vec![ValType::I32]);

    let mut maker = Insn::new();
    maker.struct_new_default(base);
    module.funcs.push(Func {
        type_index: make,
        locals: Vec::new(),
        body: maker.into_body(),
    });

    let mut body = Insn::new();
    // `ref.null none` inhabits every nullable reference type, which is what lets Java's untyped
    // `null` be stored without knowing the target type first.
    body.ref_null(HeapType::None).local_set(0);
    body.call(Module::func_index(0)).local_set(1);
    body.local_get(0).ref_is_null();
    body.local_get(1).local_get(1).ref_eq();
    body.numeric(NumOp::Add, ValType::I32).expect("i32.add");
    body.local_get(1)
        .ref_test(HeapType::Concrete(base), true)
        .numeric(NumOp::Add, ValType::I32)
        .expect("i32.add");
    // The nullable cast keeps a nullable reference, so the null check is separate.
    body.local_get(1)
        .ref_cast(HeapType::Concrete(base), true)
        .ref_as_non_null()
        .struct_get(base, 0)
        .numeric(NumOp::Add, ValType::I32)
        .expect("i32.add");
    // The non-nullable cast does both at once.
    body.local_get(1)
        .ref_cast(HeapType::Concrete(base), false)
        .struct_get(base, 0)
        .numeric(NumOp::Add, ValType::I32)
        .expect("i32.add");

    let nullable_base = ValType::Ref(RefType::nullable(HeapType::Concrete(base)));
    module.funcs.push(Func {
        type_index: ask,
        locals: vec![nullable_base, nullable_base],
        body: body.into_body(),
    });
    export(&mut module, "references", 1);
    module
}

#[test]
fn a_hand_built_reference_module_validates() {
    validate(
        &reference_module()
            .finish()
            .expect("a module whose lengths all fit"),
    );
}

/// A global's initialiser is a *constant expression*, so anything computed goes in the start
/// function instead — which is exactly the split a Java `static` field and its `<clinit>` make.
fn global_module() -> Module {
    let mut module = Module::new();
    let nothing = signature(&mut module, Vec::new(), Vec::new());
    let gives_i32 = signature(&mut module, Vec::new(), vec![ValType::I32]);

    let mut initialiser = Insn::new();
    initialiser.i32_const(41);
    module.globals.push(Global {
        ty: ValType::I32,
        init: initialiser.into_body(),
    });

    let mut start = Insn::new();
    start
        .global_get(0)
        .i32_const(1)
        .numeric(NumOp::Add, ValType::I32)
        .expect("i32.add")
        .global_set(0);
    module.funcs.push(Func {
        type_index: nothing,
        locals: Vec::new(),
        body: start.into_body(),
    });

    let mut get = Insn::new();
    get.global_get(0);
    module.funcs.push(Func {
        type_index: gives_i32,
        locals: Vec::new(),
        body: get.into_body(),
    });

    // The wide constants have no home in the two functions above, and dropping each is the
    // smallest well-typed thing to do with one.
    let mut wide = Insn::new();
    wide.i64_const(1 << 40).drop();
    wide.f32_const(1.5).drop();
    wide.f64_const(2.5).drop();
    wide.i32_const(0);
    module.funcs.push(Func {
        type_index: gives_i32,
        locals: Vec::new(),
        body: wide.into_body(),
    });

    module.start = Some(Module::func_index(0));
    export(&mut module, "get", 1);
    export(&mut module, "wide", 2);
    module
}

#[test]
fn a_hand_built_module_with_globals_and_a_start_function_validates() {
    validate(
        &global_module()
            .finish()
            .expect("a module whose lengths all fit"),
    );
}

/// `throw` and `try_table` are how a Java `throw` / `catch` pair lands on a host with an exception
/// model of its own. A catch's label is resolved *outside* the `try_table`, which is why the block
/// it targets is opened first.
fn exception_module() -> Module {
    let mut module = Module::new();
    // One tag is enough for Java: every thrown value is a reference, so the payload type is the
    // same for all of them and the *class* of the reference is what a `catch` tests. This module
    // carries no payload at all, which is the smallest shape that still exercises both halves.
    let tag_type = signature(&mut module, Vec::new(), Vec::new());
    let gives_i32 = signature(&mut module, Vec::new(), vec![ValType::I32]);
    module.tags.push(tag_type);

    let mut thrower = Insn::new();
    thrower.throw(0);
    module.funcs.push(Func {
        type_index: tag_type,
        locals: Vec::new(),
        body: thrower.into_body(),
    });

    let mut catcher = Insn::new();
    catcher.block();
    catcher.try_table(&[(0, 0)]);
    catcher.call(Module::func_index(0));
    // The call always throws, so the fallthrough is a path the program cannot reach.
    catcher.unreachable();
    catcher.end();
    catcher.i32_const(1).return_();
    catcher.end();
    catcher.i32_const(2);
    module.funcs.push(Func {
        type_index: gives_i32,
        locals: Vec::new(),
        body: catcher.into_body(),
    });

    export(&mut module, "caught", 1);
    module
}

#[test]
fn a_hand_built_module_with_a_tag_and_a_handler_validates() {
    validate(
        &exception_module()
            .finish()
            .expect("a module whose lengths all fit"),
    );
}

/// A `block` and an `if` that leave a value, which is how a `switch` *expression* and a `?:` are
/// spelled — the statement forms leave nothing and need no type.
fn typed_control_module() -> Module {
    let mut module = Module::new();
    let ty = signature(&mut module, vec![ValType::I32], vec![ValType::I32]);

    let mut body = Insn::new();
    body.block_typed(ValType::I32);
    body.local_get(0);
    body.if_typed(ValType::I32);
    body.i32_const(10);
    body.else_();
    body.i32_const(20);
    body.end();
    // A dense `switch`: one subtraction is the whole bounds check, because the index is read as
    // unsigned and a key below the lowest wraps past 2³¹ onto the default.
    body.local_get(0)
        .i32_const(1)
        .numeric(NumOp::Sub, ValType::I32)
        .expect("i32.sub");
    body.br_table(&[0, 0], 0);
    body.end();

    module.funcs.push(Func {
        type_index: ty,
        locals: Vec::new(),
        body: body.into_body(),
    });
    export(&mut module, "typed", 0);
    module
}

#[test]
fn a_hand_built_module_with_typed_blocks_and_a_branch_table_validates() {
    validate(
        &typed_control_module()
            .finish()
            .expect("a module whose lengths all fit"),
    );
}
