//! End-to-end: Java source in, a `.class` a real JVM loads, verifies, and runs out.
//!
//! This is the milestone's acceptance test. The assembler tests prove the emitter in isolation;
//! these prove the whole path — parse, resolve, infer, select overloads, erase to descriptors,
//! lower, assemble — against the only authority that matters.

use std::fmt::Write as _;
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
        // Two of these run at once under `cargo test`, and the JVM's shared perf-data file lives at a
        // fixed path per process id — a recycled one makes the second JVM print a warning onto the
        // stdout a test is comparing.
        .arg("-XX:-UsePerfData")
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
        Runnable r = () -> {};
    }
}
";
    let error = compile(source).expect_err("a lambda is not lowered yet");
    assert!(
        matches!(error, LowerError::Unsupported(_)),
        "expected an Unsupported error, got {error}"
    );
}

/// `x += 1` is the same node kind as `x = 1`, so nothing in a kind-driven lowering distinguishes
/// them, and lowering it as a plain store computes `x = 1` instead. Every operator gets its own
/// arithmetic here, against a real JVM, because a wrong one produces a class that verifies, runs, and
/// answers wrongly — which is worse than any error.
#[test]
fn every_compound_assignment_computes_what_it_says() {
    if !java_available() {
        return;
    }
    let mut body = String::new();
    let mut expected = String::new();
    for (operator, result) in [
        ("+=", 12),
        ("-=", 6),
        ("*=", 27),
        ("/=", 3),
        ("%=", 0),
        ("&=", 1),
        ("|=", 11),
        ("^=", 10),
        ("<<=", 72),
        (">>=", 1),
        (">>>=", 1),
    ] {
        writeln!(
            body,
            "        {{ int i = 9; i {operator} 3; System.out.println(i); }}"
        )
        .expect("write to a String");
        writeln!(expected, "{result}").expect("write to a String");
    }
    let source = format!(
        r"
public class Compound {{
    public static void main(String[] args) {{
{body}    }}
}}
"
    );
    assert_eq!(run(&source, "Compound"), expected);
}

/// A compound assignment narrows its result back to the target's type.
///
/// JLS §15.26.2 defines `E1 op= E2` as `E1 = (T)((E1) op (E2))`, and both halves of that cast are
/// load-bearing. `byte b = 127; b += 1` has to wrap to -128, and `int i; i += 1L` has to widen the
/// `int` to a `long`, add, and narrow back — three instructions where a naive lowering emits one.
#[test]
fn a_compound_assignment_narrows_back_to_its_target() {
    if !java_available() {
        return;
    }
    let source = r"
public class Narrowing {
    static byte small = 127;
    static int counter = 1;
    int[] cells = null;

    public static void main(String[] args) {
        small += 1;
        System.out.println(small);
        counter += 3000000000L;
        System.out.println(counter);
        char c = 'a';
        c += 1;
        System.out.println(c);
        short s = 32767;
        s += 1;
        System.out.println(s);
        double d = 1;
        d /= 4;
        System.out.println(d);
        long big = 1;
        big <<= 40;
        System.out.println(big);
    }
}
";
    assert_eq!(
        run(source, "Narrowing"),
        // 127 + 1 wraps; 1 + 3000000000 as a `long` is 3000000001, whose low 32 bits are
        // -1294967295; 'a' + 1 is 'b'; 32767 + 1 wraps; integer 1 / 4 as a `double` is 0.25.
        "-128\n-1294967295\nb\n-32768\n0.25\n1099511627776\n"
    );
}

/// A `long` / `float` / `double` comparison is not an `if_icmp*`: it reduces through `lcmp` /
/// `fcmp?` / `dcmp?` first, and a reference one is an `if_acmp*`.
///
/// The NaN rows are the ones no verifier checks. JLS §15.20.1 makes every ordering comparison
/// involving a NaN false in *both* directions, which only holds if `<` reduces with the `g` form and
/// `>` with the `l` form. Swap them and the class still loads and still runs — and answers `true` for
/// a NaN.
#[test]
fn every_width_of_comparison_answers_correctly() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Widths {
    static double nan() { return 0.0 / 0.0; }

    public static void main(String[] args) {
        long a = 2, b = 1;
        System.out.println(a > b);
        System.out.println(a < b);
        System.out.println(a == b);
        System.out.println(a != b);
        float f = 1.5f;
        System.out.println(f <= 1.5f);
        System.out.println(f >= 2.5f);
        double d = nan();
        System.out.println(d < 1.0);
        System.out.println(d > 1.0);
        System.out.println(d <= 1.0);
        System.out.println(d >= 1.0);
        System.out.println(d == d);
        System.out.println(d != d);
        String s = "x";
        System.out.println(s == null);
        System.out.println(s != null);
        boolean t = true;
        System.out.println(t == true);
        char c = 'b';
        System.out.println(c > 'a');
    }
}
"#;
    assert_eq!(
        run(source, "Widths"),
        "true\nfalse\nfalse\ntrue\n\
         true\nfalse\n\
         false\nfalse\nfalse\nfalse\nfalse\ntrue\n\
         false\ntrue\n\
         true\n\
         true\n"
    );
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

/// `ladd` takes two `long`s, so `n + 1` on a `long` needs the literal widened first. Binary numeric
/// promotion (JLS §5.6.2) is what supplies that `i2l`, and it is not a formality: one opcode names one
/// type, so *every* mixed pair needs a conversion or the class does not verify.
#[test]
fn binary_numeric_promotion_widens_the_narrower_side() {
    if !java_available() {
        return;
    }
    let source = r"
public class Mixed {
    static long addLong(long n) { return n + 1; }
    static double addDouble(int n) { return n + 0.5; }
    static float addFloat(long n) { return n + 1.5f; }
    static int addBytes(byte a, byte b) { return a + b; }
    static long shiftLong(long n, long by) { return n << by; }

    public static void main(String[] args) {
        System.out.println(addLong(4000000000L));
        System.out.println(addDouble(3));
        System.out.println(addFloat(2L));
        System.out.println(addBytes((byte) 100, (byte) 100));
        System.out.println(shiftLong(1L, 40L));
        // A `char` promotes to `int` for arithmetic, and back down only through a cast.
        char c = 'a';
        System.out.println(c + 1);
        System.out.println((char) (c + 1));
    }
}
";
    assert_eq!(
        run(source, "Mixed"),
        "4000000001\n3.5\n3.5\n200\n1099511627776\n98\nb\n"
    );
}

/// The `return` opcode comes from the *declared* return type, not from whatever the expression left
/// on the stack.
///
/// Reading it off the stack emitted `ireturn` for `long f() { return 1; }` — a class file whose
/// descriptor promises a `long` and whose body hands back an `int`. It only became reachable once
/// conversions existed, which is why it is pinned here rather than left to the promotion tests.
#[test]
fn a_return_converts_to_the_declared_type() {
    if !java_available() {
        return;
    }
    let source = r"
public class Returns {
    static long asLong() { return 1; }
    static double asDouble() { return 1; }
    static float asFloat() { return 1; }
    static byte asByte() { return (byte) 300; }
    // A reference return needs no conversion, but it does need `areturn` rather than `ireturn`.
    // `println(Object)` is not in the embedded stubs, so the value is tested rather than printed.
    static Object asObject() { return null; }

    public static void main(String[] args) {
        System.out.println(asLong());
        System.out.println(asDouble());
        System.out.println(asFloat());
        System.out.println(asByte());
        System.out.println(asObject() == null);
    }
}
";
    assert_eq!(run(source, "Returns"), "1\n1.0\n1.0\n44\ntrue\n");
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
            // `OPEN`'s initialiser has to run somewhere, and `<clinit>` is the only place. It takes
            // no access level at all — nothing can name it — and `ACC_STATIC` from version 51 on.
            ("<clinit>".to_owned(), 0x0008),
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

/// An interface's members carry the modifiers JLS lets the source leave unwritten: a field is
/// implicitly `public static final` (§9.3) and a method implicitly `public abstract` (§9.4).
/// Emitting them package-private produces a class file the verifier rejects.
#[test]
fn an_interface_gets_the_modifiers_its_source_left_unwritten() {
    let source = r"
public interface Shape {
    int SIDES = 3;

    int area();

    static int zero() {
        return 0;
    }
}
";
    let classes = compile(source).expect("compile");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(classes[0].bytes.as_slice()))
            .expect("reparse");
    let name_of = |index| class.constant_pool.utf8(index).expect("utf8").into_owned();

    // public | interface | abstract
    assert_eq!(class.access_flags.0, 0x0001 | 0x0200 | 0x0400);
    assert_eq!(
        class
            .fields
            .iter()
            .map(|f| (name_of(f.name_index), f.access_flags.0))
            .collect::<Vec<_>>(),
        // public | static | final
        [("SIDES".to_owned(), 0x0001 | 0x0008 | 0x0010)]
    );
    assert_eq!(
        class
            .methods
            .iter()
            .map(|m| (name_of(m.name_index), m.access_flags.0))
            .collect::<Vec<_>>(),
        [
            ("area".to_owned(), 0x0001 | 0x0400), // public | abstract
            ("zero".to_owned(), 0x0001 | 0x0008), // public | static
            // `SIDES` is implicitly `static final` and still has an initialiser to run, so an
            // interface gets a `<clinit>` like a class does (JVMS §2.9.2 allows one from version 51).
            ("<clinit>".to_owned(), 0x0008),
        ]
    );
    // An interface has no `ACC_SUPER` and gets no default constructor.
    assert!(
        !class
            .methods
            .iter()
            .any(|m| name_of(m.name_index) == "<init>")
    );

    if !java_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("temp dir");
    std::fs::write(directory.path().join("Shape.class"), &classes[0].bytes).expect("write");
    let output = Command::new("java")
        // Two of these run at once under `cargo test`, and the JVM's shared perf-data file lives at a
        // fixed path per process id — a recycled one makes the second JVM print a warning onto the
        // stdout a test is comparing.
        .arg("-XX:-UsePerfData")
        .arg("-cp")
        .arg(directory.path())
        .arg("Shape")
        .output()
        .expect("run java");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("ClassFormatError") && !stderr.contains("VerifyError"),
        "the JVM rejected the interface: {stderr}"
    );
}

/// A method with no body has to say *why* it has none. `abstract` says so and an interface method
/// says so implicitly; `native` says so with its own flag, and `ACC_NATIVE | ACC_ABSTRACT` is a
/// pair JVMS §4.6 forbids — a JVM rejects the class with "illegal modifiers: 0x500".
#[test]
fn a_body_less_method_that_is_not_abstract_is_reported() {
    let error = compile(
        r"
public class Bodyless {
    native int f();

    public static void main(String[] args) {}
}
",
    )
    .expect_err("`native` has no body this can lower, and no flag pair that would say so");
    assert!(
        matches!(error, LowerError::Unsupported("a method with no body")),
        "expected the body-less report, got {error}"
    );

    // `abstract` does say why, and still compiles.
    compile(
        r"
public abstract class Abstract {
    abstract int f();

    public static void main(String[] args) {}
}
",
    )
    .expect("an `abstract` method declares why it has no body");
}

/// Every loop form, and `break` / `continue` with and without a label.
///
/// `continue` in a `for` is the one that goes wrong silently: it has to run the update section
/// (JLS §14.14.1.3), and sending it to the condition instead is an infinite loop that only appears
/// when a body actually contains one. So the `for` rows below all contain a `continue`.
#[test]
fn every_loop_form_runs() {
    if !java_available() {
        return;
    }
    let source = r"
public class Loops {
    static int sumWhile(int limit) {
        int total = 0, i = 0;
        while (i < limit) { total = total + i; i = i + 1; }
        return total;
    }

    static int sumDo(int limit) {
        int total = 0, i = 0;
        do { total = total + i; i = i + 1; } while (i < limit);
        return total;
    }

    static int sumFor(int limit) {
        int total = 0;
        for (int i = 0; i < limit; i++) { total += i; }
        return total;
    }

    static int sumOddsFor(int limit) {
        int total = 0;
        for (int i = 0; i < limit; i++) {
            if (i % 2 == 0) { continue; }
            total += i;
        }
        return total;
    }

    static int firstOver(int[] values, int bound) {
        for (int v : values) {
            if (v > bound) { return v; }
        }
        return -1;
    }

    static int sumEach(int[] values) {
        int total = 0;
        for (int v : values) { total += v; }
        return total;
    }

    static int untilBreak(int limit) {
        int i = 0;
        for (;;) {
            if (i >= limit) { break; }
            i++;
        }
        return i;
    }

    static int firstPair(int[] values, int target) {
        outer:
        for (int i = 0; i < values.length; i++) {
            for (int j = 0; j < values.length; j++) {
                if (i == j) { continue; }
                if (values[i] + values[j] == target) { return i * 10 + j; }
                if (values[j] > 100) { continue outer; }
            }
        }
        return -1;
    }

    static int labelledBreak(int[] values) {
        int found = -1;
        search:
        for (int v : values) {
            if (v == 3) { found = v; break search; }
        }
        return found;
    }

    static int labelledBlock(int n) {
        int out = 0;
        done: {
            out = 1;
            if (n > 0) { break done; }
            out = 2;
        }
        return out;
    }

    public static void main(String[] args) {
        System.out.println(sumWhile(5));
        System.out.println(sumDo(5));
        System.out.println(sumDo(0));
        System.out.println(sumFor(5));
        System.out.println(sumOddsFor(6));
        int[] values = null;
        System.out.println(untilBreak(4));
        System.out.println(labelledBlock(1));
        System.out.println(labelledBlock(-1));
    }
}
";
    // `sumDo(0)` runs its body once, which is the whole difference from `while`.
    assert_eq!(run(source, "Loops"), "10\n10\n0\n10\n9\n4\n1\n2\n");
}

/// A `for`-each over an array, and the arrays it walks — reached through `args`, which is the one
/// array a `main` has without `new`.
#[test]
fn a_for_each_walks_an_array() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Walk {
    public static void main(String[] args) {
        System.out.println(args.length);
        int count = 0;
        for (String s : args) { count++; }
        System.out.println(count);
        for (int i = 0; i < args.length; i++) {
            System.out.println(args[i]);
        }
        // An element assignment, which is `dup_x2` for the value to survive the store.
        if (args.length > 0) {
            System.out.println(args[0] = "replaced");
            System.out.println(args[0]);
        }
    }
}
"#;
    let classes = compile(source).expect("compile");
    let directory = tempfile::tempdir().expect("temp dir");
    std::fs::write(directory.path().join("Walk.class"), &classes[0].bytes).expect("write");
    let output = Command::new("java")
        // Two of these run at once under `cargo test`, and the JVM's shared perf-data file lives at a
        // fixed path per process id — a recycled one makes the second JVM print a warning onto the
        // stdout a test is comparing.
        .arg("-XX:-UsePerfData")
        .arg("-cp")
        .arg(directory.path())
        .arg("Walk")
        .arg("one")
        .arg("two")
        .output()
        .expect("run java");
    assert!(
        output.status.success(),
        "the JVM rejected the class:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "2\n2\none\ntwo\nreplaced\nreplaced\n"
    );
}

/// `++` and `--`, in both positions, over each kind of place.
///
/// The narrowing is what a naive lowering drops: `byte b = 127; b++` has to wrap, because JLS §15.14
/// defines it as `b = (byte)(b + 1)`. And a postfix form has to yield the value from *before* the
/// update, which for a field means re-seating the old value under the receiver.
#[test]
fn increments_update_and_yield_the_right_value() {
    if !java_available() {
        return;
    }
    let source = r"
public class Steps {
    int field = 10;
    static int shared = 20;

    int bumpField() { return field++; }
    int preBumpField() { return ++field; }
    int readField() { return field; }

    public static void main(String[] args) {
        int i = 5;
        System.out.println(i++);
        System.out.println(i);
        System.out.println(++i);
        System.out.println(i--);
        System.out.println(--i);

        byte b = 127;
        b++;
        System.out.println(b);
        char c = 'y';
        System.out.println(++c);
        long l = 4294967296L;
        l++;
        System.out.println(l);
        double d = 1.5;
        System.out.println(d++);
        System.out.println(d);

        System.out.println(shared++);
        System.out.println(shared);
    }
}
";
    assert_eq!(
        run(source, "Steps"),
        "5\n6\n7\n7\n5\n-128\nz\n4294967297\n1.5\n2.5\n20\n21\n"
    );
}

/// Assignment to a field, and the value it yields.
///
/// An assignment is an *expression* whose value is the one assigned, so `println(o.f = 2)` has to
/// leave the value behind after the `putfield` consumed it. That is `dup_x1` — the copy goes under
/// the receiver, not on top of it.
#[test]
fn an_assignment_to_a_field_yields_its_value() {
    if !java_available() {
        return;
    }
    let source = r"
public class Store {
    int instance;
    static int shared;
    static long wide;

    int setInstance(int v) { return instance = v; }
    int getInstance() { return instance; }

    public static void main(String[] args) {
        System.out.println(shared = 7);
        System.out.println(shared);
        System.out.println(wide = 4294967296L);
        // A chain assigns right to left and each link yields what it stored.
        int a = 0, b = 0;
        System.out.println(a = b = 3);
        System.out.println(a + b);
        // A compound assignment on a `static` field reads and writes the same place.
        shared *= 6;
        System.out.println(shared);
    }
}
";
    assert_eq!(run(source, "Store"), "7\n7\n4294967296\n3\n6\n42\n");
}

/// Casts, `instanceof`, the conditional operator, and the short-circuiting operators.
#[test]
fn casts_tests_and_conditionals_run() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Choose {
    static boolean loud = false;

    static boolean note(boolean value) { loud = true; return value; }

    static String pick(int n) { return n > 0 ? "positive" : "other"; }

    public static void main(String[] args) {
        System.out.println(pick(1));
        System.out.println(pick(-1));
        // A conditional whose arms are different numeric types promotes to the wider one.
        long chosen = args.length == 0 ? 1 : 2L;
        System.out.println(chosen);

        // `&&` must not evaluate its right operand once the left decided the answer.
        loud = false;
        System.out.println(false && note(true));
        System.out.println(loud);
        loud = false;
        System.out.println(true || note(true));
        System.out.println(loud);
        // And it must evaluate it when the left did not.
        loud = false;
        System.out.println(true && note(false));
        System.out.println(loud);

        // The non-short-circuiting boolean operators, which are the same tokens as the bitwise ones.
        System.out.println(true & false);
        System.out.println(true | false);
        System.out.println(true ^ true);
        boolean flag = true;
        flag &= false;
        System.out.println(flag);

        Object boxed = "text";
        System.out.println(boxed instanceof String);
        System.out.println(boxed instanceof Integer);
        String back = (String) boxed;
        System.out.println(back.length());
        System.out.println(args instanceof Object);

        // A primitive narrowing cast, which is the only place one appears without an assignment.
        double big = 300.7;
        System.out.println((byte) big);
        System.out.println((int) big);
        System.out.println((char) 66);
        System.out.println(-big);
        System.out.println(~5);
        System.out.println(!false);
    }
}
"#;
    assert_eq!(
        run(source, "Choose"),
        "positive\nother\n1\n\
         false\nfalse\n\
         true\nfalse\n\
         false\ntrue\n\
         false\ntrue\nfalse\nfalse\n\
         true\nfalse\n4\ntrue\n\
         44\n300\nB\n-300.7\n-6\ntrue\n"
    );
}

/// A `static` field's initialiser and a `static { … }` block both run in `<clinit>`, and an instance
/// initialiser block runs in every constructor.
///
/// Nothing else runs them, so dropping them produced a class whose `static int n = 5;` read back as
/// 0 — a class that verifies, runs, and answers wrongly.
#[test]
fn the_initializers_run_where_they_belong() {
    if !java_available() {
        return;
    }
    let source = r"
public class Init {
    static int first = 5;
    static int second = first * 2;
    static int third;

    static {
        third = second + 1;
    }

    public static void main(String[] args) {
        System.out.println(first);
        System.out.println(second);
        System.out.println(third);
    }
}
";
    // The order matters: `second` reads `first`, and the block reads `second`.
    assert_eq!(run(source, "Init"), "5\n10\n11\n");
}

/// `int a = 1, b = 2;` is one declaration and two initialisers. Taking the first expression for
/// every name gave `b` the value of `a` — in a class file that verifies.
#[test]
fn each_declarator_takes_its_own_initializer() {
    if !java_available() {
        return;
    }
    let source = r"
public class Several {
    static int p = 1, q = 2, r = 3;
    int a = 4, b = 5;

    int sum() { return a * 10 + b; }

    public static void main(String[] args) {
        System.out.println(p);
        System.out.println(q);
        System.out.println(r);
        int x = 6, y = 7;
        System.out.println(x * 10 + y);
    }
}
";
    assert_eq!(run(source, "Several"), "1\n2\n3\n67\n");
}

/// Object creation is still not lowered, so the construct is reported rather than dropped. Both `new`
/// forms stop here, and so do the features the milestones after this one bring.
#[test]
fn the_features_after_this_milestone_are_still_reported() {
    for (source, expected) in [
        ("switch (1) { default: break; }", "this statement form"),
        ("Runnable r = () -> {};", "this expression form"),
        ("Runnable r = Later::main;", "this expression form"),
        ("Object c = int.class;", "this expression form"),
        ("Integer boxed = 1;", "a boxing conversion"),
        (
            "int v = switch (1) { default -> 2; };",
            "this expression form",
        ),
    ] {
        let program = format!(
            r"
public class Later {{
    public static void main(String[] args) {{
        {source}
    }}
}}
"
        );
        let error = compile(&program).expect_err("this is a later milestone");
        assert!(
            matches!(error, LowerError::Unsupported(what) if what == expected),
            "`{source}` should report {expected:?}, got {error}"
        );
    }
}

/// `new Foo(args)`, with the overload the arguments selected.
#[test]
fn object_creation_runs_the_constructor_it_selected() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Point {
    int x;
    int y;
    String tag = "made";

    Point() {
        x = 1;
        y = 1;
    }

    Point(int both) {
        x = both;
        y = both;
    }

    Point(int x, long y) {
        this.x = x;
        this.y = (int) y;
    }

    int sum() { return x * 100 + y; }

    public static void main(String[] args) {
        System.out.println(new Point().sum());
        System.out.println(new Point(3).sum());
        // The `long` parameter is what makes this overload the one an `int` literal widens into.
        System.out.println(new Point(4, 5).sum());
        System.out.println(new Point().tag);
        // A constructor's field initialisers run before its own body, so `tag` is set either way.
        Point moved = new Point(7);
        moved.x = 9;
        System.out.println(moved.sum());
        System.out.println(new StringBuilder("ab").append(1).append('c').toString());
    }
}
"#;
    assert_eq!(run(source, "Point"), "101\n303\n405\nmade\n907\nab1c\n");
}

/// A `new` whose argument list contains a branch keeps an *uninitialised* reference live across it.
///
/// That is the one shape where a stack-map frame has to name the `new`'s own bytecode offset — and the
/// offset does not exist until branch widening has run, so it is carried as an item index and
/// translated at the end. A wrong translation reparses perfectly and then fails to load.
#[test]
fn a_new_with_a_conditional_argument_verifies() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Fresh {
    int value;

    Fresh(int value) { this.value = value; }

    public static void main(String[] args) {
        Fresh chosen = new Fresh(args.length == 0 ? 10 : 20);
        System.out.println(chosen.value);
        System.out.println(new StringBuilder(args.length == 0 ? "none" : "some").append('!').toString());
    }
}
"#;
    assert_eq!(run(source, "Fresh"), "10\nnone!\n");
}

/// Every array-creation form, and the element types whose opcodes differ.
#[test]
fn every_array_creation_form_runs() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Made {
    static int[] sized(int n) { return new int[n]; }

    public static void main(String[] args) {
        int[] a = new int[3];
        a[2] = 42;
        System.out.println(a[2] + a.length);

        // Both levels at once, and only the outer one.
        int[][] both = new int[2][3];
        both[1][2] = 5;
        System.out.println(both.length * 100 + both[0].length * 10 + both[1][2]);
        int[][] outer = new int[2][];
        System.out.println(outer.length);
        outer[0] = sized(4);
        System.out.println(outer[0].length);

        // An initialiser, with and without the `new T[]` in front of it.
        int[] listed = new int[]{1, 2, 3};
        int[] bare = {4, 5};
        System.out.println(listed[2] + bare[0] + bare[1]);
        int[][] nested = {{1, 2}, {3}};
        System.out.println(nested[0][1] * 100 + nested[1][0] * 10 + nested[1].length);

        // The four narrow element types have their own opcodes, and `boolean` shares `byte`'s.
        byte[] bytes = {1, (byte) 200};
        System.out.println(bytes[1]);
        char[] chars = {'x', (char) 65535};
        System.out.println(chars[0]);
        System.out.println((int) chars[1]);
        short[] shorts = {(short) 40000};
        System.out.println(shorts[0]);
        boolean[] flags = {true, false};
        System.out.println(flags[0]);
        long[] longs = {4294967296L};
        System.out.println(longs[0]);
        double[] doubles = {1.5, 2.5};
        System.out.println(doubles[0] + doubles[1]);
        String[] names = {"first", "second"};
        System.out.println(names[1]);
        // An element assignment yields the value it wrote.
        System.out.println(a[0] = 8);
        a[1] += 3;
        System.out.println(a[1]);
        a[1]++;
        System.out.println(a[1]);
    }
}
"#;
    assert_eq!(
        run(source, "Made"),
        "45\n235\n2\n4\n12\n231\n-56\nx\n65535\n-25536\ntrue\n4294967296\n4.0\nsecond\n8\n3\n4\n"
    );
}

/// String concatenation, at every operand type and through the flattening a chain needs.
///
/// `a + b + c` parses as `(a + b) + c`. Lowering each `+` on its own builds the left string, hands it
/// to a second builder, and throws it away — correct but quadratic. And which `append` overload an
/// operand takes is not a verification question: sending a `char` to `append(int)` prints its code
/// point in a class file that loads and runs.
#[test]
fn string_concatenation_appends_each_operand_at_its_own_type() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Joined {
    static String describe(int n) { return "n=" + n; }

    public static void main(String[] args) {
        System.out.println("x" + 1 + 'c' + 2L + true + 1.5 + 1.5f);
        System.out.println(describe(7));
        // A `+` inside a concatenation whose own result is numeric is an *addition*.
        System.out.println("sum=" + (1 + 2));
        System.out.println("digits=" + 1 + 2);
        // The other way round: a numeric prefix turns into a string as soon as one operand is one.
        System.out.println(1 + 2 + "x");
        // A `null` reference appends as the four characters, not as a thrown exception.
        String missing = null;
        System.out.println("[" + missing + "]");
        Object boxed = null;
        System.out.println("[" + boxed + "]");
        // A `byte` and a `short` have no overload of their own; they are already `int`s.
        byte b = 7;
        short s = 8;
        System.out.println("" + b + s);
        // Compound concatenation reads the target and writes it back.
        String acc = "a";
        acc += "b";
        acc += 1;
        acc += 'c';
        System.out.println(acc);
        // And on a field, where the address has to survive the read.
        System.out.println(new Joined().grow());
    }

    String label = "L";

    String grow() {
        label += "-";
        label += 2;
        return label;
    }
}
"#;
    assert_eq!(
        run(source, "Joined"),
        "x1c2true1.51.5\n\
         n=7\n\
         sum=3\n\
         digits=12\n\
         3x\n\
         [null]\n\
         [null]\n\
         78\n\
         ab1c\n\
         L-2\n"
    );
}

/// A `for`-each over an `Iterable`, which JLS §14.14.2 defines as a loop over `iterator()`.
///
/// `next()` returns `Object` after erasure, so the element needs a `checkcast` on the way into the
/// loop variable — without it the variable holds an `Object` the frame says so about, and the first
/// method call on it fails verification.
#[test]
fn a_for_each_over_an_iterable_runs() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Walked {
    public static void main(String[] args) {
        java.util.List<String> names = new java.util.ArrayList<String>();
        names.add("one");
        names.add("two");
        int total = 0;
        for (String name : names) {
            System.out.println(name);
            total += name.length();
        }
        System.out.println(total);
        for (String name : names) {
            if (name.equals("one")) { continue; }
            System.out.println("kept " + name);
        }
    }
}
"#;
    assert_eq!(run(source, "Walked"), "one\ntwo\n6\nkept two\n");
}

/// `throw`, and the handler chain that catches it.
///
/// The clause order is what the exception table has to preserve: the JVM takes the *first* entry whose
/// range covers the throw and whose type matches, so a `catch (Exception)` written first swallows what
/// follows it — exactly as the source says.
#[test]
fn a_thrown_exception_reaches_the_first_matching_clause() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Caught {
    static int classify(int n) {
        try {
            if (n == 0) { throw new IllegalStateException("zero"); }
            if (n == 1) { return 100 / (n - 1); }
            return n;
        } catch (IllegalStateException e) {
            System.out.println("state: " + e.getMessage());
            return -1;
        } catch (ArithmeticException e) {
            System.out.println("math");
            return -2;
        }
    }

    static String multi(int n) {
        try {
            if (n == 0) { throw new IllegalArgumentException("arg"); }
            throw new IllegalStateException("state");
        } catch (IllegalArgumentException | IllegalStateException e) {
            // Both arms are `RuntimeException`s, so that is the type the binding has — and
            // `getMessage()` is declared above it, on `Throwable`.
            return e.getMessage();
        }
    }

    public static void main(String[] args) {
        System.out.println(classify(0));
        System.out.println(classify(1));
        System.out.println(classify(5));
        System.out.println(multi(0));
        System.out.println(multi(1));
        // A local written before the `try` is still readable in the handler, because the handler's
        // frame keeps the locals the protected range started with.
        String before = "kept";
        try {
            throw new RuntimeException("x");
        } catch (RuntimeException e) {
            System.out.println(before);
        }
    }
}
"#;
    assert_eq!(
        run(source, "Caught"),
        "state: zero\n-1\nmath\n-2\n5\narg\nstate\nkept\n"
    );
}

/// A `finally` runs on *every* way out of the region it guards.
///
/// Falling off the end, each `return`, each `break` or `continue` that leaves it, and anything thrown
/// — and it is duplicated at each of them, because `jsr` / `ret` is the alternative and no verifier
/// since Java 6 accepts it. A `return`'s value is computed *before* the block runs and cannot be
/// changed by it (JLS §14.20.2), which is why it goes into a slot of its own.
#[test]
fn a_finally_runs_on_every_exit() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Cleanup {
    static void trace(String s) { System.out.println(s); }

    static int returned() {
        try { return 1; } finally { trace("f:returned"); }
    }

    static int caught(int n) {
        try {
            if (n == 0) { throw new IllegalStateException("zero"); }
            return 10;
        } catch (IllegalStateException e) {
            return 20;
        } finally {
            trace("f:caught");
        }
    }

    static int propagated() {
        try {
            trace("body");
            throw new IllegalStateException("up");
        } finally {
            trace("f:propagated");
        }
    }

    static int looped(int limit) {
        int seen = 0;
        for (int i = 0; i < limit; i++) {
            try {
                if (i == 2) { continue; }
                if (i == 3) { break; }
                seen += 1;
            } finally {
                trace("f:looped:" + i);
            }
        }
        return seen;
    }

    static int nested() {
        try {
            try { return 7; } finally { trace("f:inner"); }
        } finally {
            trace("f:outer");
        }
    }

    static long wide() {
        // The held value takes two slots, which is the case a one-slot temporary would clobber.
        try { return 4294967296L; } finally { trace("f:wide"); }
    }

    static int shadowed() {
        int n = 1;
        try { return n; } finally { n = 99; }
    }

    public static void main(String[] args) {
        trace("returned=" + returned());
        trace("caught0=" + caught(0));
        trace("caught1=" + caught(1));
        try { propagated(); } catch (IllegalStateException e) { trace("escaped:" + e.getMessage()); }
        trace("looped=" + looped(5));
        trace("nested=" + nested());
        trace("wide=" + wide());
        // The `finally` cannot change what was already computed.
        trace("shadowed=" + shadowed());
    }
}
"#;
    assert_eq!(
        run(source, "Cleanup"),
        "f:returned\nreturned=1\n\
         f:caught\ncaught0=20\n\
         f:caught\ncaught1=10\n\
         body\nf:propagated\nescaped:up\n\
         f:looped:0\nf:looped:1\nf:looped:2\nf:looped:3\nlooped=2\n\
         f:inner\nf:outer\nnested=7\n\
         f:wide\nwide=4294967296\n\
         shadowed=1\n"
    );
}

/// `synchronized` releases its monitor however the block ends — the JVM refuses to return from a
/// method still holding one it took, so a missing release fails at run time rather than at load.
#[test]
fn a_synchronized_block_releases_its_monitor() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Locked {
    static int guarded(Object lock, int n) {
        synchronized (lock) {
            if (n == 0) { return -1; }
            return n * 2;
        }
    }

    public static void main(String[] args) {
        System.out.println(guarded(args, 0));
        System.out.println(guarded(args, 3));
        synchronized (args) {
            System.out.println("inside");
        }
        try {
            synchronized (args) {
                throw new IllegalStateException("thrown while held");
            }
        } catch (IllegalStateException e) {
            System.out.println(e.getMessage());
        }
        // Reaching here at all means the monitor was released on the exceptional path too.
        synchronized (args) {
            System.out.println("again");
        }
    }
}
"#;
    assert_eq!(
        run(source, "Locked"),
        "-1\n6\ninside\nthrown while held\nagain\n"
    );
}

/// An `assert` is a no-op unless the JVM was started with `-ea`.
///
/// That is the whole reason for the synthetic `$assertionsDisabled` field: emitting the check unguarded
/// would change what the program does, because a release build relies on assertions being skipped.
#[test]
fn an_assert_fires_only_under_ea() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Checked {
    public static void main(String[] args) {
        System.out.println("before");
        assert args.length == 99 : "message " + args.length;
        assert args.length == 99;
        System.out.println("after");
    }
}
"#;
    // The field is synthetic, `static final`, and named the way javac names it — a debugger and a
    // decompiler both recognise it, and a class read by javac has to agree.
    let classes = compile(source).expect("compile");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(classes[0].bytes.as_slice()))
            .expect("reparse");
    let names: Vec<String> = class
        .fields
        .iter()
        .map(|field| {
            class
                .constant_pool
                .utf8(field.name_index)
                .expect("utf8")
                .into_owned()
        })
        .collect();
    assert_eq!(names, ["$assertionsDisabled"]);
    // static | final | synthetic
    assert_eq!(class.fields[0].access_flags.0, 0x0008 | 0x0010 | 0x1000);

    // Off by default.
    assert_eq!(run(source, "Checked"), "before\nafter\n");

    // On with `-ea`, and the message is the concatenation the source wrote.
    let directory = tempfile::tempdir().expect("temp dir");
    std::fs::write(directory.path().join("Checked.class"), &classes[0].bytes).expect("write");
    let output = Command::new("java")
        .arg("-XX:-UsePerfData")
        .arg("-ea")
        .arg("-cp")
        .arg(directory.path())
        .arg("Checked")
        .output()
        .expect("run java");
    assert!(!output.status.success(), "the assertion should have fired");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "before\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("java.lang.AssertionError: message 0"),
        "{stderr}"
    );
}

/// A try-with-resources closes what it acquired, in reverse order, on every way out.
///
/// The suppression is the part that is easy to get wrong and hard to notice. JLS §14.20.3.1 says an
/// exception from `close()` is *added to* the one the body threw rather than replacing it — losing the
/// body's exception is the whole reason the construct exists, so a lowering that lets `close()` win has
/// undone it.
#[test]
fn a_try_with_resources_closes_and_suppresses() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Resourceful implements AutoCloseable {
    String name;
    boolean failOnClose;

    Resourceful(String name) { this.name = name; }
    Resourceful(String name, boolean failOnClose) {
        this.name = name;
        this.failOnClose = failOnClose;
    }

    public void close() {
        System.out.println("close:" + name);
        if (failOnClose) { throw new IllegalStateException("close:" + name); }
    }

    static void reverseOrder() {
        try (Resourceful a = new Resourceful("a"); Resourceful b = new Resourceful("b")) {
            System.out.println("body");
        }
    }

    static int closedBeforeReturning() {
        try (Resourceful a = new Resourceful("early")) { return 1; }
    }

    static void bodyThrows() {
        try (Resourceful a = new Resourceful("s", true)) {
            throw new IllegalArgumentException("body threw");
        }
    }

    static void onlyCloseThrows() {
        try (Resourceful a = new Resourceful("ct", true)) {
            System.out.println("ran");
        }
    }

    static void wrapped() {
        try (Resourceful a = new Resourceful("c")) {
            throw new IllegalArgumentException("inner");
        } catch (IllegalArgumentException e) {
            System.out.println("caught:" + e.getMessage());
        } finally {
            System.out.println("finally");
        }
    }

    public static void main(String[] args) {
        reverseOrder();
        System.out.println("returned=" + closedBeforeReturning());
        try {
            bodyThrows();
        } catch (IllegalArgumentException e) {
            System.out.println("primary:" + e.getMessage());
            System.out.println("suppressed=" + e.getSuppressed().length);
        }
        // With nothing thrown by the body, a failing `close()` is the exception.
        try {
            onlyCloseThrows();
        } catch (IllegalStateException e) {
            System.out.println("from close:" + e.getMessage());
        }
        wrapped();
    }
}
"#;
    assert_eq!(
        run(source, "Resourceful"),
        "body\nclose:b\nclose:a\n\
         close:early\nreturned=1\n\
         close:s\nprimary:body threw\nsuppressed=1\n\
         ran\nclose:ct\nfrom close:close:ct\n\
         close:c\ncaught:inner\nfinally\n"
    );
}
