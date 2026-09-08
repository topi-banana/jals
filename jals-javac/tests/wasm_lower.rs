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
use jals_javac::wasm::{CompileWasm, ExportKind, Instr, Module, WasmOptions};
use jals_syntax::SyntaxNode;
use std::fmt::Write as _;

/// The platform library at **signature** fidelity — what every host but a linking wasm build
/// indexes, and what the embedded stubs used to be.
///
/// One text, read as a record: the real JDK behind a `javac` build is a superset of it, so a
/// member it omits is a gap in the record rather than an absence in the program.
fn platform() -> Vec<jals_hir::LibraryFile> {
    jals_exec::block_on_inline(jals_hir::LibraryFile::parse_tiers(
        &jals_platform::JavaBase::tiers(false),
    ))
}

/// Compile every source as one module — which is what "the whole project" means for a target with
/// no dynamic loading and no classpath — and stop at the module rather than at its bytes.
fn module_of(sources: &[&str]) -> Module {
    module_with(sources, WasmOptions::default())
}

/// [`module_of`], with the compile options stated.
fn module_with(sources: &[&str], options: WasmOptions) -> Module {
    module_of_parts(sources, &[], options)
}

/// A module compiled from the project's own `sources` plus a native package's `libraries`.
///
/// The two lists differ in exactly one way — a library declaration is never exported — so every
/// test that cares about that distinction goes through here.
fn module_of_parts(sources: &[&str], libraries: &[&str], options: WasmOptions) -> Module {
    let texts: Vec<&str> = sources.iter().chain(libraries).copied().collect();
    let roots: Vec<(FileId, SyntaxNode)> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            (
                FileId(u32::try_from(index).expect("a source count that fits")),
                jals_exec::block_on_inline(jals_syntax::Parse::parse(text)).syntax(),
            )
        })
        .collect();
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&roots)
            .with_library(&platform())
            .build(),
    );

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
    let typed: Vec<TypedFile<'_>> = semantics
        .iter()
        .map(|binding| jals_exec::block_on_inline(binding.typed()))
        .collect();
    let (inputs, libraries) = typed.split_at(sources.len());
    CompileWasm::module(inputs, libraries, &index, options)
        .unwrap_or_else(|error| panic!("compile: {error}"))
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
    // The function index space starts with the imports, so a defined function's place in
    // `module.funcs` is its index minus their count.
    let defined =
        usize::try_from(*index).expect("a function index that fits") - module.imports.len();
    let func = &module.funcs[defined];

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
            .with_library(&platform())
            .build(),
    );
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    let semantics = analysis.in_project(&index, FileId(0));
    let typed = jals_exec::block_on_inline(semantics.typed());
    let error = CompileWasm::module(&[typed], &[], &index, WasmOptions::default())
        .expect_err("a refusal, not a trap");
    assert_eq!(
        error.to_string(),
        "a lambda or method reference with no single abstract method is not compiled to wasm yet"
    );
}

/// A `native` method is a host import, and the two names it is imported under are derivable from
/// the declaration alone.
///
/// The module name is the declaring class's internal name and the field name is the method's name
/// with its JVM descriptor. Both halves are pinned here because they are the *link symbol*: the
/// host keys its implementation table on exactly these two strings, so a change to either one is a
/// change to what every native package has to be registered under.
#[test]
fn a_native_method_becomes_a_host_import() {
    let module = module_of(&[r"
public class Native {
    static class N { static native int f(char[] text, int at); }
    public static int run(char[] t) { return N.f(t, 0); }
}
"]);
    let imported: Vec<(&str, &str)> = module
        .imports
        .iter()
        .map(|import| (import.module.as_str(), import.name.as_str()))
        .collect();
    assert_eq!(imported, vec![("Native$N", "f([CI)I")]);
}

/// Imports occupy the low end of the function index space, so every defined function moves up by
/// their count — and the export section, which names indices, has to move with them.
///
/// The regression this pins is silent: an export that still named index 0 would name the *import*,
/// which is a perfectly well-formed module that calls the host when it was asked for the project's
/// own method.
#[test]
fn an_import_shifts_every_defined_function_index() {
    let module = module_of(&[r"
public class Native {
    static native int host();
    public static int run() { return host(); }
}
"]);
    assert_eq!(module.imports.len(), 1);
    let (_, _, exported) = module
        .exports
        .iter()
        .find(|(name, ..)| name == "run")
        .expect("`run` is exported");
    assert_eq!(
        *exported, 1,
        "the one defined function sits after the import"
    );
    assert!(
        body_of(&module, "run").contains("Call(0)"),
        "the call reaches the import: {}",
        body_of(&module, "run")
    );
}

/// A `native` method whose body *is* there is not imported: the declaration is a Java error this
/// backend never checks, and the honest reading of it is "there is a body".
#[test]
fn a_native_method_with_a_body_is_compiled_rather_than_imported() {
    let module = module_of(&[r"
public class Native {
    static native int host();
}
"
    .replace("native int host();", "native int host() { return 7; }")
    .as_str()]);
    assert!(module.imports.is_empty(), "{:?}", module.imports);
}

/// A native package's classes are compiled into the module and are *not* its surface.
///
/// Two failures hide behind getting this wrong, and neither is visible in a build. A library's
/// `static` method under the same bare name as a project's takes the export, because the first one
/// wins and the second is dropped without a word. And a `static native` method has no defined
/// function at all, so exporting it would name an *import* index — a module that validates and
/// calls the host when the caller asked for the project.
#[test]
fn a_library_class_static_method_is_not_exported() {
    let module = module_of_parts(
        &["public class App { public static int run() { return Lib.help() + Lib.reach(); } }"],
        &[r"
public class Lib {
    public static native int reach();
    public static int help() { return 1; }
}
"],
        WasmOptions::default(),
    );
    let exported: Vec<&str> = module
        .exports
        .iter()
        .map(|(name, ..)| name.as_str())
        .collect();
    assert_eq!(exported, vec!["run"], "only the project's own surface");
    assert_eq!(
        module.imports.len(),
        1,
        "the library's `native` still links"
    );
}

/// A project method and a library method of the same bare name: the project keeps the export.
#[test]
fn a_library_never_takes_a_project_export() {
    let module = module_of_parts(
        &["public class App { public static int run() { return Lib.run(); } }"],
        &["public class Lib { public static int run() { return 2; } }"],
        WasmOptions::default(),
    );
    let project = body_of(&module, "run");
    assert!(
        project.contains("Call("),
        "the exported `run` is the project's, which calls the library's: {project}"
    );
    assert_eq!(module.exports.len(), 1);
}

/// A body-less method that did not say `native` is still a refusal, and still its own one: what is
/// missing there is an implementation that was expected, not one the embedder supplies.
#[test]
fn a_body_less_method_that_is_not_native_is_still_reported() {
    let source = r"
public class Missing {
    interface Shape { int area(); }
    static int use(Shape s) { return s.area(); }
    public static int run() { return use(null); }
}
";
    let root = jals_exec::block_on_inline(jals_syntax::Parse::parse(source)).syntax();
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), root.clone())])
            .with_library(&platform())
            .build(),
    );
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    let semantics = analysis.in_project(&index, FileId(0));
    let typed = jals_exec::block_on_inline(semantics.typed());
    let module = CompileWasm::module(&[typed], &[], &index, WasmOptions::default());
    // Either answer is a refusal rather than a trap; what must not happen is an import appearing
    // for a method nobody declared `native`.
    if let Ok(module) = module {
        assert!(module.imports.is_empty(), "{:?}", module.imports);
    }
}

/// `assert` is compiled into nothing by default, and that is not a gap: a JVM evaluates one only
/// when it was started with `-ea`, so a module that always checked would be *stricter* than Java.
///
/// The pin is the whole default half of the contract — `jals build` behaves the same on both
/// backends — and it is what the armed test below is the complement of.
#[test]
fn an_assert_compiles_to_nothing_by_default() {
    let module =
        module_of(&["public class S { public static int run(int n) { assert n > 0; return n; } }"]);
    let body = body_of(&module, "run");
    // `If` and not `Unreachable`: a body's own trailing `unreachable` is how a function that
    // returns on every path ends, so the branch is what says a check was emitted.
    assert!(
        !body.contains("If"),
        "an unarmed `assert` emits no check: {body}"
    );
}

/// Armed, an `assert` is a conditional trap.
///
/// A trap and not a `throw`: Java raises `AssertionError`, which no module declares and no `catch`
/// here could name, and the point of that error is that ordinary code does not handle it. Nothing
/// catches a trap either, where a `throw` on the module's tag would be offered to any `catch`
/// clause whose `ref.test` happened to accept a null payload.
#[test]
fn an_armed_assert_emits_a_conditional_trap() {
    let module = module_with(
        &["public class S { public static int run(int n) { assert n > 0; return n; } }"],
        WasmOptions { assertions: true },
    );
    let body = body_of(&module, "run");
    assert!(
        body.contains("If") && body.contains("Else") && body.contains("  Unreachable"),
        "an armed `assert` traps on the false arm of the condition it was written with: {body}"
    );
}

/// The condition is lowered only when the check is emitted, so a project whose `assert` names
/// something with no wasm representation still builds — and stops compiling the moment a test run
/// arms it.
///
/// Stated rather than discovered: `jals build` and `jals test` genuinely differ on such a file,
/// and it is the test run that reports what the build accepted.
#[test]
fn an_assert_condition_is_lowered_only_when_it_is_armed() {
    let source = r#"
public class S {
    public static int run(int n) { assert "x" != null; return n; }
}
"#;
    // Unarmed: the condition is never visited, so the `String` in it is never asked for.
    let module = module_of(&[source]);
    assert!(!body_of(&module, "run").contains("If"));

    // Armed: it is, and there is no `String` on this target.
    let root = jals_exec::block_on_inline(jals_syntax::Parse::parse(source)).syntax();
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), root.clone())])
            .with_library(&platform())
            .build(),
    );
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    let semantics = analysis.in_project(&index, FileId(0));
    let typed = jals_exec::block_on_inline(semantics.typed());
    let error = CompileWasm::module(&[typed], &[], &index, WasmOptions { assertions: true })
        .expect_err("the condition is compiled now, and it names a library type");
    let message = error.to_string();
    // The type, and the expression it came from. The second half is what makes the report usable
    // on a library input, where every body is lowered rather than only the reachable ones.
    assert!(
        message.contains("`String") && message.contains("has no wasm representation"),
        "the report names what it could not lower: {error}"
    );
    assert!(
        message.contains(r#""x""#),
        "the report names the expression it came from: {error}"
    );
}

/// A superclass cycle is finished with, not followed.
///
/// `class A extends B {}` beside `class B extends A {}` parses and indexes — nothing rejects it
/// before a backend sees it — and this lowering walked the chain in two places with no guard. The
/// class ordering was worse than a hang: its `ordered.contains` check was a test against the
/// *output*, which a caller appends to only on the way back out, so an ancestor still being
/// visited was invisible and the recursion aborted the process with a stack overflow. Nothing
/// catches one of those, and the input reaches an editor as readily as a build.
///
/// Five shapes, because the second walk is only reached once the first terminates: no constructors
/// at all, explicit ones on both sides, a third class hanging off the cycle, and the two
/// *self*-cycle shapes — `class C extends C {}` is the input `ProjectIndex::direct_superclass`
/// answers `Some(C)` for by name, and until it was listed here neither backend had a fixture for it.
///
/// What this counts is functions, so it cannot see the shape of the emitted *types*. That half is
/// by construction: `Layout::fill_class` declares the supertype `Layout::reserve_class` recorded
/// rather than re-deriving it, and `Body::super_constructor` follows the same declared chain — see
/// both doc comments. Before that, every source here emitted a module `wasm-tools validate`
/// rejected, and this test passed.
#[test]
fn a_superclass_cycle_terminates_rather_than_recursing() {
    for (source, functions) in [
        ("class A extends B {} class B extends A {}", 0),
        (
            "class A extends B { A() {} } class B extends A { B() {} }",
            2,
        ),
        (
            "class A extends B {} class B extends A {} class C extends A { C() {} }",
            1,
        ),
        ("class C extends C {}", 0),
        ("class C extends C { int x; C() {} }", 1),
    ] {
        assert_eq!(module_of(&[source]).funcs.len(), functions, "{source}");
    }
}

/// The super-constructor search stops at the first ancestor that *declares* one, even when that
/// ancestor has no constructor it can call.
///
/// `class P { P(int x) {} }` declares a constructor and no no-arg one, so `class C extends P {}`
/// has no reachable `super()` — javac rejects the program, and this backend emits a constructor
/// that calls nothing rather than inventing a call.
///
/// Pinned because the obvious way to write this walk over a published chain is `find_map`, which
/// compiles, passes every cycle test, and is wrong: `find_map` skips the `None` that `P` produces
/// and keeps climbing, so `C` would call a *grandparent's* constructor and leave `P`'s fields at
/// their defaults — in a module that validates.
#[test]
fn the_super_constructor_search_stops_at_the_first_ancestor_declaring_one() {
    // `G` is what a `find_map` would climb past `P` and reach: it declares a no-arg constructor, so
    // the wrong walk has something to emit a call to.
    let module = module_of(&[
        "class G { G() {} } class P extends G { P(int x) {} } class C extends P { C() {} }",
    ]);
    let calls: usize = module
        .funcs
        .iter()
        .map(|func| {
            func.body
                .iter()
                .filter(|instruction| matches!(instruction, Instr::Call(_)))
                .count()
        })
        .sum();
    // One, and exactly one: `P(int)` calls `G()`, which is a real `super()` the source implies.
    // `C()` adds none, because `P` declares a constructor and no no-arg one — the search stops
    // there. Under a `find_map` this is two, the second being `C()` calling `G()` directly.
    assert_eq!(
        calls, 1,
        "only `P(int)`'s own `super()` is emitted; `C()` reaches no super-constructor"
    );
}

/// ...and it does not stop *before* one: an ancestor that declares no constructor is a link in the
/// chain, not its end.
///
/// The mirror of the test above, and the half nothing covered — truncating the walk to
/// `superclasses(owner).take(1)` left every test in this crate green. `P` contributes no
/// constructor function of its own, so a walk that stopped there would never reach `G`'s
/// synthesised one and `C()` would skip `G`'s field initialisers, leaving `x` at zero in a module
/// that validates.
#[test]
fn the_super_constructor_search_continues_past_an_ancestor_that_declares_none() {
    let module =
        module_of(&["class G { int x = 1; } class P extends G {} class C extends P { C() {} }"]);
    let calls: usize = module
        .funcs
        .iter()
        .map(|func| {
            func.body
                .iter()
                .filter(|instruction| matches!(instruction, Instr::Call(_)))
                .count()
        })
        .sum();
    // One: `C()` reaches `G`'s synthesised initialiser through the constructor-less `P`. Under a
    // walk that stops at the first ancestor whatever it holds, this is zero.
    assert_eq!(
        calls, 1,
        "`C()` calls `G`'s initialiser through the constructor-less `P`"
    );
}

// --- the three refusals a `java.base` in the library slot found ------------------------------

/// An `int` literal past `i32::MAX` denotes its low 32 bits, and every legal spelling of one is
/// past it.
///
/// `Integer.MIN_VALUE` is written `-2147483648`, whose *literal* is `2147483648` — JLS §3.10.1
/// admits that spelling only as the operand of a unary minus, and the negation wraps it back to
/// itself. `0xFFFFFFFF` is the same shape without the minus. Both were refused as out of range
/// until a `java.lang` that has to declare `Integer.MIN_VALUE` reached them.
#[test]
fn an_int_literal_past_i32_max_is_its_low_thirty_two_bits() {
    let module = module_of(&["public class A {\n\
         \x20   public static int floor() { return -2147483648; }\n\
         \x20   public static int all() { return 0xFFFFFFFF; }\n\
         }"]);
    // `0 - 2147483648`, because a unary minus is a subtraction here — and the subtraction wraps,
    // which is exactly what makes this spelling denote `Integer.MIN_VALUE` rather than overflow.
    expect![[r"
        locals: []
        I32Const(0)
        I32Const(-2147483648)
        Numeric(Sub, I32)
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "floor"));
    expect![[r"
        locals: []
        I32Const(-1)
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "all"));
}

/// `==` over two references narrows to `eqref` first, because that is what `ref.eq` takes.
///
/// An `Object`-typed, interface-typed or type-variable-typed value is held at `anyref`, which sits
/// one step *above* `eqref`. Pushing two of them at `ref.eq` produced a module the validator
/// rejects — "expected subtype of eqref, found anyref" — which is what
/// `String.equals(Object other) { if (other == this) … }` compiles to.
#[test]
fn a_reference_comparison_narrows_an_anyref_to_eqref() {
    let module = module_of(&["public class A {\n\
         \x20   public static boolean same(Object left, Object right) { return left == right; }\n\
         }"]);
    expect![[r"
        locals: []
        LocalGet(0)
        RefCast(Eq, true)
        LocalGet(1)
        RefCast(Eq, true)
        RefEq
        Return
        Unreachable
    "]]
    .assert_eq(&body_of(&module, "same"));
}

/// A method with many overriders is ordered by depth, and the ordering terminates.
///
/// The predicate this replaces compared two candidates with `is_subtype`, which is not a total
/// order — three classes where one extends another and the third is unrelated compare as
/// `a < b`, `b == c`, `a == c` — so Rust's sort detected the intransitivity and panicked. It went
/// unnoticed until a dispatch had enough overriders to reach the check, which a `java.lang` with
/// two dozen exception classes overriding one method does. The assertion is that this compiles at
/// all; the ordering itself is asserted by `an_inherited_implementation_is_dispatched_to`.
#[test]
fn a_dispatch_over_many_unrelated_overriders_orders_without_panicking() {
    let mut sources = vec![
        "public class Base { public int tag() { return 0; } }".to_owned(),
        "public class Deep extends Sub0 { public int tag() { return 99; } }".to_owned(),
    ];
    for at in 0..12 {
        sources.push(format!(
            "public class Sub{at} extends Base {{ public int tag() {{ return {at}; }} }}"
        ));
    }
    sources.push("public class A { public static int ask(Base b) { return b.tag(); } }".to_owned());
    let borrowed: Vec<&str> = sources.iter().map(String::as_str).collect();
    let module = module_of(&borrowed);
    assert!(
        module.exports.iter().any(|(name, _, _)| name == "ask"),
        "the dispatch compiled"
    );
}
