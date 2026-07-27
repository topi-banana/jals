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
use jals_javac::jvm::{Assembler, BinOp, Branch, Compare, Receiver};

/// Java 25, matching the class files the rest of the workspace pins its fixtures to.
const MAJOR_JAVA_25: u16 = 69;

const PRINT_STREAM: &str = "java/io/PrintStream";
const SYSTEM_OUT: &str = "Ljava/io/PrintStream;";

fn java_available() -> bool {
    Command::new("java")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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
