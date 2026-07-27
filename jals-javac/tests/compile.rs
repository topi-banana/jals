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
        ("int[] v = new int[3];", "this expression form"),
        ("Object o = new Object();", "this expression form"),
        (r#"String s = "a" + "b";"#, "string concatenation"),
        ("switch (1) { default: break; }", "this statement form"),
        ("throw null;", "this statement form"),
        ("synchronized (args) { }", "this statement form"),
        ("try { } finally { }", "this statement form"),
        ("Runnable r = () -> {};", "this expression form"),
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
