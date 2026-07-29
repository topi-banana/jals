//! The assembler's acceptance tests: a class it built is loaded, verified, and run by a real JVM.
//!
//! Nothing here goes through a Java parser — bodies are emitted directly against the assembler.
//! That is deliberate: it isolates branch resolution, frame sizing, and `StackMapTable` derivation
//! from the lowering that will later feed them.

use std::fmt::Write as _;
use std::process::{Command, Stdio};

use expect_test::expect;
use jals_classfile::{
    AttributeBody, ClassAccessFlags, ClassFile, ConstantPool, MethodAccessFlags, MethodInfo,
    VerificationType,
};
use jals_javac::jvm::{AsmError, Assembler, BinOp, Branch, Compare, Numeric, Receiver};

/// Java 25, matching the class files the rest of the workspace pins its fixtures to.
const MAJOR_JAVA_25: u16 = 69;

const PRINT_STREAM: &str = "java/io/PrintStream";
const SYSTEM_OUT: &str = "Ljava/io/PrintStream;";

/// Whether a JVM is on this host. A missing one stands the test down — loudly, because the JVM is
/// the only authority on whether an emitted class file verifies, and a quiet stand-down reads as a
/// pass.
fn java_available() -> bool {
    let present = Command::new("java")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !present {
        eprintln!("note: no `java` on this host; this test is checking less than it looks like");
    }
    present
}

/// A public class named `name` extending `Object`, with a default constructor and `main`.
fn class_with_main(
    name: &str,
    main: impl FnOnce(&mut Assembler<'_>) -> Result<(), jals_javac::jvm::AsmError>,
) -> ClassFile {
    class_with_statics(name, &[], main)
}

/// [`class_with_main`], plus a `static` field per `(name, descriptor)` in `statics`.
fn class_with_statics(
    name: &str,
    statics: &[(&str, &str)],
    main: impl FnOnce(&mut Assembler<'_>) -> Result<(), jals_javac::jvm::AsmError>,
) -> ClassFile {
    let mut pool = ConstantPool::new();
    let this_class = pool.class_index(name).expect("this");
    let super_class = pool.class_index("java/lang/Object").expect("super");

    let init_name = pool.utf8_index("<init>").expect("<init>");
    let init_descriptor = pool.utf8_index("()V").expect("()V");
    let init_code = {
        let mut asm = Assembler::new(&mut pool, Receiver::Constructor(name), "()V").expect("ctor");
        asm.load(0).expect("aload_0");
        asm.invoke_special("java/lang/Object", "<init>", "()V", false)
            .expect("super()");
        asm.return_(None).expect("return");
        asm.finish().expect("finish ctor")
    };

    let fields: Vec<_> = statics
        .iter()
        .map(|(field, descriptor)| jals_classfile::FieldInfo {
            access_flags: jals_classfile::FieldAccessFlags(
                jals_classfile::FieldAccessFlags::STATIC,
            ),
            name_index: pool.utf8_index(field).expect("field name"),
            descriptor_index: pool.utf8_index(descriptor).expect("field descriptor"),
            attributes: Vec::new(),
        })
        .collect();

    let main_name = pool.utf8_index("main").expect("main");
    let main_descriptor = pool
        .utf8_index("([Ljava/lang/String;)V")
        .expect("main descriptor");
    let main_code = {
        let mut asm = Assembler::new(&mut pool, Receiver::Static, "([Ljava/lang/String;)V")
            .expect("main asm");
        main(&mut asm).expect("emit main");
        asm.finish().expect("finish main")
    };

    let mut class = ClassFile::new(MAJOR_JAVA_25, 0, pool);
    class.access_flags = ClassAccessFlags(ClassAccessFlags::PUBLIC | ClassAccessFlags::SUPER);
    class.this_class = this_class;
    class.super_class = super_class;
    class.fields = fields;
    class.methods.push(MethodInfo {
        access_flags: MethodAccessFlags(MethodAccessFlags::PUBLIC),
        name_index: init_name,
        descriptor_index: init_descriptor,
        attributes: vec![init_code],
    });
    class.methods.push(MethodInfo {
        access_flags: MethodAccessFlags(MethodAccessFlags::PUBLIC | MethodAccessFlags::STATIC),
        name_index: main_name,
        descriptor_index: main_descriptor,
        attributes: vec![main_code],
    });
    class
}

/// `System.out.println("Hello, world!"); for (int i = 0; i < 3; i++) System.out.println(i);`
fn hello_and_count(asm: &mut Assembler<'_>) -> Result<(), jals_javac::jvm::AsmError> {
    asm.get_static("java/lang/System", "out", SYSTEM_OUT)?;
    asm.const_string("Hello, world!")?;
    asm.invoke_virtual(PRINT_STREAM, "println", "(Ljava/lang/String;)V")?;

    // `args` is slot 0, so the counter takes slot 1.
    asm.const_int(0)?;
    asm.store(1)?;

    let test = asm.label();
    let done = asm.label();
    asm.bind(test)?;
    asm.load(1)?;
    asm.const_int(3)?;
    asm.branch(Branch::IntCmp(Compare::Ge), done)?;

    asm.get_static("java/lang/System", "out", SYSTEM_OUT)?;
    asm.load(1)?;
    asm.invoke_virtual(PRINT_STREAM, "println", "(I)V")?;

    asm.load(1)?;
    asm.const_int(1)?;
    asm.binary(BinOp::Add, &VerificationType::Integer)?;
    asm.store(1)?;
    asm.branch(Branch::Always, test)?;

    asm.bind(done)?;
    asm.return_(None)
}

/// Run `class` (named `name`) on a real JVM and return its stdout.
fn run(name: &str, class: &ClassFile) -> String {
    let directory = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        directory.path().join(format!("{name}.class")),
        class.write(),
    )
    .expect("write class");

    let output = Command::new("java")
        // Two of these run at once under `cargo test`, and the JVM's shared perf-data file lives at a
        // fixed path per process id — a recycled one makes the second JVM print a warning onto the
        // stdout a test is comparing.
        .arg("-XX:-UsePerfData")
        .arg("-cp")
        .arg(directory.path())
        .arg(name)
        .output()
        .expect("run java");
    assert!(
        output.status.success(),
        "the JVM rejected the assembled class:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

/// The `Code` body of the method at `index`, and its stack map.
fn code_of(class: &ClassFile, index: usize) -> (&jals_classfile::CodeAttribute, String) {
    let body = class.methods[index]
        .attributes
        .iter()
        .find_map(|attribute| match &attribute.body {
            AttributeBody::Code(code) => Some(code),
            _ => None,
        })
        .expect("a Code attribute");

    let mut rendered = String::new();
    let mut pc = 0usize;
    for instruction in &body.code {
        writeln!(rendered, "{pc}: {instruction:?}").expect("write to a String");
        pc += instruction.encoded_len(pc);
    }
    for attribute in &body.attributes {
        if let AttributeBody::StackMapTable(frames) = &attribute.body {
            for frame in frames {
                writeln!(rendered, "frame {frame:?}").expect("write to a String");
            }
        }
    }
    (body, rendered)
}

#[test]
fn an_assembled_loop_runs_on_a_real_jvm() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Loop", hello_and_count);
    assert_eq!(run("Loop", &class), "Hello, world!\n0\n1\n2\n");
}

#[test]
fn an_assembled_class_reparses_unchanged() {
    let class = class_with_main("Loop", hello_and_count);
    let bytes = class.write();
    let reparsed =
        jals_exec::block_on_inline(ClassFile::read(bytes.as_slice())).expect("re-read the class");
    assert_eq!(reparsed, class, "the model changed across write/read");
    assert_eq!(reparsed.write(), bytes, "the bytes are not a fixed point");
}

/// The frame sizes are derived, not declared. `main` needs two stack words (the `PrintStream` plus
/// its argument) and two local slots (`args` and the counter).
#[test]
fn the_derived_frame_sizes_are_pinned() {
    let class = class_with_main("Loop", hello_and_count);
    let (main, _) = code_of(&class, 1);
    assert_eq!(main.max_stack, 2);
    assert_eq!(main.max_locals, 2);

    let (constructor, _) = code_of(&class, 0);
    assert_eq!(constructor.max_stack, 1);
    assert_eq!(constructor.max_locals, 1);
}

/// Enough dead weight to push any branch spanning it past a signed 16-bit offset.
///
/// `iconst_0` and `pop` are one byte each, so a pair is two. The exact filler does not matter —
/// only that the distance a branch has to cover exceeds 32767.
fn pad_past_short_branch(asm: &mut Assembler<'_>) -> Result<(), jals_javac::jvm::AsmError> {
    for _ in 0..17_000 {
        asm.const_int(0)?;
        asm.pop()?;
    }
    Ok(())
}

/// The same loop with a body too long to branch over in 16 bits: the forward conditional and the
/// backward `goto` both have to widen, and the widening moves the very offsets that triggered it.
fn far_loop(asm: &mut Assembler<'_>) -> Result<(), jals_javac::jvm::AsmError> {
    asm.const_int(0)?;
    asm.store(1)?;

    let test = asm.label();
    let done = asm.label();
    asm.bind(test)?;
    asm.load(1)?;
    asm.const_int(3)?;
    asm.branch(Branch::IntCmp(Compare::Ge), done)?;

    pad_past_short_branch(asm)?;

    asm.get_static("java/lang/System", "out", SYSTEM_OUT)?;
    asm.load(1)?;
    asm.invoke_virtual(PRINT_STREAM, "println", "(I)V")?;
    asm.load(1)?;
    asm.const_int(1)?;
    asm.binary(BinOp::Add, &VerificationType::Integer)?;
    asm.store(1)?;
    asm.branch(Branch::Always, test)?;

    asm.bind(done)?;
    asm.return_(None)
}

/// A branch that does not fit in 16 bits widens, and the JVM runs the result unchanged.
///
/// `goto` becomes `goto_w`; the conditional, which has no wide form, inverts and jumps over a
/// `goto_w` instead. Both rewrites change instruction lengths, so the resolver has to re-measure —
/// this is the test that the fixpoint actually converges on correct offsets.
#[test]
fn a_branch_too_far_for_16_bits_widens() {
    let class = class_with_main("Far", far_loop);
    let (main, _) = code_of(&class, 1);

    let widened = main
        .code
        .iter()
        .filter(|instruction| matches!(instruction, jals_classfile::Instruction::GotoW(_)))
        .count();
    assert_eq!(widened, 2, "both far branches should have widened");
    // The conditional inverted rather than widening in place: `Ge` was emitted, so the body now
    // carries its opposite, jumping over the `goto_w` that does the real work.
    assert!(
        main.code
            .iter()
            .any(|instruction| matches!(instruction, jals_classfile::Instruction::IfIcmplt(8))),
        "the conditional should have inverted and jumped over the goto_w"
    );

    if java_available() {
        assert_eq!(run("Far", &class), "0\n1\n2\n");
    }
}

/// The rest of the emit vocabulary, in one body a JVM can check the answers of:
///
/// ```java
/// total = 7L;                                 // const_long, put_static
/// System.out.println(total);                  // get_static on our own class
/// String absent = null;                       // const_null
/// System.out.println(absent);                 //   -> "null"
/// System.out.println(Math.abs(-5));           // invoke_static
/// System.out.println(1 + 1);                  // dup, so one `iconst_1` feeds both operands
/// System.out.println(List.of().size());       // a static *interface* method, then invoke_interface
/// ```
fn exercise_rest(asm: &mut Assembler<'_>) -> Result<(), jals_javac::jvm::AsmError> {
    asm.const_long(7)?;
    asm.put_static("Extras", "total", "J")?;
    asm.get_static("java/lang/System", "out", SYSTEM_OUT)?;
    asm.get_static("Extras", "total", "J")?;
    asm.invoke_virtual(PRINT_STREAM, "println", "(J)V")?;

    asm.get_static("java/lang/System", "out", SYSTEM_OUT)?;
    asm.const_null()?;
    asm.invoke_virtual(PRINT_STREAM, "println", "(Ljava/lang/String;)V")?;

    asm.get_static("java/lang/System", "out", SYSTEM_OUT)?;
    asm.const_int(-5)?;
    asm.invoke_static("java/lang/Math", "abs", "(I)I", false)?;
    asm.invoke_virtual(PRINT_STREAM, "println", "(I)V")?;

    asm.get_static("java/lang/System", "out", SYSTEM_OUT)?;
    asm.const_int(1)?;
    asm.dup()?;
    asm.binary(BinOp::Add, &VerificationType::Integer)?;
    asm.invoke_virtual(PRINT_STREAM, "println", "(I)V")?;

    asm.get_static("java/lang/System", "out", SYSTEM_OUT)?;
    // `List.of()` is `static` on an *interface*, so its constant must be an `InterfaceMethodRef`
    // even though the instruction is `invokestatic`.
    asm.invoke_static("java/util/List", "of", "()Ljava/util/List;", true)?;
    asm.invoke_interface("java/util/List", "size", "()I")?;
    asm.invoke_virtual(PRINT_STREAM, "println", "(I)V")?;

    asm.return_(None)
}

#[test]
fn the_rest_of_the_emit_vocabulary_runs_on_a_real_jvm() {
    if !java_available() {
        return;
    }
    let class = class_with_statics("Extras", &[("total", "J")], exercise_rest);
    assert_eq!(run("Extras", &class), "7\nnull\n5\n2\n0\n");
}

/// The resolved body, pinned so a change in branch offsets or frame contents is visible.
///
/// Both frames describe `locals: [String[], int]` with an empty stack — the loop's test and its
/// exit are reached with the counter live and nothing pending.
#[test]
fn the_resolved_loop_body_is_pinned() {
    let class = class_with_main("Loop", hello_and_count);
    let (_, rendered) = code_of(&class, 1);
    expect![[r"
        0: GetStatic(19)
        3: Ldc(23)
        5: InvokeVirtual(29)
        8: Iconst0
        9: Istore1
        10: Iload1
        11: Iconst3
        12: IfIcmpge(17)
        15: GetStatic(19)
        18: Iload1
        19: InvokeVirtual(32)
        22: Iload1
        23: Iconst1
        24: Iadd
        25: Istore1
        26: Goto(-16)
        29: Return
        frame Full { offset_delta: 10, locals: [Object { cpool_index: 13 }, Integer], stack: [] }
        frame Full { offset_delta: 18, locals: [Object { cpool_index: 13 }, Integer], stack: [] }
    "]]
    .assert_eq(&rendered);
}

/// `if_icmp*` compares two `int`s, and "not a reference" is not the same predicate: a `long` is not
/// a reference either. Accepting one produced a class the JVM rejected with *"Type `long_2nd` is
/// not assignable to integer"* — a defect the emitter is in a position to catch and the verifier
/// should never have to.
#[test]
fn a_branch_rejects_an_operand_that_is_not_an_int() {
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "(JJ)Z").expect("assembler");
    asm.load(0).expect("lload_0");
    asm.load(2).expect("lload_2");
    let taken = asm.label();
    assert_eq!(
        asm.branch(Branch::IntCmp(Compare::Eq), taken),
        Err(jals_javac::jvm::AsmError::TypeMismatch),
        "two `long`s are not an `if_icmpeq`"
    );
}

/// The reference forms still accept any reference, which is the other half of the same check.
#[test]
fn a_reference_branch_still_accepts_any_reference() {
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm =
        Assembler::new(&mut pool, Receiver::Static, "([Ljava/lang/String;)V").expect("assembler");
    asm.load(0).expect("aload_0");
    let taken = asm.label();
    asm.branch(Branch::RefNull(true), taken)
        .expect("`ifnull` over a `String[]`");
    asm.return_(None).expect("return");
    asm.bind(taken).expect("bind");
    asm.return_(None).expect("return");
    asm.finish().expect("finish");
}

/// A back edge may bring locals the loop head never described — that is what a body declaring its
/// own variables does — but it may not take away one the head *did* describe. The code after the
/// label was emitted against that frame.
#[test]
fn a_back_edge_may_add_slots_but_not_lose_them() {
    // Adding: the head describes slot 0, the body writes slot 1, and the arrival still describes
    // slot 0.
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "(I)V").expect("assembler");
    let head = asm.label();
    asm.bind(head).expect("bind");
    asm.const_int(0).expect("iconst_0");
    asm.store(1).expect("istore_1");
    asm.branch(Branch::Always, head).expect("back edge");
    asm.finish().expect("a wider arrival is not a conflict");

    // Losing: the head describes slot 0 as an `int`, and the arrival has overwritten it with a
    // reference, so the merge can keep neither.
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "(I)V").expect("assembler");
    let head = asm.label();
    asm.bind(head).expect("bind");
    asm.const_null().expect("aconst_null");
    asm.store(0).expect("astore_0");
    assert_eq!(
        asm.branch(Branch::Always, head),
        Err(jals_javac::jvm::AsmError::IncompatibleFrame),
        "slot 0 stopped being the `int` the head's frame describes"
    );
}

// --- the primitives a full lowering needs -----------------------------------
//
// Everything below emits directly against the assembler and hands the result to a real JVM. That is
// the only authority on whether a `StackMapTable` describes the code it covers, and it is a much
// stronger check than a reparse: a wrong frame, a wrong branch offset, or a wrong `switch` padding
// all reparse perfectly and then fail to load.

/// Push `System.out`, ready for a `println`.
fn out(asm: &mut Assembler<'_>) -> Result<(), AsmError> {
    asm.get_static("java/lang/System", "out", SYSTEM_OUT)
}

/// `System.out.println(<whatever `value` leaves on the stack>)` at `descriptor`.
fn println(
    asm: &mut Assembler<'_>,
    descriptor: &str,
    value: impl FnOnce(&mut Assembler<'_>) -> Result<(), AsmError>,
) -> Result<(), AsmError> {
    out(asm)?;
    value(asm)?;
    asm.invoke_virtual(PRINT_STREAM, "println", descriptor)
}

/// The conversion opcodes, with a real JVM checking what they compute.
///
/// The narrowing cases are the ones worth running. JLS §5.1.3 defines `double`-to-`int` as truncation
/// toward zero and `int`-to-`byte` as taking the low eight bits *signed*, and `byte`-to-`char` needs
/// an `i2c` even though both types live on the stack as `int` — a signed byte's negative half has no
/// place in an unsigned `char`.
#[test]
fn the_conversions_run_on_a_real_jvm() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Convert", |asm| {
        // `(long) 7 * 1000000000L` — an `i2l` is what lets a `long` multiply see an `int` literal.
        println(asm, "(J)V", |asm| {
            asm.const_int(7)?;
            asm.convert(Numeric::Int, Numeric::Long)?;
            asm.const_long(1_000_000_000)?;
            asm.binary(BinOp::Mul, &VerificationType::Long)
        })?;
        // `(int) -3.99` truncates toward zero, not down.
        println(asm, "(I)V", |asm| {
            asm.const_double(-3.99)?;
            asm.convert(Numeric::Double, Numeric::Int)
        })?;
        // `(byte) 200` is -56: the low eight bits, read signed.
        println(asm, "(I)V", |asm| {
            asm.const_int(200)?;
            asm.convert(Numeric::Int, Numeric::Byte)
        })?;
        // `(char) (byte) -1` is 65535, which is the `i2c` that a same-stack-type conversion still
        // needs.
        println(asm, "(I)V", |asm| {
            asm.const_int(-1)?;
            asm.convert(Numeric::Byte, Numeric::Char)
        })?;
        // `(long) 3.5f` goes through `f2l` in one step.
        println(asm, "(J)V", |asm| {
            asm.const_float(3.5)?;
            asm.convert(Numeric::Float, Numeric::Long)
        })?;
        // `(byte) 300L` is two steps — `l2i` then `i2b` — because there is no single opcode.
        println(asm, "(I)V", |asm| {
            asm.const_long(300)?;
            asm.convert(Numeric::Long, Numeric::Byte)
        })?;
        asm.return_(None)
    });
    assert_eq!(
        run("Convert", &class),
        "7000000000\n-3\n-56\n65535\n3\n44\n"
    );
}

/// A widening between two integral types narrower than `int` emits nothing at all: the value is
/// already in the target's range, and an `i2s` there would be noise in every `byte` expression.
#[test]
fn a_widening_among_the_narrow_integrals_emits_nothing() {
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "(B)V").expect("assembler");
    asm.load(0).expect("iload_0");
    asm.convert(Numeric::Byte, Numeric::Short).expect("b -> s");
    asm.convert(Numeric::Short, Numeric::Int).expect("s -> i");
    asm.convert(Numeric::Int, Numeric::Int).expect("i -> i");
    asm.pop().expect("pop");
    asm.return_(None).expect("return");
    let AttributeBody::Code(code) = asm.finish().expect("finish").body else {
        panic!("a Code attribute");
    };
    // `iload_0`, `pop`, `return` — and nothing between them.
    assert_eq!(code.code.len(), 3, "{:?}", code.code);
}

/// The bitwise and shift families, including the two that differ only in the sign bit.
#[test]
fn the_bitwise_and_shift_families_run() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Bits", |asm| {
        for (op, left, right) in [
            (BinOp::And, 12, 10),
            (BinOp::Or, 12, 10),
            (BinOp::Xor, 12, 10),
        ] {
            println(asm, "(I)V", |asm| {
                asm.const_int(left)?;
                asm.const_int(right)?;
                asm.binary(op, &VerificationType::Integer)
            })?;
        }
        // `-8 >> 1` keeps the sign and gives -4; `-8 >>> 28` does not and gives 15.
        println(asm, "(I)V", |asm| {
            asm.const_int(-8)?;
            asm.const_int(1)?;
            asm.binary(BinOp::Shr, &VerificationType::Integer)
        })?;
        println(asm, "(I)V", |asm| {
            asm.const_int(-8)?;
            asm.const_int(28)?;
            asm.binary(BinOp::Ushr, &VerificationType::Integer)
        })?;
        // `1L << 40` is the asymmetric one: `lshl` shifts a `long` by an **`int`**.
        println(asm, "(J)V", |asm| {
            asm.const_long(1)?;
            asm.const_int(40)?;
            asm.binary(BinOp::Shl, &VerificationType::Long)
        })?;
        asm.return_(None)
    });
    assert_eq!(run("Bits", &class), "8\n14\n6\n-4\n15\n1099511627776\n");
}

/// A shift's right operand is an `int` whatever the left one is. Handing `binary` two `long`s for an
/// `lshl` used to typecheck here and produce a class the verifier rejects.
#[test]
fn a_long_shift_takes_an_int_count() {
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "(JJ)V").expect("assembler");
    asm.load(0).expect("lload_0");
    asm.load(2).expect("lload_2");
    assert_eq!(
        asm.binary(BinOp::Shl, &VerificationType::Long),
        Err(AsmError::TypeMismatch),
        "`lshl` does not shift by a `long`"
    );
}

/// Materialise `left <cmp> right` as a `boolean` and print it.
fn print_comparison(
    asm: &mut Assembler<'_>,
    ty: &VerificationType,
    compare: Compare,
    left: impl FnOnce(&mut Assembler<'_>) -> Result<(), AsmError>,
    right: impl FnOnce(&mut Assembler<'_>) -> Result<(), AsmError>,
) -> Result<(), AsmError> {
    out(asm)?;
    left(asm)?;
    right(asm)?;
    let taken = asm.label();
    let done = asm.label();
    asm.branch_compare(ty, compare, taken)?;
    asm.const_int(0)?;
    asm.branch(Branch::Always, done)?;
    asm.bind(taken)?;
    asm.const_int(1)?;
    asm.bind(done)?;
    asm.invoke_virtual(PRINT_STREAM, "println", "(Z)V")
}

/// A `long` / `float` / `double` comparison goes through `lcmp` / `fcmp?` / `dcmp?`, and a reference
/// one through `if_acmp*`.
///
/// **The NaN cases are the point.** JLS §15.20.1 makes every ordering comparison involving a NaN
/// false, in *both* directions — so `nan < 1` and `nan > 1` are both false, and so are `nan <= 1` and
/// `nan >= 1`. That only holds if `<` and `<=` reduce with `fcmpg` (NaN yields 1, which is not below
/// zero) and `>` / `>=` with `fcmpl` (NaN yields -1, which is not above it). Swapping the two
/// produces a comparison that is *true* for NaN, in a class file that verifies and loads.
#[test]
fn the_wide_and_reference_comparisons_run() {
    if !java_available() {
        return;
    }
    let nan = |asm: &mut Assembler<'_>| {
        asm.const_double(0.0)?;
        asm.const_double(0.0)?;
        asm.binary(BinOp::Div, &VerificationType::Double)
    };
    let class = class_with_main("Compare", |asm| {
        // `2L > 1L` and `1L > 2L`.
        print_comparison(
            asm,
            &VerificationType::Long,
            Compare::Gt,
            |asm| asm.const_long(2),
            |asm| asm.const_long(1),
        )?;
        print_comparison(
            asm,
            &VerificationType::Long,
            Compare::Gt,
            |asm| asm.const_long(1),
            |asm| asm.const_long(2),
        )?;
        // `1.5f < 2.5f`.
        print_comparison(
            asm,
            &VerificationType::Float,
            Compare::Lt,
            |asm| asm.const_float(1.5),
            |asm| asm.const_float(2.5),
        )?;
        // Every one of these must be false.
        for compare in [Compare::Lt, Compare::Le, Compare::Gt, Compare::Ge] {
            print_comparison(asm, &VerificationType::Double, compare, nan, |asm| {
                asm.const_double(1.0)
            })?;
        }
        // `args != null`, which is an `if_acmpne` against `aconst_null`.
        print_comparison(
            asm,
            &VerificationType::Null,
            Compare::Ne,
            |asm| asm.load(0),
            |asm: &mut Assembler<'_>| asm.const_null(),
        )?;
        asm.return_(None)
    });
    assert_eq!(
        run("Compare", &class),
        "true\nfalse\ntrue\nfalse\nfalse\nfalse\nfalse\ntrue\n"
    );
}

/// Ordering two references is not a Java program, so it is a generator bug rather than an emission.
#[test]
fn a_reference_ordering_is_reported() {
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm =
        Assembler::new(&mut pool, Receiver::Static, "([Ljava/lang/String;)V").expect("assembler");
    asm.load(0).expect("aload_0");
    asm.load(0).expect("aload_0");
    let taken = asm.label();
    assert_eq!(
        asm.branch_compare(&VerificationType::Null, Compare::Lt, taken),
        Err(AsmError::TypeMismatch),
        "`<` is not defined over two references"
    );
}

/// `iinc` reads and writes a local without touching the operand stack, which is why `i++` on a local
/// is one instruction.
#[test]
fn iinc_updates_a_local_in_place() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Increment", |asm| {
        asm.const_int(40)?;
        asm.store(1)?;
        asm.increment(1, 2)?;
        asm.increment(1, -1)?;
        println(asm, "(I)V", |asm| asm.load(1))?;
        asm.return_(None)
    });
    assert_eq!(run("Increment", &class), "41\n");

    // It is an `int` instruction, and a wider local is not one.
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "(J)V").expect("assembler");
    assert_eq!(
        asm.increment(0, 1),
        Err(AsmError::TypeMismatch),
        "`iinc` does not increment a `long`"
    );
    assert_eq!(
        asm.increment(4, 1),
        Err(AsmError::UnwrittenLocal),
        "nothing has written slot 4"
    );
}

/// The `dup` family, in the shape an assignment *expression* needs it: the value has to survive the
/// store, and it starts out above the receiver (or above the array and index) rather than alone.
#[test]
fn the_dup_family_reseats_a_value_under_its_target() {
    if !java_available() {
        return;
    }
    let class = class_with_statics("Dup", &[("slot", "I"), ("wide", "J")], |asm| {
        // `println(slot = 7)` — `dup_x0` is just `dup`, but the *static* case has nothing under it.
        println(asm, "(I)V", |asm| {
            asm.const_int(7)?;
            asm.dup()?;
            asm.put_static("Dup", "slot", "I")
        })?;
        // A wide value duplicates as `dup2`.
        println(asm, "(J)V", |asm| {
            asm.const_long(9)?;
            asm.dup()?;
            asm.put_static("Dup", "wide", "J")
        })?;
        // `println(a[1] = 5)`: the copy goes under the array and the index, which is `dup_x2`.
        println(asm, "(I)V", |asm| {
            asm.const_int(2)?;
            asm.new_array("I")?;
            asm.const_int(1)?;
            asm.const_int(5)?;
            asm.dup_below(2)?;
            asm.array_store("I")
        })?;
        // `a[0] += 4` reads and writes one `(array, index)` pair, which `dup_pair` computes once.
        println(asm, "(I)V", |asm| {
            asm.const_int(1)?;
            asm.new_array("I")?;
            asm.const_int(0)?;
            asm.dup_pair()?;
            asm.array_load("I")?;
            asm.const_int(4)?;
            asm.binary(BinOp::Add, &VerificationType::Integer)?;
            asm.dup_below(2)?;
            asm.array_store("I")
        })?;
        // `swap`, checked by an order the printer would otherwise hide.
        println(asm, "(I)V", |asm| {
            asm.const_int(3)?;
            asm.const_int(10)?;
            asm.swap()?;
            asm.binary(BinOp::Sub, &VerificationType::Integer)
        })?;
        asm.return_(None)
    });
    assert_eq!(run("Dup", &class), "7\n9\n5\n4\n7\n");
}

/// A `dup` that would land inside a wide value has no opcode, and `swap` has no `swap2`.
#[test]
fn a_dup_across_half_a_wide_value_is_reported() {
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "(JI)V").expect("assembler");
    asm.load(0).expect("lload_0");
    asm.load(2).expect("iload_2");
    assert_eq!(
        asm.dup_below(1),
        Err(AsmError::TypeMismatch),
        "`dup_x1` cannot reach over one half of a `long`"
    );

    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "(JJ)V").expect("assembler");
    asm.load(0).expect("lload_0");
    asm.load(2).expect("lload_2");
    assert_eq!(
        asm.swap(),
        Err(AsmError::TypeMismatch),
        "the JVM has no `swap2`"
    );
}

/// Arrays: allocated, written, read back, and measured — for a primitive element and a reference
/// one, whose `Class` entry is spelled differently.
#[test]
fn arrays_are_allocated_read_and_measured() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Arrays", |asm| {
        // `int[] a = new int[3]; a[2] = 42;`
        asm.const_int(3)?;
        asm.new_array("I")?;
        asm.store(1)?;
        asm.load(1)?;
        asm.const_int(2)?;
        asm.const_int(42)?;
        asm.array_store("I")?;
        println(asm, "(I)V", |asm| {
            asm.load(1)?;
            asm.const_int(2)?;
            asm.array_load("I")
        })?;
        println(asm, "(I)V", |asm| {
            asm.load(1)?;
            asm.array_length()
        })?;

        // A reference element takes `anewarray` over a `Class` entry, and `aaload` back out.
        asm.const_int(1)?;
        asm.new_array("Ljava/lang/String;")?;
        asm.store(2)?;
        asm.load(2)?;
        asm.const_int(0)?;
        asm.const_string("boxed")?;
        asm.array_store("Ljava/lang/String;")?;
        println(asm, "(Ljava/lang/String;)V", |asm| {
            asm.load(2)?;
            asm.const_int(0)?;
            asm.array_load("Ljava/lang/String;")
        })?;

        // `byte[]` and `boolean[]` share `bastore`, and `char[]` sign-extends differently from
        // `short[]` — the four narrow element types are where an array opcode table goes wrong.
        asm.const_int(1)?;
        asm.new_array("C")?;
        asm.store(3)?;
        asm.load(3)?;
        asm.const_int(0)?;
        asm.const_int(65_535)?;
        asm.array_store("C")?;
        println(asm, "(I)V", |asm| {
            asm.load(3)?;
            asm.const_int(0)?;
            asm.array_load("C")
        })?;

        // `new int[2][3]` allocates both levels at once.
        asm.const_int(2)?;
        asm.const_int(3)?;
        asm.new_multi_array("[[I", 2)?;
        asm.store(4)?;
        println(asm, "(I)V", |asm| {
            asm.load(4)?;
            asm.const_int(1)?;
            asm.array_load("[I")?;
            asm.array_length()
        })?;
        asm.return_(None)
    });
    assert_eq!(run("Arrays", &class), "42\n3\nboxed\n65535\n3\n");
}

/// `new` leaves an `uninitialized(offset)` naming *the offset of the `new` itself*, and the offset
/// does not exist until branch widening has run.
///
/// A frame carries that type only where an uninitialised value is live across a branch — which is
/// exactly what an argument expression with a conditional in it produces, and what javac emits for
/// `new StringBuilder(c ? "a" : "b")`. A JVM verifying this method is the only check that the marker
/// was translated back to a real offset: a wrong one reparses perfectly and then fails to load.
#[test]
fn an_uninitialized_reference_survives_a_branch() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Fresh", |asm| {
        out(asm)?;
        asm.new_object("java/lang/StringBuilder")?;
        asm.dup()?;
        // The argument's own value is chosen by a branch, with two uninitialised references already
        // on the stack under it.
        let empty = asm.label();
        let chosen = asm.label();
        asm.load(0)?;
        asm.array_length()?;
        asm.branch(Branch::IntZero(Compare::Eq), empty)?;
        asm.const_string("args:")?;
        asm.branch(Branch::Always, chosen)?;
        asm.bind(empty)?;
        asm.const_string("none:")?;
        asm.bind(chosen)?;
        asm.invoke_special(
            "java/lang/StringBuilder",
            "<init>",
            "(Ljava/lang/String;)V",
            false,
        )?;
        asm.load(0)?;
        asm.array_length()?;
        asm.invoke_virtual(
            "java/lang/StringBuilder",
            "append",
            "(I)Ljava/lang/StringBuilder;",
        )?;
        asm.invoke_virtual(
            "java/lang/StringBuilder",
            "toString",
            "()Ljava/lang/String;",
        )?;
        asm.invoke_virtual(PRINT_STREAM, "println", "(Ljava/lang/String;)V")?;
        asm.return_(None)
    });

    // The frames really do describe two uninitialised references, and at the `new`'s own offset.
    let (_, rendered) = code_of(&class, 1);
    assert!(
        rendered.contains("Uninitialized { offset: 3 }"),
        "the marker was not translated to the `new`'s offset:\n{rendered}"
    );
    assert_eq!(run("Fresh", &class), "none:0\n");
}

/// `checkcast` and `instanceof`, whose operand is a `Class` entry spelled as an internal name for a
/// class and as a *descriptor* for an array.
#[test]
fn a_cast_and_a_type_test_run() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Cast", |asm| {
        println(asm, "(Z)V", |asm| {
            asm.load(0)?;
            asm.instance_of("java/lang/Object")
        })?;
        println(asm, "(Z)V", |asm| {
            asm.load(0)?;
            asm.instance_of("[Ljava/lang/Integer;")
        })?;
        // A widening store and a narrowing read back, which is what a cast is for.
        asm.load(0)?;
        asm.store(1)?;
        println(asm, "(I)V", |asm| {
            asm.load(1)?;
            asm.check_cast("[Ljava/lang/String;")?;
            asm.array_length()
        })?;
        asm.return_(None)
    });
    assert_eq!(run("Cast", &class), "true\nfalse\n0\n");
}

/// A thrown exception, caught by a handler whose frame nothing jumps to.
///
/// The handler's entry state is *given* rather than merged: control arrives on an edge the JVM
/// supplies, so no `branch` ever records it, and `bind` would report a label control cannot reach.
/// Getting the exception table's `end_pc` wrong, or omitting the handler's frame, both produce a
/// class that loads and then fails verification — so a JVM running this is the check.
#[test]
fn a_thrown_exception_reaches_its_handler() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Thrown", |asm| {
        let start = asm.label();
        let end = asm.label();
        let handler = asm.label();
        let after = asm.label();

        // Written *before* the range starts, which is what makes it readable in the handler: a local
        // the range itself assigns might not have been reached when the throw happened, and Java's
        // definite-assignment rules refuse to read it there for the same reason.
        asm.const_string("before")?;
        asm.store(1)?;

        asm.bind(start)?;
        asm.new_object("java/lang/IllegalStateException")?;
        asm.dup()?;
        asm.const_string("boom")?;
        asm.invoke_special(
            "java/lang/IllegalStateException",
            "<init>",
            "(Ljava/lang/String;)V",
            false,
        )?;
        asm.throw()?;
        // The range ends where the protected code does. Nothing arrives here and nothing jumps
        // here — it is an offset in the exception table and nothing else.
        asm.mark(end)?;

        asm.bind_handler(handler, start, "java/lang/IllegalStateException")?;
        asm.protect(start, end, handler, Some("java/lang/IllegalStateException"))?;
        asm.invoke_virtual(
            "java/lang/IllegalStateException",
            "getMessage",
            "()Ljava/lang/String;",
        )?;
        asm.store(2)?;
        println(asm, "(Ljava/lang/String;)V", |asm| asm.load(2))?;
        // Slot 1 is still readable here, because the handler's frame keeps the locals the protected
        // range started with rather than an empty set.
        println(asm, "(Ljava/lang/String;)V", |asm| asm.load(1))?;
        asm.branch(Branch::Always, after)?;

        asm.bind(after)?;
        asm.return_(None)
    });
    let (code, _) = code_of(&class, 1);
    assert_eq!(code.exception_table.len(), 1);
    assert_eq!(run("Thrown", &class), "boom\nbefore\n");
}

/// A `finally`'s catch-all is `catch_type` 0, and its handler still receives a `Throwable`.
#[test]
fn a_catch_all_handler_runs_for_any_throwable() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Finally", |asm| {
        let start = asm.label();
        let end = asm.label();
        let handler = asm.label();

        asm.bind(start)?;
        asm.const_int(1)?;
        asm.const_int(0)?;
        // An `ArithmeticException` nothing in the source mentions, which is what a catch-all is for.
        asm.binary(BinOp::Div, &VerificationType::Integer)?;
        asm.pop()?;
        asm.mark(end)?;
        asm.return_(None)?;

        asm.bind_handler(handler, start, "java/lang/Throwable")?;
        asm.protect(start, end, handler, None)?;
        asm.pop()?;
        println(asm, "(Ljava/lang/String;)V", |asm| {
            asm.const_string("cleaned up")
        })?;
        asm.return_(None)
    });
    assert_eq!(run("Finally", &class), "cleaned up\n");
}

/// An empty protected range is dropped rather than written.
///
/// It protects nothing, and the JVM would carry the entry anyway. It is also not an emitter mistake:
/// a `finally` splits its protected range at every inlined copy of the block, so a `try` whose last
/// statement is a `return` closes one range at exactly the offset the next one opens at.
#[test]
fn an_empty_protected_range_is_dropped() {
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    pool.class_index("java/lang/Throwable").expect("Throwable");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "()V").expect("assembler");
    let start = asm.label();
    let end = asm.label();
    let handler = asm.label();
    asm.bind(start).expect("bind");
    asm.mark(end).expect("mark");
    asm.return_(None).expect("return");
    asm.bind_handler(handler, start, "java/lang/Throwable")
        .expect("handler");
    asm.protect(start, end, handler, None).expect("protect");
    asm.pop().expect("pop");
    asm.return_(None).expect("return");
    let AttributeBody::Code(code) = asm.finish().expect("finish").body else {
        panic!("a Code attribute");
    };
    assert!(
        code.exception_table.is_empty(),
        "the range covers no instruction, so there is nothing to protect"
    );
}

/// Both `switch` forms, at all four alignments.
///
/// The operands of a `tableswitch` and a `lookupswitch` start at a four-byte boundary *measured from
/// the start of the method*, so an instruction's length depends on where it sits — the one place in
/// this assembler where that is true. Moving one can also make it **shorter**, which is why the
/// widening fixpoint's termination argument is about the set of widened branches rather than about
/// lengths. Four alignments in one verified method is what says the padding and the offsets agree.
#[test]
fn both_switch_forms_run_at_every_alignment() {
    if !java_available() {
        return;
    }
    // `const_int(0); pop()` is two bytes and `const_int(100); pop()` is three, so 0 / 2 / 3 / 5
    // bytes of filler cover all four residues.
    let filler = |asm: &mut Assembler<'_>, bytes: usize| -> Result<(), AsmError> {
        let (twos, threes) = match bytes {
            0 => (0, 0),
            2 => (1, 0),
            3 => (0, 1),
            _ => (1, 1),
        };
        for _ in 0..twos {
            asm.const_int(0)?;
            asm.pop()?;
        }
        for _ in 0..threes {
            asm.const_int(100)?;
            asm.pop()?;
        }
        Ok(())
    };
    let class = class_with_main("Switch", |asm| {
        for (offset, bytes) in [0usize, 2, 3, 5].into_iter().enumerate() {
            filler(asm, bytes)?;
            // Dense keys take the table form; a key 1000 away takes the lookup form. Both are
            // driven by the same `switch` call, and both are emitted at this alignment.
            for keys in [&[0i32, 1, 2][..], &[0, 1000][..]] {
                out(asm)?;
                asm.const_int(i32::try_from(offset).unwrap_or(0))?;
                let arms: Vec<_> = keys.iter().map(|&key| (key, asm.label())).collect();
                let default = asm.label();
                let done = asm.label();
                asm.switch(&arms, default)?;
                for (key, label) in arms {
                    asm.bind(label)?;
                    asm.const_int(key)?;
                    asm.branch(Branch::Always, done)?;
                }
                asm.bind(default)?;
                asm.const_int(-1)?;
                asm.bind(done)?;
                asm.invoke_virtual(PRINT_STREAM, "println", "(I)V")?;
            }
        }
        asm.return_(None)
    });
    let (_, rendered) = code_of(&class, 1);
    assert!(
        rendered.contains("TableSwitch") && rendered.contains("LookupSwitch"),
        "both forms should have been chosen:\n{rendered}"
    );
    // Key 0 / 1 / 2 hit an arm; key 3 falls through to `default`. The sparse switch only claims 0.
    assert_eq!(run("Switch", &class), "0\n0\n1\n-1\n2\n-1\n-1\n-1\n");
}

/// A key inside a `tableswitch`'s span that no arm claims goes to `default`. That is what lets the
/// dense form cover a span with holes in it at all.
#[test]
fn a_table_switch_fills_its_holes_with_the_default() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Holes", |asm| {
        for key in 0..5 {
            out(asm)?;
            asm.const_int(key)?;
            let arms = [(0, asm.label()), (2, asm.label()), (4, asm.label())];
            let default = asm.label();
            let done = asm.label();
            asm.switch(&arms, default)?;
            for (key, label) in arms {
                asm.bind(label)?;
                asm.const_int(key * 10)?;
                asm.branch(Branch::Always, done)?;
            }
            asm.bind(default)?;
            asm.const_int(-1)?;
            asm.bind(done)?;
            asm.invoke_virtual(PRINT_STREAM, "println", "(I)V")?;
        }
        asm.return_(None)
    });
    let (_, rendered) = code_of(&class, 1);
    assert!(
        rendered.contains("TableSwitch"),
        "keys 0 / 2 / 4 are dense enough for the table form:\n{rendered}"
    );
    assert_eq!(run("Holes", &class), "0\n-1\n20\n-1\n40\n");
}

/// Two arms on one key would make the jump ambiguous — and `tableswitch`, which indexes rather than
/// searches, would silently keep only one of them.
#[test]
fn a_duplicated_switch_key_is_reported() {
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "(I)V").expect("assembler");
    asm.load(0).expect("iload_0");
    let arms = [(1, asm.label()), (1, asm.label())];
    let default = asm.label();
    assert_eq!(
        asm.switch(&arms, default),
        Err(AsmError::DuplicateCase),
        "one key cannot have two arms"
    );
}

/// A local slot past 255 takes the `wide` prefix. A method with that many is legal, and reachable
/// rather than theoretical: slots are never reused here, so `max_locals` counts every declaration
/// in the body rather than the widest live set.
#[test]
fn a_local_slot_past_a_byte_takes_the_wide_form() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Wide", |asm| {
        for slot in 1..=300u16 {
            asm.const_int(i32::from(slot))?;
            asm.store(slot)?;
        }
        asm.increment(300, 1)?;
        println(asm, "(I)V", |asm| asm.load(300))?;
        asm.return_(None)
    });
    let (code, rendered) = code_of(&class, 1);
    assert_eq!(code.max_locals, 301);
    assert!(
        rendered.contains("Wide(Istore(300))") && rendered.contains("Wide(Iinc { index: 300"),
        "slot 300 needs the wide forms:\n{}",
        &rendered[rendered.len().saturating_sub(400)..]
    );
    assert_eq!(run("Wide", &class), "301\n");
}

/// `monitorenter` / `monitorexit`, balanced — the JVM refuses to return from a method still holding
/// a monitor it took.
#[test]
fn a_monitor_is_taken_and_released() {
    if !java_available() {
        return;
    }
    let class = class_with_main("Monitor", |asm| {
        asm.load(0)?;
        asm.store(1)?;
        asm.load(1)?;
        asm.monitor_enter()?;
        println(asm, "(Ljava/lang/String;)V", |asm| {
            asm.const_string("locked")
        })?;
        asm.load(1)?;
        asm.monitor_exit()?;
        asm.return_(None)
    });
    assert_eq!(run("Monitor", &class), "locked\n");
}

/// `invokedynamic` names no owner, and its descriptor is what the stack effect comes from.
///
/// The call site names only itself, its descriptor, and which `BootstrapMethods` entry computes the handle
/// it will call — which is what lets one bootstrap serve every site of the same shape. The assembler treats
/// it as an invocation with no receiver, so a `()Lp/Iface;` site leaves one reference and nothing else.
#[test]
fn an_invokedynamic_leaves_what_its_descriptor_says() {
    let mut pool = ConstantPool::new();
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "()Lp/Iface;").expect("assembler");
    asm.invoke_dynamic(0, "run", "()Lp/Iface;")
        .expect("call site");
    let top = asm.stack_top().expect("a value");
    asm.return_(Some(&top)).expect("return");
    let code = asm.finish().expect("finish");
    let jals_classfile::AttributeBody::Code(body) = &code.body else {
        panic!("a Code attribute");
    };
    // One `invokedynamic` and one `areturn`, and a frame deep enough for the reference it left.
    assert_eq!(body.max_stack, 1);
    assert!(
        body.code.iter().any(|instruction| matches!(
            instruction,
            jals_classfile::Instruction::InvokeDynamic { .. }
        )),
        "the call site is an `invokedynamic`: {:?}",
        body.code
    );
}

/// A slot written through `store_as` keeps its *declared* type, and that is what the frames say.
///
/// `store` types a slot by the value put in it, which is right for a slot the lowering took for
/// itself and wrong for one the source declared: a `String` assigned to an `Object` local leaves an
/// `Object` behind. Nothing else can restore that, because the widening happens at the assignment
/// and the assembler has no hierarchy to widen with later.
///
/// Both places that have to agree are checked here. A backward jump merges against the header's
/// frame, which `store` alone made a `String` the reassignment then contradicted; an exception
/// handler is given the range's start state, which it would have described the same wrong way.
#[test]
fn a_declared_slot_keeps_its_declared_type() {
    let object = "Ljava/lang/Object;";

    // A loop header describing slot 1 as `Object`: the back edge writes a `String` into it, and a
    // declared slot merges cleanly because both sides call it an `Object`.
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "()V").expect("assembler");
    asm.const_null().expect("aconst_null");
    asm.store_as(1, object).expect("astore_1");
    let head = asm.label();
    asm.bind(head).expect("bind");
    asm.const_string("s").expect("ldc");
    asm.store_as(1, object).expect("astore_1");
    asm.branch(Branch::Always, head).expect("back edge");
    asm.finish()
        .expect("both sides describe slot 1 as `Object`");

    // The same shape through `store`, which types the slot by the value: the header saw `Object`
    // (from `null`) and the back edge arrives with a `String`, so the merge loses the slot — and a
    // slot the frame described may not stop being described.
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "()V").expect("assembler");
    asm.const_string("a").expect("ldc");
    asm.store(1).expect("astore_1");
    let head = asm.label();
    asm.bind(head).expect("bind");
    asm.new_object("java/lang/Object").expect("new");
    asm.dup().expect("dup");
    asm.invoke_special("java/lang/Object", "<init>", "()V", false)
        .expect("<init>");
    asm.store(1).expect("astore_1");
    assert_eq!(
        asm.branch(Branch::Always, head),
        Err(AsmError::IncompatibleFrame),
        "slot 1 stopped being the `String` the header's frame describes"
    );

    // A declared slot reassigned inside a protected range: the handler's frame is the range's start
    // state, so the declared type is the only one that covers every instruction in between.
    let mut pool = ConstantPool::new();
    pool.class_index("java/lang/Object").expect("Object");
    let mut asm = Assembler::new(&mut pool, Receiver::Static, "()V").expect("assembler");
    asm.const_string("start").expect("ldc");
    asm.store_as(1, object).expect("astore_1");
    let (start, end, handler) = (asm.label(), asm.label(), asm.label());
    asm.bind(start).expect("bind");
    asm.new_object("java/lang/Object").expect("new");
    asm.dup().expect("dup");
    asm.invoke_special("java/lang/Object", "<init>", "()V", false)
        .expect("<init>");
    asm.store_as(1, object).expect("astore_1");
    asm.mark(end).expect("mark");
    asm.bind_handler(handler, start, "java/lang/Throwable")
        .expect("handler");
    asm.protect(start, end, handler, None).expect("protect");
    asm.pop().expect("pop the caught reference");
    asm.return_(None).expect("return");
    let code = asm.finish().expect("the handler frame covers the range");
    let AttributeBody::Code(body) = &code.body else {
        panic!("`finish` builds a `Code` attribute");
    };
    let frame = body
        .attributes
        .iter()
        .find_map(|attribute| match &attribute.body {
            AttributeBody::StackMapTable(frames) => frames.last(),
            _ => None,
        })
        .expect("a stack map frame");
    let jals_classfile::StackMapFrame::Full { locals, .. } = frame else {
        panic!("every frame is written as a `full_frame`");
    };
    assert_eq!(
        locals[1],
        VerificationType::Object {
            cpool_index: pool.class_index("java/lang/Object").expect("Object"),
        },
        "the handler describes slot 1 by its declaration, not by the first value written"
    );
}
