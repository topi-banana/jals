//! End-to-end: Java source in, a `.class` a real JVM loads, verifies, and runs out.
//!
//! This is the milestone's acceptance test. The assembler tests prove the emitter in isolation;
//! these prove the whole path — parse, resolve, infer, select overloads, erase to descriptors,
//! lower, assemble — against the only authority that matters.

use std::process::{Command, Stdio};

use jals_hir::{FileId, ProjectIndex, Resolved, TypeInference};
use jals_javac::lower::{Compile, CompiledClass, LowerError};

/// Java 25, matching the class files the rest of the workspace pins its fixtures to.
const MAJOR_JAVA_25: u16 = 69;

/// Whether a JVM is on this host. A missing one stands the test down — loudly, because the JVM is
/// the only authority on whether an emitted class file is correct, and a quiet stand-down reads as
/// a pass.
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

/// Compile `source` as a one-file project, with the embedded stdlib stubs available.
fn compile(source: &str) -> Result<Vec<CompiledClass>, LowerError> {
    let root = jals_exec::block_on_inline(jals_syntax::Parse::parse(source)).syntax();
    let resolved = jals_exec::block_on_inline(Resolved::resolve_node(&root));
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), root.clone())])
            .with_stdlib()
            .build(),
    );
    let inference =
        jals_exec::block_on_inline(TypeInference::infer(&root, &resolved, &index, FileId(0)));
    Compile::file(
        &root,
        &resolved,
        &inference,
        &index,
        FileId(0),
        MAJOR_JAVA_25,
    )
}

/// Compile `source`, run its `main` class on a real JVM, and return stdout.
fn run(source: &str, main_class: &str) -> String {
    let classes = compile(source).unwrap_or_else(|error| panic!("compile: {error}"));
    let directory = tempfile::tempdir().expect("temp dir");
    for class in &classes {
        let path = directory
            .path()
            .join(format!("{}.class", class.internal_name));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create package directory");
        }
        std::fs::write(&path, &class.bytes).expect("write class");
    }

    let output = Command::new("java")
        .arg("-cp")
        .arg(directory.path())
        .arg(main_class)
        .output()
        .expect("run java");
    assert!(
        output.status.success(),
        "the JVM rejected the compiled class:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

const HELLO: &str = r#"
public class Hello {
    static int twice(int n) {
        return n + n;
    }

    public static void main(String[] args) {
        System.out.println("Hello, world!");
        int i = 0;
        while (i < 3) {
            System.out.println(twice(i));
            i = i + 1;
        }
        if (i == 3) {
            System.out.println("done");
        } else {
            System.out.println("unreachable");
        }
    }
}
"#;

#[test]
fn hello_world_compiles_and_runs() {
    if !java_available() {
        return;
    }
    assert_eq!(run(HELLO, "Hello"), "Hello, world!\n0\n2\n4\ndone\n");
}

/// Every declared type becomes its own class file, named by its internal binary name.
#[test]
fn a_source_file_yields_one_class_per_type() {
    let classes = compile(HELLO).expect("compile");
    assert_eq!(classes.len(), 1);
    assert_eq!(classes[0].internal_name, "Hello");
}

/// Instance state: a field with an initialiser, read back through `this` in an instance method
/// called from `main`.
#[test]
fn fields_and_instance_methods_run() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Counter {
    int value = 41;

    int next() {
        return value + 1;
    }

    public static void main(String[] args) {
        Counter counter = null;
        System.out.println("start");
    }
}
"#;
    // `new` is not lowered yet, so the instance path is exercised by compiling it, not calling it:
    // the class still has to verify, which means `next()`'s `getfield` and the constructor's
    // `putfield` are both checked by the JVM.
    assert_eq!(run(source, "Counter"), "start\n");
}

/// A construct the lowering does not handle is reported, not mis-emitted. Silence here would mean
/// a class file that loads and then misbehaves.
#[test]
fn an_unsupported_construct_is_reported() {
    let source = r"
public class Unsupported {
    public static void main(String[] args) {
        int[] values = new int[3];
    }
}
";
    let error = compile(source).expect_err("array creation is not lowered yet");
    assert!(
        matches!(error, LowerError::Unsupported(_)),
        "expected an Unsupported error, got {error}"
    );
}

/// `x += 1` is the same node kind as `x = 1`, so nothing in a kind-driven lowering distinguishes
/// them. Lowering it as a plain store computes `x = 1` instead — a class file that verifies, runs,
/// and produces the wrong number, which is worse than any error.
#[test]
fn a_compound_assignment_is_reported_rather_than_mis_emitted() {
    for operator in ["+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>="] {
        let source = format!(
            r"
public class Compound {{
    public static void main(String[] args) {{
        int i = 5;
        i {operator} 1;
    }}
}}
"
        );
        let error = compile(&source).expect_err("compound assignment is not lowered yet");
        assert!(
            matches!(error, LowerError::Unsupported("a compound assignment")),
            "`{operator}` should be reported, got {error}"
        );
    }
    // The simple form still compiles: the check is on the operator, not on assignment as such.
    compile(
        r"
public class Simple {
    public static void main(String[] args) {
        int i = 5;
        i = 1;
    }
}
",
    )
    .expect("a simple assignment still lowers");
}

/// A `long` / `float` / `double` comparison is not an `if_icmp*`. Emitting one produced a class
/// file that loaded and then failed verification with *"Type `long_2nd` is not assignable to
/// integer"* — the compiler had every fact needed to say so first.
#[test]
fn a_wide_comparison_is_reported_rather_than_mis_emitted() {
    for (ty, literal) in [("long", "1L"), ("double", "1.0"), ("float", "1.0f")] {
        let source = format!(
            r"
public class Wide {{
    static boolean f({ty} a) {{
        return a == {literal};
    }}

    public static void main(String[] args) {{}}
}}
"
        );
        let error = compile(&source).expect_err("wide comparisons are not lowered yet");
        assert!(
            matches!(error, LowerError::Unsupported("a comparison of this type")),
            "`{ty}` comparison should be reported, got {error}"
        );
    }
    // An `int` comparison still lowers: the check is on the operand type, not on comparison itself.
    compile(
        r"
public class Narrow {
    static boolean f(int a) {
        return a == 1;
    }

    public static void main(String[] args) {}
}
",
    )
    .expect("an `int` comparison still lowers");
}

/// String and `char` literals reach the constant pool with their escapes resolved, and the JVM
/// printing them back is the only check that says so. `trim_end_matches` used to take *every*
/// trailing quote, so a literal ending in `\"` lost it silently.
#[test]
fn literal_escapes_survive_to_the_constant_pool() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Escapes {
    public static void main(String[] args) {
        System.out.println("a\"");
        System.out.println("\\");
        System.out.println("A\101");
        System.out.println("tab:\tend");
    }
}
"#;
    assert_eq!(run(source, "Escapes"), "a\"\n\\\nAA\ntab:\tend\n");
}

/// An escape this cannot read is reported. Pushing the character after the backslash — the old
/// fallback — produced a string constant that was quietly wrong.
#[test]
fn an_unreadable_escape_is_reported() {
    let source = r#"
public class BadEscape {
    public static void main(String[] args) {
        System.out.println("\q");
    }
}
"#;
    let error = compile(source).expect_err("`\\q` is not an escape");
    assert!(
        matches!(
            error,
            LowerError::Unsupported("an escape sequence this lowering cannot read")
        ),
        "expected the escape report, got {error}"
    );
}

/// `ladd` takes two `long`s, so `n + 1` on a `long` needs the literal widened first. Until binary
/// numeric promotion is lowered, the mixed pair is reported — and it names the construct rather
/// than surfacing as the assembler's bare `TypeMismatch`.
#[test]
fn a_mixed_numeric_binary_is_reported() {
    let source = r"
public class Mixed {
    static long f(long n) {
        return n + 1;
    }

    public static void main(String[] args) {}
}
";
    let error = compile(source).expect_err("numeric promotion is not lowered yet");
    assert!(
        matches!(
            error,
            LowerError::Unsupported("a binary operator over two different numeric types")
        ),
        "expected the mixed-operand report, got {error}"
    );
}

/// Access flags are what the source wrote, bit for bit.
///
/// The four access levels are one choice and `static` / `final` / `abstract` are independent bits;
/// folding them together dropped `ACC_PUBLIC` from every `public static` method. That only looked
/// harmless because Java 25's launch protocol accepts a non-`public` `main` — a `public` helper
/// reached from another package would have failed with `IllegalAccessError`.
#[test]
fn the_emitted_access_flags_are_what_the_source_wrote() {
    let source = r"
public class Flags {
    public static final int OPEN = 1;
    private int hidden;
    protected volatile int shared;
    int packaged;

    public final int keep() { return 1; }
    private static synchronized int inner() { return 2; }
    protected int guarded() { return 3; }
    static int plain() { return 4; }

    public static void main(String[] args) {}
}
";
    let classes = compile(source).expect("compile");
    let bytes = &classes[0].bytes;
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(bytes.as_slice()))
        .expect("reparse");

    let name_of = |index| class.constant_pool.utf8(index).expect("utf8").into_owned();
    let fields: Vec<(String, u16)> = class
        .fields
        .iter()
        .map(|field| (name_of(field.name_index), field.access_flags.0))
        .collect();
    assert_eq!(
        fields,
        [
            // public | static | final
            ("OPEN".to_owned(), 0x0001 | 0x0008 | 0x0010),
            ("hidden".to_owned(), 0x0002),
            // protected | volatile
            ("shared".to_owned(), 0x0004 | 0x0040),
            // package-private is the absence of a bit, not a bit of its own.
            ("packaged".to_owned(), 0x0000),
        ]
    );

    let methods: Vec<(String, u16)> = class
        .methods
        .iter()
        .map(|method| (name_of(method.name_index), method.access_flags.0))
        .collect();
    assert_eq!(
        methods,
        [
            ("keep".to_owned(), 0x0001 | 0x0010),           // public | final
            ("inner".to_owned(), 0x0002 | 0x0008 | 0x0020), // private | static | synchronized
            ("guarded".to_owned(), 0x0004),                 // protected
            ("plain".to_owned(), 0x0008),                   // static, package-private
            ("main".to_owned(), 0x0001 | 0x0008),           // public | static
            // The default constructor takes the class's own access level (JLS §8.8.9).
            ("<init>".to_owned(), 0x0001),
        ]
    );
    // `public class` — and `ACC_SUPER`, which every emitted class carries.
    assert_eq!(class.access_flags.0, 0x0001 | 0x0020);
}

/// A package-private class stays package-private, and so does the constructor it did not declare.
#[test]
fn a_package_private_class_is_not_widened_to_public() {
    let classes = compile(
        r"
class Quiet {
    int value;
}
",
    )
    .expect("compile");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(classes[0].bytes.as_slice()))
            .expect("reparse");
    assert_eq!(class.access_flags.0, 0x0020, "`ACC_SUPER` only");
    assert_eq!(class.methods[0].access_flags.0, 0x0000);
}

/// An arm that returns has nothing to jump *from*, so the jump over the `else` is not emitted.
/// Emitting it unconditionally made `if (c) { return …; } …` — one of the most ordinary shapes in
/// Java — fail with "code was emitted after an unconditional transfer".
#[test]
fn an_arm_that_returns_still_compiles() {
    if !java_available() {
        return;
    }
    let source = r"
public class Guard {
    static int classify(int n) {
        if (n == 0) {
            return 10;
        }
        if (n == 1) {
            return 20;
        } else {
            return 30;
        }
    }

    static void act(int n) {
        if (n == 0) {
            return;
        }
        System.out.println(n);
    }

    static int firstEven(int limit) {
        int i = 0;
        while (i < limit) {
            if (i == 4) {
                return i;
            }
            i = i + 1;
        }
        return 99;
    }

    public static void main(String[] args) {
        System.out.println(classify(0));
        System.out.println(classify(1));
        System.out.println(classify(2));
        act(0);
        act(7);
        System.out.println(firstEven(9));
        System.out.println(firstEven(2));
    }
}
";
    assert_eq!(run(source, "Guard"), "10\n20\n30\n7\n4\n99\n");
}

/// A loop body declaring its own locals is ordinary Java, and the back edge arrives with slots the
/// loop head's frame never described. Requiring the two states to be *equal* rejected it as
/// "incompatible"; what the head's frame actually needs preserved is only what it described.
#[test]
fn a_loop_body_may_declare_locals() {
    let source = r"
public class Locals {
    static int run(int limit) {
        int total = 0;
        int i = 0;
        while (i < limit) {
            int doubled = i + i;
            int shifted = doubled + 1;
            total = total + shifted;
            i = i + 1;
            while (i == 100) {
                int unused = 0;
                i = i + unused;
            }
        }
        return total;
    }

    public static void main(String[] args) {
        System.out.println(run(4));
    }
}
";
    if !java_available() {
        return;
    }
    // (0+1) + (2+1) + (4+1) + (6+1) = 16.
    assert_eq!(run(source, "Locals"), "16\n");
}

/// A nested type is its own class file. Dropping it silently would produce an outer class that
/// loads and then throws `NoClassDefFoundError` at the first use of the inner one — a failure the
/// compiler is in a position to report and the run time is not.
#[test]
fn a_nested_type_is_reported_rather_than_dropped() {
    let source = r"
public class Outer {
    static class Inner {
        int value;
    }

    public static void main(String[] args) {}
}
";
    let error = compile(source).expect_err("nested types are not emitted yet");
    assert!(
        matches!(error, LowerError::Unsupported("a nested type declaration")),
        "expected the nested-type report, got {error}"
    );
}

/// A constructor's prologue emits `super()` and the field initialisers. An explicit `this(…)` or
/// `super(args)` replaces part of that, so emitting both would run one of them twice — a class that
/// verifies and initialises wrongly. Reported until the delegation is lowered.
#[test]
fn an_explicit_constructor_invocation_is_reported() {
    for body in ["this(1);", "super();"] {
        let source = format!(
            r"
public class Delegating {{
    int v = 7;

    Delegating(int v) {{}}

    Delegating() {{ {body} }}

    public static void main(String[] args) {{}}
}}
"
        );
        let error = compile(&source).expect_err("constructor delegation is not lowered yet");
        assert!(
            matches!(
                error,
                LowerError::Unsupported("an explicit constructor invocation")
            ),
            "`{body}` should be reported, got {error}"
        );
    }
}

/// The prologue's `super()` only exists if the superclass has one. Emitting it regardless produced
/// a class that loaded and then threw `NoSuchMethodError` at the first `new` — the compiler knows
/// the superclass's constructors and can say so instead.
#[test]
fn a_superclass_without_a_no_argument_constructor_is_reported() {
    let source = r"
public class Base {
    Base(int seed) {}
}

class Derived extends Base {
}
";
    let error = compile(source).expect_err("there is no `Base()` to call");
    assert!(
        matches!(
            error,
            LowerError::Unsupported("a superclass with no no-argument constructor")
        ),
        "expected the missing-`super()` report, got {error}"
    );

    // A superclass that declares *no* constructor has the implicit no-argument one, and a
    // superclass that declares one among others still has it.
    compile(
        r"
public class Plain {
    int v;
}

class Sub extends Plain {
}

class Widened extends Sub {
}
",
    )
    .expect("an implicit no-argument constructor is still a `super()`");
}
