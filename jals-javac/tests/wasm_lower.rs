//! The wasm lowering's assertions: Java source in, the instructions it emitted out.
//!
//! The counterpart of `asm.rs`'s pinned bodies, and the half of this backend that `wasm.rs` cannot
//! reach. `wasm.rs` compiles a project, hands the bytes to `wasm-tools`, and runs them under
//! `wasmtime` — so on a host with neither, all it asserts is that the compile returned `Ok`. CI's
//! wasm cell is exactly such a host: `jals-javac`'s tests run under `wasm32-wasip1`, where
//! spawning a process is unsupported, so every engine-backed assertion in this crate stands down
//! on the one platform this backend targets.
//!
//! Nothing here reaches for a host. [`CompileWasm::module`] hands back the module before it is
//! encoded, and the instructions are read straight out of it, so these assertions hold wherever
//! the crate compiles.
//!
//! # The trailing `Unreachable`
//!
//! Every pinned body ends with one, and it is not dead weight the lowering forgot to drop. wasm
//! validates a function against its result type on *fallthrough*, and Java's own rules do not:
//! a method whose every path `return`s still falls off the end as far as the validator is
//! concerned. `unreachable` is what satisfies it without inventing a value to return, and it is
//! also what the last arm of a `switch` that returns from every case leaves behind.

use expect_test::expect;
use jals_hir::{FileAnalysis, FileId, FileSemantics, ProjectIndex, TypedFile};
use jals_javac::wasm::{CompileWasm, ExportKind, Instr, Module};
use jals_syntax::SyntaxNode;
use std::fmt::Write as _;

/// Compile every source as one module — which is what "the whole project" means for a target with
/// no dynamic loading and no classpath — and stop at the module rather than at its bytes.
fn module_of(sources: &[&str]) -> Module {
    let roots: Vec<(FileId, SyntaxNode)> = sources
        .iter()
        .enumerate()
        .map(|(index, text)| {
            (
                FileId(u32::try_from(index).expect("a source count that fits")),
                jals_exec::block_on_inline(jals_syntax::Parse::parse(text)).syntax(),
            )
        })
        .collect();
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&roots).with_stdlib().build());

    let analyses: Vec<FileAnalysis> = roots
        .iter()
        .map(|(_, root)| jals_exec::block_on_inline(FileAnalysis::of(root)))
        .collect();
    // The bindings own the inference memo the witnesses borrow, so both live to the end.
    let semantics: Vec<FileSemantics<'_>> = roots
        .iter()
        .zip(&analyses)
        .map(|((file, _), analysis)| analysis.in_project(&index, *file))
        .collect();
    let inputs: Vec<TypedFile<'_>> = semantics
        .iter()
        .map(|binding| jals_exec::block_on_inline(binding.typed()))
        .collect();
    CompileWasm::module(&inputs, &index).unwrap_or_else(|error| panic!("compile: {error}"))
}

/// The exported function named `export`, rendered as its declared locals followed by its
/// instructions.
///
/// Indented by nesting rather than numbered by offset, because wasm's control flow is structured:
/// a `br` names how many blocks to leave, not a byte to jump to, so the nesting *is* the
/// information an offset carries on the JVM side.
fn body_of(module: &Module, export: &str) -> String {
    let Some((_, _, index)) = module
        .exports
        .iter()
        .find(|(name, kind, _)| name == export && matches!(kind, ExportKind::Func))
    else {
        let names: Vec<&str> = module
            .exports
            .iter()
            .map(|(name, _, _)| name.as_str())
            .collect();
        panic!("no exported function `{export}`; the module exports {names:?}")
    };
    let func = &module.funcs[usize::try_from(*index).expect("a function index that fits")];

    let mut rendered = String::new();
    writeln!(rendered, "locals: {:?}", func.locals).expect("write to a String");
    let mut depth = 0usize;
    for instruction in &func.body {
        // `end` and `else` describe the structure they close, so they sit at its level.
        if matches!(instruction, Instr::End | Instr::Else) {
            depth = depth.saturating_sub(1);
        }
        writeln!(
            rendered,
            "{:indent$}{instruction:?}",
            "",
            indent = depth * 2
        )
        .expect("write to a String");
        if matches!(
            instruction,
            Instr::Block
                | Instr::BlockTyped(_)
                | Instr::Loop
                | Instr::If
                | Instr::IfTyped(_)
                | Instr::Else
                | Instr::TryTable(_)
        ) {
            depth += 1;
        }
    }
    rendered
}

// --- the shape of an ordinary body ---------------------------------------------------------

/// The smallest whole body, so that a change in how a method is framed — a stray `end`, a
/// spurious local — is visible before it is visible anywhere else.
#[test]
fn a_static_method_lowers_to_its_parameters_and_its_expression() {
    let module = module_of(&["public class A { public static int bump(int x) { return x + 1; } }"]);
    expect![[r"
        locals: []
        LocalGet(0)
        I32Const(1)
        Numeric(Add, I32)
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "bump"));
}

// --- structured control ----------------------------------------------------------------------

/// A `while` is two nested labels, not one: the `loop` is where `continue` goes and the `block`
/// around it is where `break` goes. This is the claim that makes the whole backend lower from the
/// syntax tree rather than from the other backend's `goto` stream, and it had no engine-free test.
#[test]
fn a_while_loop_is_a_block_around_a_loop() {
    let module = module_of(&[r"
public class B {
    public static int total(int n) {
        int sum = 0;
        int i = 0;
        while (i < n) {
            sum = sum + i;
            i = i + 1;
        }
        return sum;
    }
}
"]);
    expect![[r"
        locals: [I32, I32]
        I32Const(0)
        LocalSet(1)
        I32Const(0)
        LocalSet(2)
        Block
          Loop
            LocalGet(2)
            LocalGet(0)
            Numeric(Lt, I32)
            I32Eqz
            BrIf(1)
            LocalGet(1)
            LocalGet(2)
            Numeric(Add, I32)
            LocalSet(1)
            LocalGet(2)
            I32Const(1)
            Numeric(Add, I32)
            LocalSet(2)
            Br(0)
          End
        End
        LocalGet(1)
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "total"));
}

/// `?:` lowers to a typed `if`, never to `select`. `select` pops *both* value operands, so both
/// arms would already have run — `c ? f() : g()` would call both, and a trapping arm would trap
/// whether or not it was taken. JLS §15.25 evaluates exactly one arm.
///
/// No program that avoids side effects in its arms can tell the two apart, so running one proves
/// nothing here.
#[test]
fn a_conditional_expression_is_a_typed_if_rather_than_a_select() {
    let module =
        module_of(&["public class C { public static int pick(boolean b) { return b ? 1 : 2; } }"]);
    expect![[r"
        locals: []
        LocalGet(0)
        IfTyped(I32)
          I32Const(1)
        Else
          I32Const(2)
        End
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "pick"));
}

/// A `switch` over dense keys becomes a `br_table`, and the bounds check is the `i32.sub` that
/// precedes it: the index is read as **unsigned**, so a key below the lowest wraps past 2³¹ and
/// lands on the default with every key above the highest.
#[test]
fn a_dense_switch_becomes_a_branch_table_with_one_subtraction_for_a_bounds_check() {
    let module = module_of(&[r"
public class D {
    public static int rank(int k) {
        switch (k) {
            case 1: return 10;
            case 2: return 20;
            default: return 0;
        }
    }
}
"]);
    expect![[r"
        locals: []
        Block
          Block
            Block
              Block
                LocalGet(0)
                I32Const(1)
                Numeric(Sub, I32)
                BrTable([0, 1], 2)
              End
              I32Const(10)
              Return
            End
            I32Const(20)
            Return
          End
          I32Const(0)
          Return
        End
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "rank"));
}

// --- the garbage-collected heap ----------------------------------------------------------------

/// A class is a `struct` type and a field is a slot in it, so `new` / write / read is
/// `struct.new_default` / `struct.set` / `struct.get` against one type index. Nothing allocates,
/// traces, or frees here — the host's collector owns the object from `struct.new_default` on.
#[test]
fn a_field_write_and_read_go_through_one_struct_type() {
    let module = module_of(&[r"
public class E {
    int f;
    public static int roundtrip() {
        E e = new E();
        e.f = 7;
        return e.f;
    }
}
"]);
    expect![[r"
        locals: [Ref(RefType { nullable: true, heap: Concrete(1) }), Ref(RefType { nullable: true, heap: Concrete(1) })]
        StructNewDefault(1)
        LocalSet(0)
        LocalGet(0)
        LocalSet(1)
        LocalGet(1)
        I32Const(7)
        StructSet(1, 0)
        LocalGet(0)
        StructGet(1, 0)
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "roundtrip"));
}

// --- conversions --------------------------------------------------------------------------------

/// **The float-to-integer truncation saturates.** wasm's `i32.trunc_f32_s` traps on a NaN or an
/// out-of-range value; JLS §5.1.3 requires 0 for a NaN and the nearest representable value
/// otherwise. Using the trapping form would turn `(int) (0.0f / 0.0f)` from a 0 into a crash.
///
/// The two opcodes differ nowhere else, so no program that stays in range can tell them apart —
/// which is exactly why this is asserted on the instruction rather than on a result.
#[test]
fn a_narrowing_from_a_float_saturates_and_then_sign_extends() {
    let module =
        module_of(&["public class F { public static int clamp(float f) { return (byte) f; } }"]);
    expect![[r"
        locals: []
        LocalGet(0)
        I32TruncSatF32S
        I32Extend8S
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "clamp"));
}

/// `char` is the one unsigned integral type, so its narrowing masks where `byte` and `short`
/// sign-extend. wasm has no `i32.extend16_u`, so the mask *is* the conversion.
#[test]
fn a_narrowing_to_char_masks_because_it_is_the_unsigned_one() {
    let module =
        module_of(&["public class G { public static int low(int x) { return (char) x; } }"]);
    expect![[r"
        locals: []
        LocalGet(0)
        I32Const(65535)
        Numeric(And, I32)
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "low"));
}

/// wasm has no integer negation at all, so `-x` is emitted as the subtraction it is rather than
/// as an opcode that does not exist — while a floating `-d` does have one.
#[test]
fn integer_negation_is_a_subtraction_and_floating_negation_is_not() {
    let module = module_of(&[r"
public class H {
    public static int ineg(int x) { return -x; }
    public static double dneg(double d) { return -d; }
}
"]);
    expect![[r"
        locals: []
        I32Const(0)
        LocalGet(0)
        Numeric(Sub, I32)
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "ineg"));
    expect![[r"
        locals: []
        LocalGet(0)
        F64Neg
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "dneg"));
}

/// A `long` shift needs its count *extended* on the way in: `i64.shl` takes two `i64`s where the
/// JVM's `lshl` takes a `long` and an `int`. A real difference between the targets, and one the
/// JVM backend needs nothing for.
#[test]
fn a_long_shift_extends_its_count_because_wasm_takes_two_i64s() {
    let module =
        module_of(&["public class I { public static long up(long v, int n) { return v << n; } }"]);
    expect![[r"
        locals: []
        LocalGet(0)
        LocalGet(1)
        I64ExtendI32S
        Numeric(Shl, I64)
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "up"));
}

// --- arrays -------------------------------------------------------------------------------------

/// An array is an `array` type of its own, and `length` is `array.len` rather than a field.
#[test]
fn an_array_is_allocated_defaulted_and_measured_with_its_own_instructions() {
    let module = module_of(&[r"
public class J {
    public static int use() {
        int[] a = new int[3];
        a[1] = 5;
        return a[1] + a.length;
    }
}
"]);
    expect![[r"
        locals: [Ref(RefType { nullable: true, heap: Concrete(2) }), Ref(RefType { nullable: true, heap: Concrete(2) }), I32]
        I32Const(3)
        ArrayNewDefault(2)
        LocalSet(0)
        LocalGet(0)
        LocalSet(1)
        I32Const(1)
        LocalSet(2)
        LocalGet(1)
        LocalGet(2)
        I32Const(5)
        ArraySet(2)
        LocalGet(0)
        I32Const(1)
        ArrayGet(2)
        LocalGet(0)
        ArrayLen
        Numeric(Add, I32)
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "use"));
}

// --- conversions and casts erasure leaves behind -----------------------------------------------

/// A widening primitive conversion (JLS §5.1.2) is silent in Java and an instruction in wasm.
///
/// `static long take(long x)` called as `take(1)` puts an `i32` where the signature says `i64`.
/// Nothing on this side said so and the validator refused the module — which is exactly the failure
/// an engine-free assertion catches on the platform this backend targets, where no validator runs.
/// `value_as` routes a *declaration* through the numeric path and so got `long a = 1;` right all
/// along, which is what made one source spell one conversion two ways.
#[test]
fn an_int_argument_widens_to_a_long_parameter() {
    let module = module_of(&[r"
public class Widen {
    static long take(long x) { return x + 1; }
    public static long run() { return take(1); }
}
"]);
    expect![[r"
        locals: []
        I32Const(1)
        I64ExtendI32S
        Call(0)
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "run"));
}

/// The fall-through of a dispatch chain casts nothing, so an erased receiver arrives at it as
/// `anyref` — and the function it calls is declared over a concrete struct.
///
/// `<T extends C> int g(T t) { return t.m(); }` is the everyday shape. Each `ref.test` arm above
/// casts the receiver to the type it just tested for; the fall-through tested nothing, and pushed
/// the spilled local as it stood.
#[test]
fn a_dispatch_fall_through_casts_its_receiver() {
    let module = module_of(&[r"
public class Fall {
    static class C { int m() { return 3; } }
    static class D extends C { int m() { return 4; } }
    static <T extends C> int g(T t) { return t.m(); }
    public static int run() { return g(new D()); }
}
"]);
    let body = body_of(&module, "g");
    assert!(
        body.contains("RefCast"),
        "the fall-through arm must narrow the receiver it spilled:\n{body}"
    );
    // Two casts per arm would mean the fall-through is still pushing an `anyref`: one for the arm
    // that tested `D`, and one for the fall-through to `C`.
    assert_eq!(
        body.matches("RefCast").count(),
        2,
        "one cast per tested arm and one for the fall-through:\n{body}"
    );
}

/// A store into a field or an array element declared at a concrete type is a place the validator
/// checks exactly, and erasure puts an `anyref` on the stack in front of it.
///
/// The premise the old code rested on — "a reference target is already the right type or the
/// analysis would not have typed the assignment" — held only while every reference this backend
/// produced was concrete. `b.held = id(c);` was a module `wasm-tools` refuses.
#[test]
fn a_store_of_an_erased_value_casts_to_what_the_place_holds() {
    let module = module_of(&[r"
public class Store {
    static class Cell { int v; }
    static class Box { Cell held; }
    static <T> T id(T t) { return t; }
    public static int run() {
        Cell c = new Cell();
        Box b = new Box();
        b.held = id(c);
        Cell[] a = new Cell[1];
        a[0] = id(c);
        return b.held.v + a[0].v;
    }
}
"]);
    let body = body_of(&module, "run");
    // One for the field store, one for the array element store, and one for each of the two reads
    // back through `held` / `a[0]`, which are erased the same way.
    assert!(
        body.matches("RefCast").count() >= 2,
        "both the field store and the array-element store narrow what they store:\n{body}"
    );
}

/// A method whose implementation is inherited rather than declared still has one.
///
/// `class C extends Base implements I {}` declares nothing at all, and its implementation of `I.f`
/// is `Base.f`. `Base` is no subtype of `I`, so asking whether `Base.f` *overrides* `I.f` correctly
/// answers no — and answers the wrong question. Reading that no as "nothing in this module
/// implements it" emitted `unreachable` against a receiver whose body was one function away: a
/// module that validated, instantiated, and trapped, where the merge base refused by name.
#[test]
fn an_inherited_implementation_is_dispatched_to() {
    let module = module_of(&[r"
public class Inherit {
    interface I { int f(); }
    static class Base { public int f() { return 7; } }
    static class C extends Base implements I {}
    public static int run() { I i = new C(); return i.f(); }
}
"]);
    let body = body_of(&module, "run");
    assert!(
        !body.contains("Unreachable\n        Return"),
        "the call must dispatch, not trap:\n{body}"
    );
    assert!(
        body.contains("RefTest") && body.contains("Call("),
        "an inherited implementation is reached through the dispatch chain:\n{body}"
    );
}

/// A lambda the index could give no single abstract method is refused, not laid out.
///
/// A lambda is typed by its *target*, and in argument position that target is the parameter of an
/// overload chosen after the index is built — so `use(() -> 5)` reached the layout with no method
/// member. Skipping it left the struct declared with no body behind it: the creation emitted
/// `struct.new_default`, the object implemented nothing, and the call through the interface found
/// no override and became `unreachable`.
#[test]
fn a_lambda_with_no_abstract_method_is_refused() {
    let source = r"
public class Arg {
    interface I { int f(); }
    static int use(I i) { return i.f(); }
    public static int run() { return use(() -> 5); }
}
";
    let root = jals_exec::block_on_inline(jals_syntax::Parse::parse(source)).syntax();
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), root.clone())])
            .with_stdlib()
            .build(),
    );
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    let semantics = analysis.in_project(&index, FileId(0));
    let typed = jals_exec::block_on_inline(semantics.typed());
    let error = CompileWasm::module(&[typed], &index).expect_err("a refusal, not a trap");
    assert_eq!(
        error.to_string(),
        "a lambda or method reference with no single abstract method is not compiled to wasm yet"
    );
}

/// A `static` call with no body in the module is a missing implementation, not an unreachable
/// dispatch: there is no receiver for "no object of this type can exist" to be about.
#[test]
fn a_native_call_is_reported_rather_than_trapped() {
    let source = r"
public class Native {
    static class N { static native int f(); }
    public static int run() { return N.f(); }
}
";
    let root = jals_exec::block_on_inline(jals_syntax::Parse::parse(source)).syntax();
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), root.clone())])
            .with_stdlib()
            .build(),
    );
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    let semantics = analysis.in_project(&index, FileId(0));
    let typed = jals_exec::block_on_inline(semantics.typed());
    let error = CompileWasm::module(&[typed], &index).expect_err("a refusal, not a trap");
    assert!(
        error
            .to_string()
            .contains("no body for it is compiled into the module"),
        "the report names the missing implementation: {error}"
    );
}
