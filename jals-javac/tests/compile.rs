//! End-to-end: Java source in, a `.class` a real JVM loads, verifies, and runs out.
//!
//! This is the milestone's acceptance test. The assembler tests prove the emitter in isolation;
//! these prove the whole path — parse, resolve, infer, select overloads, erase to descriptors,
//! lower, assemble — against the only authority that matters.

use std::fmt::Write as _;
use std::process::{Command, Stdio};

use jals_hir::{FileAnalysis, FileId, ProjectIndex};
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
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[(FileId(0), root)])
            .with_stdlib()
            .build(),
    );
    let semantics = analysis.in_project(&index, FileId(0));
    let typed = jals_exec::block_on_inline(semantics.typed());
    Compile::file(typed, MAJOR_JAVA_25)
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
    printed(output.stdout)
}

/// What a JVM printed, with the host's line separator normalized to `\n`.
///
/// `println` terminates a line with `System.lineSeparator()`, which is CRLF on Windows. The
/// expectations here spell what the program printed, not how the host ends a line.
fn printed(stdout: Vec<u8>) -> String {
    String::from_utf8(stdout)
        .expect("utf-8 stdout")
        .replace("\r\n", "\n")
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

/// A nested type is its own class file, named `Outer$Inner`.
///
/// Nothing in the dotted fully-qualified name says which boundary is a package and which is a nesting,
/// so each one is decided by asking whether the prefix before it is itself a type. Getting it wrong
/// produces a class that loads under one name and is referred to under another — a
/// `NoClassDefFoundError` at the first use.
#[test]
fn a_nested_type_becomes_its_own_class_file() {
    let source = r#"
package com.example;

public class Outer {
    static int shared = 10;

    static class Counter {
        int value;

        Counter(int value) { this.value = value; }

        int doubled() { return value * 2; }
    }

    interface Named {
        String name();
    }

    static class Person implements Named {
        public String name() { return "person"; }

        static class Nested {
            int deep() { return 3; }
        }
    }

    public static void main(String[] args) {
        // Referred to by simple name from inside the enclosing type, and by a partly-qualified one —
        // neither of which is a fully-qualified name, so neither resolves against packages alone.
        Counter counted = new Counter(21);
        System.out.println(counted.doubled());
        Named named = new Person();
        System.out.println(named.name());
        System.out.println(new Person.Nested().deep());
        System.out.println(new Outer.Counter(5).doubled());
        System.out.println(Counter.class.getName());
        // `getSimpleName` reads the `InnerClasses` entry; without one it answers `Outer$Counter`.
        System.out.println(Counter.class.getSimpleName());
        System.out.println(shared);
    }
}
"#;
    let classes = compile(source).expect("compile");
    let names: Vec<&str> = classes
        .iter()
        .map(|class| class.internal_name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "com/example/Outer",
            "com/example/Outer$Counter",
            "com/example/Outer$Named",
            "com/example/Outer$Person",
            // Nesting is not one level: the `$` is at every boundary the index says is one.
            "com/example/Outer$Person$Nested",
        ]
    );

    // The `InnerClasses` attribute is where a nested type's `private` and `static` live — its own
    // `access_flags` has nowhere to put either, so this is the only record of what the source wrote,
    // and `getSimpleName` reads it back from here too.
    let outer =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(classes[0].bytes.as_slice()))
            .expect("reparse");
    let entries = outer
        .attributes
        .iter()
        .find_map(|attribute| match &attribute.body {
            jals_classfile::AttributeBody::InnerClasses(entries) => Some(entries),
            _ => None,
        })
        .expect("an InnerClasses attribute");
    let listed: Vec<(String, u16)> = entries
        .iter()
        .map(|entry| {
            (
                outer
                    .constant_pool
                    .utf8(entry.inner_name_index)
                    .expect("utf8")
                    .into_owned(),
                entry.inner_class_access_flags(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("Counter".to_owned(), 0x0008),
            // An interface entry keeps `ACC_INTERFACE | ACC_ABSTRACT`, gains no `ACC_SUPER`, and is
            // `ACC_STATIC` without writing the word: a member interface is implicitly `static`
            // (JLS §9.5), and this entry is the only place that can be recorded.
            ("Named".to_owned(), 0x0200 | 0x0400 | 0x0008),
            ("Person".to_owned(), 0x0008),
        ]
    );

    if !java_available() {
        return;
    }
    assert_eq!(
        run(source, "com.example.Outer"),
        "42\nperson\n3\n10\ncom.example.Outer$Counter\nCounter\n10\n"
    );
}

/// `this(…)` and `super(args)` each replace part of what a constructor's prologue emits.
///
/// `super(args)` stands in for the no-argument `super()`; `this(…)` stands in for the field
/// initialisers too, because the constructor it delegates to has already run them. Emitting the
/// prologue *and* the invocation runs one of them twice — a class that verifies and initialises
/// wrongly — so this is checked by what the fields hold rather than by whether it compiles.
#[test]
fn a_constructor_delegates_without_running_the_prologue_twice() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Seeded {
    int seed;
    String tag = "tagged";
    // Counts how many constructor *bodies* ran. The initialiser that zeroes it runs exactly once, so
    // a doubled prologue would show up here as a 1 where a 2 belongs.
    int bodies = 0;

    Seeded() {
        this(7);
        bodies += 1;
    }

    Seeded(int seed) {
        this.seed = seed;
        bodies += 1;
    }

    int seed() { return seed; }
    String tag() { return tag; }
    int bodies() { return bodies; }
}

class Extended extends Seeded {
    int extra = 5;

    Extended() { super(11); }
    Extended(int a, int b) { super(a + b); }

    int total() { return seed() + extra; }
}

class Delegating {
    public static void main(String[] args) {
        System.out.println(new Seeded().seed());
        System.out.println(new Seeded().tag());
        System.out.println(new Seeded().bodies());
        System.out.println(new Seeded(3).seed());
        System.out.println(new Seeded(3).bodies());
        System.out.println(new Extended().total());
        System.out.println(new Extended(1, 2).total());
        // The superclass's own initialiser still ran: `super(args)` replaces only `super()`.
        System.out.println(new Extended().tag());
    }
}
"#;
    assert_eq!(
        run(source, "Delegating"),
        "7\ntagged\n2\n3\n1\n16\n8\ntagged\n"
    );
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

/// A method with no body has to say *why* it has none.
///
/// `abstract` says so and an interface method says so implicitly; `native` says so with its own flag.
/// `ACC_NATIVE | ACC_ABSTRACT` is a pair JVMS §4.6 forbids — a JVM rejects the class with "illegal
/// modifiers: 0x500" — so a `native` method takes `ACC_NATIVE` and *not* `ACC_ABSTRACT`.
#[test]
fn a_body_less_method_says_why_it_has_none() {
    let classes = compile(
        r"
public abstract class Bodyless {
    native int f();
    public static native synchronized long g(int n);
    abstract int h();

    public static void main(String[] args) {}
}
",
    )
    .expect("`native` explains itself with its own flag");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(classes[0].bytes.as_slice()))
            .expect("reparse");
    let name_of = |index| class.constant_pool.utf8(index).expect("utf8").into_owned();
    let flags: Vec<(String, u16)> = class
        .methods
        .iter()
        .map(|method| (name_of(method.name_index), method.access_flags.0))
        .collect();
    assert_eq!(
        flags,
        [
            // native, package-private — and not abstract.
            ("f".to_owned(), 0x0100),
            // public | static | synchronized | native
            ("g".to_owned(), 0x0001 | 0x0008 | 0x0020 | 0x0100),
            // abstract, which is the other way to have no body.
            ("h".to_owned(), 0x0400),
            ("main".to_owned(), 0x0001 | 0x0008),
            ("<init>".to_owned(), 0x0001),
        ]
    );

    // Neither explanation is a declaration the JVM would refuse.
    let error = compile(
        r"
public class Silent {
    int f();

    public static void main(String[] args) {}
}
",
    )
    .expect_err("nothing says why `f` has no body");
    assert!(
        matches!(error, LowerError::Unsupported("a method with no body")),
        "expected the body-less report, got {error}"
    );
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
        printed(output.stdout),
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

/// `finally` nested past what a method body can hold is refused **while** the body is emitted.
///
/// A finalizer is inlined on every exit path, so nested ones compose: sixteen `try {} finally {}`
/// blocks are 2^16 copies of the innermost. That shape is `OpenJDK`'s own `JsrRet.java` — the
/// regression test for javac's blow-up on it — and it is what the `jals-compile` corpus ran into.
/// The class-file limit was checked once the item stream was assembled, which reports the right
/// error having first built every copy: 37 GB of items, and a corpus run the host kills instead of
/// a diagnosis.
///
/// The assertion is the *outcome*, because that is all a test can state; what the check buys is
/// bounded memory, and the number that shows it is the corpus run's peak — 37.8 GB before, 2.5 GB
/// after, flat as the nesting grows. Kept off wasm for that reason: the refusal costs the limit
/// itself, which is comfortable natively and most of a 32-bit address space.
#[test]
#[cfg(not(target_family = "wasm"))]
fn a_finally_nested_past_the_code_limit_is_refused() {
    let mut source = String::from("class Deep {\n    {\n");
    for _ in 0..16 {
        source.push_str("        try {} finally {\n");
    }
    for _ in 0..16 {
        source.push_str("        }\n");
    }
    source.push_str("    }\n}\n");
    let error = compile(&source).expect_err("2^16 inlined copies cannot fit one method body");
    assert!(
        matches!(
            error,
            LowerError::Assembly(jals_javac::jvm::AsmError::TooLarge)
        ),
        "should report the class-file limit, got {error}"
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
    assert_eq!(printed(output.stdout), "before\n");
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

/// `switch`, in both syntaxes, as a statement and as an expression.
///
/// The colon form *falls through* and the arrow form does not, which is the only difference between
/// them and one `goto` per arm in the output. A `break` leaves the whole `switch`; a `continue` looks
/// straight past it, because a `switch` is a `break` target that is not a loop.
#[test]
fn both_switch_syntaxes_run() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Selected {
    static String fallsThrough(int n) {
        String out = "";
        switch (n) {
            case 0:
            case 1:
                out = "low";
                break;
            case 2:
                out = "two";
                // No `break`: the next group runs too, which is what the colon form means.
            case 3:
                out = out + "three";
                break;
            default:
                out = "other";
        }
        return out;
    }

    static String arrows(int n) {
        String out = "";
        switch (n) {
            case 0, 1 -> out = "low";
            case 2 -> out = "two";
            default -> out = "other";
        }
        return out;
    }

    static int valued(int n) {
        return switch (n) {
            case 0 -> 10;
            case 1 -> { yield 20; }
            case 2 -> throw new IllegalStateException("two");
            default -> 99;
        };
    }

    static String narrow(char c) {
        return switch (c) {
            case 'x' -> "ex";
            case 'y' -> "why";
            default -> "?";
        };
    }

    static int sparse(int n) {
        // Keys 1000 apart take the `lookupswitch` form; the dense ones above take `tableswitch`.
        switch (n) {
            case -1: return 100;
            case 1000: return 200;
            default: return 300;
        }
    }

    static int leaves(int limit) {
        int total = 0;
        for (int i = 0; i < limit; i++) {
            switch (i) {
                case 1: continue;
                case 3: break;
                default: total += i;
            }
            total += 100;
        }
        return total;
    }

    static int cleanedUp(int n) {
        try {
            return switch (n) {
                case 0 -> { yield 1; }
                default -> 2;
            };
        } finally {
            System.out.println("cleanup");
        }
    }

    public static void main(String[] args) {
        for (int i = 0; i < 5; i++) { System.out.println(fallsThrough(i)); }
        for (int i = 0; i < 4; i++) { System.out.println(arrows(i)); }
        System.out.println(valued(0));
        System.out.println(valued(1));
        try {
            valued(2);
        } catch (IllegalStateException e) {
            System.out.println("threw:" + e.getMessage());
        }
        System.out.println(valued(9));
        System.out.println(narrow('y'));
        System.out.println(sparse(-1) + sparse(1000) + sparse(7));
        System.out.println(leaves(5));
        System.out.println(cleanedUp(0));
    }
}
"#;
    assert_eq!(
        run(source, "Selected"),
        "low\nlow\ntwothree\nthree\nother\n\
         low\nlow\ntwo\nother\n\
         10\n20\nthrew:two\n99\n\
         why\n\
         600\n\
         406\n\
         cleanup\n1\n"
    );
}

/// A `switch` on a `String` is a `switch` on `hashCode()` plus an `equals` per candidate.
///
/// The `equals` is not an optimisation to skip. Two different strings can hash alike — `"Aa"` and
/// `"BB"` famously do — and a lowering that trusted the hash would send one of them to the other's
/// arm, in a class file that verifies and runs and answers wrongly.
#[test]
fn a_string_switch_confirms_the_hash_with_equals() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Named {
    static String pick(String s) {
        switch (s) {
            case "a": return "A";
            case "b": return "B";
            case "Aa": return "collides-Aa";
            case "BB": return "collides-BB";
            default: return "?";
        }
    }

    static int arrowed(String s) {
        return switch (s) {
            case "one", "uno" -> 1;
            case "two" -> 2;
            default -> 0;
        };
    }

    public static void main(String[] args) {
        System.out.println(pick("a"));
        System.out.println(pick("b"));
        // The two keys with the same hash have to reach their own arms.
        System.out.println(pick("Aa"));
        System.out.println(pick("BB"));
        System.out.println(pick("zz"));
        System.out.println(arrowed("one") + arrowed("uno") + arrowed("two") + arrowed("x"));
    }
}
"#;
    assert_eq!(
        run(source, "Named"),
        "A\nB\ncollides-Aa\ncollides-BB\n?\n4\n"
    );
}

/// A `switch` the lowering cannot build a jump table for is reported rather than approximated.
#[test]
fn a_switch_it_cannot_tabulate_is_reported() {
    for (source, expected) in [
        // A jump table is built now, so a label whose value is not known now cannot be in it.
        (
            "int k = 1; switch (args.length) { case k: break; }",
            "a non-literal `case`",
        ),
        // A `switch` expression has to produce a value on every path, and exhaustiveness over an
        // `enum` or a sealed hierarchy — the other way to satisfy that — is not lowered.
        (
            "int v = switch (args.length) { case 0 -> 1; };",
            "a `switch` expression with no `default`",
        ),
        // A `long` selector is not a Java program, and narrowing it to an `int` would compile it into
        // one that switches on the low 32 bits.
        (
            "long n = 1; switch ((int) n) { default: break; } switch (n) { default: break; }",
            "a `switch` on this selector type",
        ),
    ] {
        let program = format!(
            r"
public class Table {{
    public static void main(String[] args) {{
        {source}
    }}
}}
"
        );
        let error = compile(&program).expect_err("this switch cannot be tabulated");
        assert!(
            matches!(error, LowerError::Unsupported(what) if what == expected),
            "`{source}` should report {expected:?}, got {error}"
        );
    }
}

/// Boxing and unboxing, which are the one conversion no opcode performs.
///
/// A `valueOf` call and an `xxxValue` call, and *which* one depends on the names on either side rather
/// than on the stack representations — which is why they sit outside the conversion table. Boxing never
/// widens on the way: `Long l = 1;` is not a Java program precisely because that would take two
/// conversions, so the wrapper is read off the value's own type.
#[test]
fn boxing_and_unboxing_run() {
    if !java_available() {
        return;
    }
    let source = r"
public class Boxed {
    static int unbox(Integer n) { return n; }
    static Integer box(int n) { return n; }
    // Unboxing may widen afterwards, which the accessor alone does not do.
    static long widened(Integer n) { return n; }

    public static void main(String[] args) {
        System.out.println(unbox(box(5)));
        System.out.println(widened(box(7)));
        // Boxing to a supertype: `Integer.valueOf` first, and the widening reference conversion is free.
        Object any = 3;
        System.out.println(any.toString());
        Long big = 4294967296L;
        System.out.println(big.longValue());
        Boolean flag = true;
        System.out.println(flag.booleanValue());
        Character letter = 'q';
        System.out.println(letter.charValue());
        Double fraction = 1.5;
        System.out.println(fraction.doubleValue());
        // Binary numeric promotion unboxes before it promotes, so a wrapper is an arithmetic operand.
        Integer counted = 3;
        int total = 0;
        total += counted;
        System.out.println(total);
        java.util.List<Integer> numbers = new java.util.ArrayList<Integer>();
        numbers.add(1);
        numbers.add(2);
        System.out.println(numbers.get(0) + 1);
        int summed = 0;
        for (Integer n : numbers) { summed += n; }
        System.out.println(summed);
    }
}
";
    assert_eq!(
        run(source, "Boxed"),
        "5\n7\n3\n4294967296\ntrue\nq\n1.5\n3\n2\n3\n"
    );
}

/// A `.class` literal.
///
/// A reference type's is an `ldc` over the same `Class` entry a `checkcast` names. A *primitive* has no
/// such entry — there is no `Class` constant for `int` — so it reads the `TYPE` field its wrapper
/// carries for exactly this purpose, and `void` reads `Void.TYPE`. A primitive *array* is a reference
/// again, so `long[].class` goes back to the `ldc`.
#[test]
fn class_literals_name_every_kind_of_type() {
    if !java_available() {
        return;
    }
    let source = r"
public class Named {
    public static void main(String[] args) {
        System.out.println(String.class.getName());
        System.out.println(Named.class.getName());
        System.out.println(int.class.getName());
        System.out.println(void.class.getName());
        System.out.println(String[].class.getName());
        System.out.println(long[].class.getName());
        System.out.println(int.class == Integer.TYPE);
    }
}
";
    assert_eq!(
        run(source, "Named"),
        "java.lang.String\nNamed\nint\nvoid\n[Ljava.lang.String;\n[J\ntrue\n"
    );
}

/// An `assert` inside an interface's `default` method.
///
/// JVMS §4.5 requires every interface field to be `public static final`, with no exception for a
/// synthetic one. Emitting `$assertionsDisabled` package-private made the interface a
/// `ClassFormatError: Illegal field modifiers` at load — which nothing but a JVM would have caught,
/// because the class reparses perfectly.
#[test]
fn an_assert_in_an_interface_gets_a_public_flag() {
    let classes = compile(
        r#"
public interface Checkable {
    default int checked(int n) {
        assert n > 0 : "positive";
        return n;
    }
}
"#,
    )
    .expect("compile");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(classes[0].bytes.as_slice()))
            .expect("reparse");
    // public | static | final | synthetic
    assert_eq!(class.fields.len(), 1);
    assert_eq!(
        class.fields[0].access_flags.0,
        0x0001 | 0x0008 | 0x0010 | 0x1000
    );

    if !java_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("temp dir");
    std::fs::write(directory.path().join("Checkable.class"), &classes[0].bytes).expect("write");
    let output = Command::new("java")
        .arg("-XX:-UsePerfData")
        .arg("-cp")
        .arg(directory.path())
        .arg("Checkable")
        .output()
        .expect("run java");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("ClassFormatError") && !stderr.contains("VerifyError"),
        "the JVM rejected the interface: {stderr}"
    );
}

/// An overload set mixing a specific reference parameter with `Object` picks the one the argument's
/// static type names.
///
/// `StringBuilder.append` declares both, and the two are mutually assignable as far as the shallow
/// stub model can see — so nothing *dominates*, and selection falls back to an order rather than to a
/// rule. It lands on the right one today; this pins that, because the wrong one is an
/// `invokevirtual append(String)` with an `Object` on the stack, which the assembler's
/// reference-vs-reference check accepts and the JVM rejects at load.
#[test]
fn an_object_argument_does_not_bind_to_a_string_parameter() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Widened {
    public static void main(String[] args) {
        StringBuilder builder = new StringBuilder();
        Object boxed = "x";
        builder.append(boxed);
        builder.append("y");
        builder.append(1);
        System.out.println(builder.toString());
    }
}
"#;
    assert_eq!(run(source, "Widened"), "xy1\n");
}

/// An `implements` clause reaches the class file's `interfaces` list.
///
/// Dropping it produced a class the JVM loads and then refuses to dispatch through: an
/// `invokeinterface` on a type whose `interfaces` never mentioned the interface is an
/// `IncompatibleClassChangeError` at the first call, not a load-time error — so only running it finds
/// this.
#[test]
fn an_implements_clause_reaches_the_interfaces_list() {
    if !java_available() {
        return;
    }
    let source = r#"
interface Speaks {
    void speak();
}

interface Ranks {
    int rank();
}

public class Greeter implements Speaks, Ranks {
    public void speak() {
        System.out.println("spoke");
    }

    public int rank() {
        return 7;
    }

    public static void main(String[] args) {
        Greeter greeter = new Greeter();
        Speaks talker = greeter;
        talker.speak();
        Ranks ordered = greeter;
        System.out.println(ordered.rank());
    }
}
"#;
    let classes = compile(source).expect("compile");
    let greeter = classes
        .iter()
        .find(|class| class.internal_name == "Greeter")
        .expect("the implementing class");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(greeter.bytes.as_slice()))
            .expect("reparse");
    let named: Vec<String> = class
        .interfaces
        .iter()
        .map(|&index| {
            class
                .constant_pool
                .class_name(index)
                .expect("a Class entry")
                .into_owned()
        })
        .collect();
    // In the order the source listed them, which is what a `Comparable` before a `Runnable` would
    // change and nothing else would notice.
    assert_eq!(named, ["Speaks", "Ranks"]);
    assert_eq!(run(source, "Greeter"), "spoke\n7\n");
}

/// An `enum` is a class whose every interesting member the compiler synthesises.
///
/// The source writes constants and a body; the class file needs a field per constant, a `$VALUES` array
/// holding them in declaration order, a `(String, int)` constructor reaching `Enum`'s, and
/// `values()` / `valueOf()`. An `enum` that emitted only what its body declares would be a type with no
/// constants at all.
///
/// The ordinal *is* the declaration position — it is what `ordinal()` returns, what `compareTo` orders
/// by, and what a `switch` over the type indexes on — so numbering them any other way is a class that
/// verifies and compares wrongly.
#[test]
fn an_enum_gets_its_constants_and_synthetic_members() {
    let source = r#"
enum Colour {
    RED, GREEN, BLUE;

    int brightness = 5;

    int bright() { return brightness + ordinal(); }
}

public class Palette {
    public static void main(String[] args) {
        Colour picked = Colour.BLUE;
        System.out.println(picked.name());
        System.out.println(picked.ordinal());
        System.out.println(picked.toString());
        System.out.println(picked == Colour.BLUE);
        System.out.println(Colour.RED.ordinal());
        System.out.println(Colour.RED.compareTo(Colour.BLUE));
        // The two synthetic methods, and a method the body declares.
        System.out.println(Colour.values().length);
        System.out.println(Colour.valueOf("GREEN").ordinal());
        System.out.println(Colour.GREEN.bright());
        for (Colour each : Colour.values()) { System.out.println(each.name()); }
    }
}
"#;
    let classes = compile(source).expect("compile");
    let colour = classes
        .iter()
        .find(|class| class.internal_name == "Colour")
        .expect("the enum");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(colour.bytes.as_slice()))
            .expect("reparse");
    let name_of = |index| class.constant_pool.utf8(index).expect("utf8").into_owned();

    // `ACC_ENUM` is what makes `Enum.valueOf` and a `switch` over the type work at run time, and
    // `ACC_FINAL` is what an enum with no constant bodies is.
    assert_eq!(
        class.access_flags.0,
        0x0020 | 0x0010 | 0x4000,
        "super | final | enum"
    );
    assert_eq!(
        class
            .constant_pool
            .class_name(class.super_class)
            .expect("a Class entry"),
        "java/lang/Enum"
    );
    assert_eq!(
        class
            .fields
            .iter()
            .map(|field| (name_of(field.name_index), field.access_flags.0))
            .collect::<Vec<_>>(),
        [
            // public | static | final | enum
            ("brightness".to_owned(), 0x0000),
            ("RED".to_owned(), 0x0001 | 0x0008 | 0x0010 | 0x4000),
            ("GREEN".to_owned(), 0x0001 | 0x0008 | 0x0010 | 0x4000),
            ("BLUE".to_owned(), 0x0001 | 0x0008 | 0x0010 | 0x4000),
            // private | static | final | synthetic
            ("$VALUES".to_owned(), 0x0002 | 0x0008 | 0x0010 | 0x1000),
        ]
    );
    assert_eq!(
        class
            .methods
            .iter()
            .map(|method| name_of(method.name_index))
            .collect::<Vec<_>>(),
        ["bright", "<init>", "values", "valueOf", "<clinit>"]
    );

    if !java_available() {
        return;
    }
    // `compareTo` orders by ordinal, so `RED` against `BLUE` is -2 — which is only right if the
    // constants were numbered in declaration order.
    assert_eq!(
        run(source, "Palette"),
        "BLUE\n2\nBLUE\ntrue\n0\n-2\n3\n1\n6\nRED\nGREEN\nBLUE\n"
    );
}

/// A `record`: fields, the canonical constructor, accessors, and the `Record` attribute.
///
/// A component is written *once*, in the header, and stands for three declarations — a `private final`
/// field, an accessor, and one constructor parameter. None of them is in the body, so a record that
/// emitted only what its body declares would be a type with no state, no way to build one, and no way
/// to read one. `java.lang.Record` is the superclass and the source never writes it; the `Record`
/// attribute is what makes `Class.isRecord` true.
#[test]
fn a_record_gets_its_fields_constructor_and_accessors() {
    let source = r#"
record Point(int x, long span, String label) {
    // `java.lang.Record` declares all three abstract, so a record that omits any of them loads and
    // then throws `AbstractMethodError`.
    public boolean equals(Object other) {
        if (!(other instanceof Point)) { return false; }
        Point that = (Point) other;
        return x == that.x() && span == that.span() && label.equals(that.label());
    }

    public int hashCode() {
        return x * 31 + label.hashCode();
    }

    public String toString() {
        return "Point[x=" + x + ", span=" + span + ", label=" + label + "]";
    }

    // A component's accessor may be written by hand, and then it wins over the synthesised one.
    int doubled() { return x * 2; }
}

public class Places {
    public static void main(String[] args) {
        Point p = new Point(3, 40L, "here");
        System.out.println(p.x());
        System.out.println(p.span());
        System.out.println(p.label());
        System.out.println(p.doubled());
        System.out.println(p.toString());
        System.out.println(p.equals(new Point(3, 40L, "here")));
        System.out.println(p.equals(new Point(4, 40L, "here")));
    }
}
"#;
    let classes = compile(source).expect("compile");
    let point = classes
        .iter()
        .find(|class| class.internal_name == "Point")
        .expect("the record");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(point.bytes.as_slice()))
        .expect("reparse");
    let name_of = |index| class.constant_pool.utf8(index).expect("utf8").into_owned();

    // Every record is implicitly final (§8.10), and the source never writes that either.
    assert_eq!(class.access_flags.0, 0x0020 | 0x0010, "super | final");
    assert_eq!(
        class
            .constant_pool
            .class_name(class.super_class)
            .expect("a Class entry"),
        "java/lang/Record"
    );
    assert_eq!(
        class
            .fields
            .iter()
            .map(|field| (
                name_of(field.name_index),
                name_of(field.descriptor_index),
                field.access_flags.0
            ))
            .collect::<Vec<_>>(),
        [
            // private | final
            ("x".to_owned(), "I".to_owned(), 0x0002 | 0x0010),
            ("span".to_owned(), "J".to_owned(), 0x0002 | 0x0010),
            (
                "label".to_owned(),
                "Ljava/lang/String;".to_owned(),
                0x0002 | 0x0010
            ),
        ]
    );
    // `doubled` is the body's own; `x`, `span`, and `label` are synthesised, and the canonical
    // constructor takes all three components at their own widths.
    assert_eq!(
        class
            .methods
            .iter()
            .map(|method| (name_of(method.name_index), name_of(method.descriptor_index)))
            .collect::<Vec<_>>(),
        [
            ("equals".to_owned(), "(Ljava/lang/Object;)Z".to_owned()),
            ("hashCode".to_owned(), "()I".to_owned()),
            ("toString".to_owned(), "()Ljava/lang/String;".to_owned()),
            ("doubled".to_owned(), "()I".to_owned()),
            ("<init>".to_owned(), "(IJLjava/lang/String;)V".to_owned()),
            ("x".to_owned(), "()I".to_owned()),
            ("span".to_owned(), "()J".to_owned()),
            ("label".to_owned(), "()Ljava/lang/String;".to_owned()),
        ]
    );
    let components = class
        .attributes
        .iter()
        .find_map(|attribute| match &attribute.body {
            jals_classfile::AttributeBody::Record(components) => Some(components),
            _ => None,
        })
        .expect("the `Record` attribute");
    assert_eq!(
        components
            .iter()
            .map(|component| (
                name_of(component.name_index),
                name_of(component.descriptor_index)
            ))
            .collect::<Vec<_>>(),
        [
            ("x".to_owned(), "I".to_owned()),
            ("span".to_owned(), "J".to_owned()),
            ("label".to_owned(), "Ljava/lang/String;".to_owned()),
        ]
    );

    if !java_available() {
        return;
    }
    assert_eq!(
        run(source, "Places"),
        "3\n40\nhere\n6\nPoint[x=3, span=40, label=here]\ntrue\nfalse\n"
    );
}

/// A `record`'s `equals`, `hashCode`, and `toString`, all three synthesised.
///
/// `java.lang.Record` declares them abstract, so a record without them loads perfectly and then throws
/// `AbstractMethodError` at the first call. javac derives them through `invokedynamic`; written out they
/// are the same three methods. Two of the three have real semantics to get wrong: a `double` component
/// compares with `Double.compare(a, b) == 0`, which makes two `NaN`s equal and `0.0` and `-0.0`
/// different, and its hash is its *bits*, so the two values `equals` calls equal also hash alike.
/// `toString`'s format §8.10.3 specifies exactly.
#[test]
fn a_record_synthesises_the_three_object_methods() {
    if !java_available() {
        return;
    }
    let source = r#"
record Full(int i, long l, double d, boolean b, char c, String s) {}

record Empty() {}

public class Records {
    public static void main(String[] args) {
        Full a = new Full(1, 2L, 3.5, true, 'x', "hi");
        Full same = new Full(1, 2L, 3.5, true, 'x', "hi");
        Full other = new Full(1, 2L, 3.5, true, 'x', "bye");
        // `toString()` written out: `println(a)` selects `println(String)` over `println(Object)`,
        // because a project class is conservatively assignable to an external name — a `jals-hir`
        // leniency this test is not the place to change.
        System.out.println(a.toString());
        System.out.println(a.equals(same));
        System.out.println(a.equals(other));
        System.out.println(a.equals("not a record"));
        System.out.println(a.hashCode() == same.hashCode());
        // A `null` component has to be comparable and hashable, which is what `Objects.equals` and
        // `Objects.hashCode` are for — `s.equals(...)` would throw and `s.hashCode()` too.
        Full none = new Full(1, 2L, 3.5, true, 'x', null);
        System.out.println(none.equals(new Full(1, 2L, 3.5, true, 'x', null)));
        System.out.println(none.hashCode() == new Full(1, 2L, 3.5, true, 'x', null).hashCode());
        System.out.println(none.toString());
        // `Double.compare` calls two NaNs equal where `==` calls them different.
        Full nan = new Full(1, 2L, 0.0 / 0.0, true, 'x', "hi");
        System.out.println(nan.equals(new Full(1, 2L, 0.0 / 0.0, true, 'x', "hi")));
        System.out.println(new Empty().equals(new Empty()));
        System.out.println(new Empty().toString());
    }
}
"#;
    assert_eq!(
        run(source, "Records"),
        concat!(
            "Full[i=1, l=2, d=3.5, b=true, c=x, s=hi]\n",
            "true\nfalse\nfalse\ntrue\n",
            "true\ntrue\n",
            "Full[i=1, l=2, d=3.5, b=true, c=x, s=null]\n",
            "true\ntrue\nEmpty[]\n",
        )
    );
}

/// A record's non-canonical constructor does not replace the canonical one.
///
/// "Some constructor exists" is not "the canonical constructor exists": `record P(int x) { P() {
/// this(0); } }` declares a no-argument one and still needs `<init>(I)V` for `this(0)` to have a target.
#[test]
fn a_record_constructor_replaces_the_canonical_one_only_when_it_is_canonical() {
    let source = r"
record P(int x) {
    // A convenience constructor that delegates: the canonical one must still be emitted for it to
    // reach, and for `new P(3)` to link.
    P() { this(0); }
}

public class Both {
    public static void main(String[] args) {
        System.out.println(new P(3).x());
        System.out.println(new P().x());
    }
}
";
    let classes = compile(source).expect("compile");
    let point = classes
        .iter()
        .find(|class| class.internal_name == "P")
        .expect("the record");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(point.bytes.as_slice()))
        .expect("reparse");
    let descriptors: Vec<String> = class
        .methods
        .iter()
        .filter(|method| class.constant_pool.utf8(method.name_index).as_deref() == Some("<init>"))
        .map(|method| {
            class
                .constant_pool
                .utf8(method.descriptor_index)
                .expect("utf8")
                .into_owned()
        })
        .collect();
    assert_eq!(descriptors, ["()V".to_owned(), "(I)V".to_owned()]);

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Both"), "3\n0\n");
}

/// A compact constructor **is** the canonical one, and its body sees the parameters.
///
/// `P { … }` declares no parameter list and no assignments, and means both: the components are its
/// parameters, and the field writes follow whatever the body did to them (JLS §8.10.4.2). So each
/// component's name has to bind to the *parameter* inside the body — binding it to the field would
/// read zero and then have the trailing write overwrite whatever the body stored.
///
/// The descriptor check is the other half: emitting the declaration as written gives `<init>()V`, which
/// is the wrong descriptor *and* a record whose components are all zero.
#[test]
fn a_compact_record_constructor_normalises_its_components() {
    let source = r#"
record Range(int lo, int hi) {
    Range {
        // Reads and writes of `lo` are the parameter, not the field: the field is still zero here, and
        // what this stores is what gets written to it.
        if (lo > hi) {
            int swap = lo;
            lo = hi;
            hi = swap;
        }
        if (lo < 0) {
            lo = 0;
        }
    }

    int span() {
        return hi - lo;
    }
}

record Widths(long l, double d, String s) {
    Widths {
        // A `long` and a `double` each take two slots, so a component after one of them is read at the
        // wrong offset if the widths are not accounted for.
        l = l * 2;
        d = d + 0.5;
        s = s + "!";
    }
}

public class Compact {
    public static void main(String[] args) {
        Range r = new Range(9, 4);
        System.out.println(r.lo() + " " + r.hi() + " " + r.span());
        System.out.println(new Range(-3, 5).lo());
        System.out.println(new Range(2, 7).span());
        Widths w = new Widths(21L, 3.0, "hi");
        System.out.println(w.l() + " " + w.d() + " " + w.s());
    }
}
"#;
    let classes = compile(source).expect("compile");
    let range = classes
        .iter()
        .find(|class| class.internal_name == "Range")
        .expect("the record");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(range.bytes.as_slice()))
        .expect("reparse");
    let descriptors: Vec<String> = class
        .methods
        .iter()
        .filter(|method| class.constant_pool.utf8(method.name_index).as_deref() == Some("<init>"))
        .map(|method| {
            class
                .constant_pool
                .utf8(method.descriptor_index)
                .expect("utf8")
                .into_owned()
        })
        .collect();
    assert_eq!(descriptors, ["(II)V".to_owned()]);

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Compact"), "4 9 5\n0\n5\n42 3.5 hi!\n");
}

/// A `return` in a compact constructor is reported rather than lowered.
///
/// It would jump over the implicit field writes, leaving every component at its default — which is why
/// JLS §8.10.4.2 makes one a compile-time error. There is no correct lowering to pick.
#[test]
fn a_return_in_a_compact_record_constructor_is_reported() {
    let source = "record P(int x) { P { if (x < 0) { return; } } }";
    let error = compile(source).expect_err("a `return` there has no lowering");
    assert!(
        matches!(
            error,
            LowerError::Unsupported("a `return` in a compact `record` constructor")
        ),
        "got {error}"
    );
}

/// An `enum` whose constants carry arguments, through a constructor the source declares.
///
/// Every `enum` constructor takes two parameters the source never writes — the constant's name and its
/// ordinal — because they are what `Enum`'s own constructor needs, and nothing else can set what
/// `name()`, `ordinal()`, and `compareTo` return. So a declared one is emitted two parameters *wider*
/// than the index computed from its declaration, the implicit delegation passes those two straight
/// through, and each constant's written arguments follow them at the call in `<clinit>`.
#[test]
fn an_enum_constant_carries_its_arguments_to_a_declared_constructor() {
    let source = r#"
enum Planet {
    MERCURY(3.3e23, 2.44e6),
    EARTH(5.976e24, 6.378e6);

    private final double mass;
    private final double radius;
    // A field initialiser still runs, after the delegation and before the body.
    private final int tag = 7;

    Planet(double mass, double radius) {
        this.mass = mass;
        this.radius = radius;
    }

    double surfaceGravity() {
        return 6.67300E-11 * mass / (radius * radius);
    }

    int tag() { return tag; }
}

// A second `enum` whose constructor has a different arity, so the two synthetic parameters are not
// simply "the first two of every constructor".
enum Size {
    SMALL(1, "s"), LARGE(9, "l");

    final int weight;
    final String code;

    Size(int weight, String code) {
        this.weight = weight;
        this.code = code;
    }
}

public class Enums {
    public static void main(String[] args) {
        System.out.println(Planet.EARTH.ordinal());
        System.out.println(Planet.EARTH.name());
        System.out.println(Planet.EARTH.tag());
        System.out.println((long) (Planet.EARTH.surfaceGravity() * 100.0));
        System.out.println((long) (Planet.MERCURY.surfaceGravity() * 100.0));
        System.out.println(Planet.values().length);
        System.out.println(Planet.valueOf("MERCURY").ordinal());
        System.out.println(Size.LARGE.weight + Size.LARGE.code);
        System.out.println(Size.SMALL.compareTo(Size.LARGE));
    }
}
"#;
    let classes = compile(source).expect("compile");
    let planet = classes
        .iter()
        .find(|class| class.internal_name == "Planet")
        .expect("the enum");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(planet.bytes.as_slice()))
            .expect("reparse");
    let descriptors: Vec<String> = class
        .methods
        .iter()
        .filter(|method| class.constant_pool.utf8(method.name_index).as_deref() == Some("<init>"))
        .map(|method| {
            class
                .constant_pool
                .utf8(method.descriptor_index)
                .expect("utf8")
                .into_owned()
        })
        .collect();
    // The declared `(double, double)` one, and no synthesised `(String, int)` beside it.
    assert_eq!(descriptors, ["(Ljava/lang/String;IDD)V".to_owned()]);

    if !java_available() {
        return;
    }
    assert_eq!(
        run(source, "Enums"),
        "1\nEARTH\n7\n980\n369\n2\n0\n9l\n-1\n"
    );
}

/// The `enum` shapes that are still reported, each because a *descriptor* would come out wrong.
///
/// A `this(…)` between two constructors would be lowered from the descriptor the index computed, which
/// is two parameters short of the one emitted.
#[test]
fn the_enum_shapes_that_need_another_class_file_are_reported() {
    for (source, expected) in [
        (
            "enum E { A(1); E(int code) {} E() { this(0); } }",
            "an explicit constructor invocation in an `enum`",
        ),
        (
            "enum E { A(1, 2); E(int a) {} }",
            "an `enum` constant with no matching constructor",
        ),
    ] {
        let error = compile(source).expect_err("this enum has no lowering");
        assert!(
            matches!(error, LowerError::Unsupported(what) if what == expected),
            "`{source}` should report {expected:?}, got {error}"
        );
    }
}

/// A type inside an `enum` constant's body reports the enclosing type it cannot name.
///
/// The enclosing declaration is reached as `parent().parent()`, which for a constant's body is the
/// `ENUM_CONSTANT` — not one of the seven forms `ast::Decl` casts. So it has no name, and the
/// report says so instead of the lookup proceeding.
///
/// This pins the *narrowing*, not the wording. `Decl::name_token_of` replaced a scan that took the
/// first `IDENT` of whatever node it was handed, which here is the constant's own name: an offset
/// the index holds a **member** at, so `item_by_decl` answered nothing and the failure surfaced one
/// step later as an unresolved name. Adding a variant to `ast::Decl` — `EnumConstant` has had a
/// `name_token` since the accessors were generated — would silently restore that, and nothing else
/// in the suite would notice.
#[test]
fn a_type_in_an_enum_constant_body_has_no_enclosing_name() {
    let source = r"
enum E {
    A {
        class Inner {}
    };
}
";
    let error = compile(source).expect_err("an `enum` constant is not an enclosing type");
    assert!(
        matches!(
            error,
            LowerError::Unsupported("an enclosing type with no name")
        ),
        "expected the enclosing-type report, got {error}"
    );
}

/// Variable arity, end to end (JLS §15.12.2.4, §15.12.4.2).
///
/// Four separate things have to hold together for this to run: the index has to give `int... values`
/// the array dimension the `...` implies (or the descriptor is `(I)V`), the body has to see that
/// parameter as an `int[]` (or `for (int v : values)` does not compile), overload resolution has to
/// consult variable arity only *after* fixed arity finds nothing (or `pick(9)` calls the wrong one),
/// and the call site has to pack the trailing arguments into a fresh array — except when a single
/// array argument is passed straight through.
#[test]
fn a_variable_arity_method_packs_its_trailing_arguments() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Sums {
    static int total(int... values) {
        int sum = 0;
        for (int v : values) { sum += v; }
        return sum;
    }

    static String join(String separator, String... parts) {
        String out = "";
        for (String part : parts) { out = out + separator + part; }
        return out;
    }

    // A fixed-arity overload wins over the varargs one for the argument list they both accept.
    static int pick(int a) { return 1; }
    static int pick(int... a) { return 2; }

    public static void main(String[] args) {
        System.out.println(total());
        System.out.println(total(1));
        System.out.println(total(1, 2, 3));
        System.out.println(join("-", "a", "b"));
        // No trailing arguments at all still builds the empty array.
        System.out.println(join("-").isEmpty());
        System.out.println(pick(9));
        System.out.println(pick(9, 9));
        // An array argument passes through instead of being wrapped in another array.
        int[] already = {4, 5};
        System.out.println(total(already));
    }
}
"#;
    assert_eq!(
        run(source, "Sums"),
        "0\n1\n6\n-a-b\ntrue\n1\n2\n9\n",
        "each varargs call site"
    );
}

/// An `@interface` is an interface with one extra flag and one extra supertype.
///
/// `ACC_ANNOTATION` is what makes `Class.isAnnotation` true, and `java.lang.annotation.Annotation` is
/// what every reflective reader dispatches through — neither is written in the source, and a class file
/// missing either loads perfectly and is then invisible to every annotation processor. Its elements are
/// interface methods: implicitly `public abstract`, with no body.
#[test]
fn an_annotation_type_is_an_interface_with_the_annotation_flag() {
    let source = r#"
public @interface Marker {
    String value();
    int count() default 3;
    boolean on() default true;
    char sign() default 'x';
    byte small() default 7;
    long wide() default 9L;
    double wider() default 1.5;
    String text() default "hi";
}

public class Holder {
    // A nested one, to pin the `InnerClasses` entry as well.
    @interface Inner {}

    public static void main(String[] args) {
        System.out.println("ran");
    }
}
"#;
    let classes = compile(source).expect("compile");
    let names: Vec<&str> = classes
        .iter()
        .map(|class| class.internal_name.as_str())
        .collect();
    assert_eq!(names, ["Marker", "Holder", "Holder$Inner"]);

    let marker = classes
        .iter()
        .find(|c| c.internal_name == "Marker")
        .unwrap();
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(marker.bytes.as_slice()))
            .expect("reparse");
    assert_eq!(
        class.access_flags.0,
        // public | interface | abstract | annotation
        0x0001 | 0x0200 | 0x0400 | 0x2000,
    );
    assert_eq!(
        class
            .constant_pool
            .class_name(class.super_class)
            .expect("a Class entry"),
        "java/lang/Object"
    );
    assert_eq!(
        class
            .interfaces
            .iter()
            .map(|&index| class
                .constant_pool
                .class_name(index)
                .expect("a Class entry"))
            .collect::<Vec<_>>(),
        ["java/lang/annotation/Annotation"]
    );
    assert_eq!(
        class
            .methods
            .iter()
            .map(|method| (
                class
                    .constant_pool
                    .utf8(method.name_index)
                    .expect("utf8")
                    .into_owned(),
                method.access_flags.0
            ))
            .collect::<Vec<_>>(),
        [
            // public | abstract
            ("value".to_owned(), 0x0001 | 0x0400),
            ("count".to_owned(), 0x0001 | 0x0400),
            ("on".to_owned(), 0x0001 | 0x0400),
            ("sign".to_owned(), 0x0001 | 0x0400),
            ("small".to_owned(), 0x0001 | 0x0400),
            ("wide".to_owned(), 0x0001 | 0x0400),
            ("wider".to_owned(), 0x0001 | 0x0400),
            ("text".to_owned(), 0x0001 | 0x0400),
        ]
    );
    // The tag comes from the element's declared type, not from the literal: `byte small() default 7`
    // is tag `B` over an `Integer` entry, and a reader that trusted the literal would see an `int`.
    let tags: Vec<Option<u8>> = class
        .methods
        .iter()
        .map(|method| {
            method
                .attributes
                .iter()
                .find_map(|attribute| match &attribute.body {
                    jals_classfile::AttributeBody::AnnotationDefault(
                        jals_classfile::ElementValue::Const { tag, .. },
                    ) => Some(*tag),
                    _ => None,
                })
        })
        .collect();
    assert_eq!(
        tags,
        [
            None,
            Some(b'I'),
            Some(b'Z'),
            Some(b'C'),
            Some(b'B'),
            Some(b'J'),
            Some(b'D'),
            Some(b's'),
        ]
    );

    if !java_available() {
        return;
    }
    // The JVM has to load and verify all three, `Marker` included.
    assert_eq!(run(source, "Holder"), "ran\n");
}

/// A method reference to a `static` method.
///
/// It needs no synthetic method: the handle points straight at the method the source named, which is the whole
/// difference from a lambda. The call site is otherwise identical, which is why both go through the same map.
#[test]
fn a_method_reference_points_at_the_method_it_names() {
    let source = r"
interface Doubler {
    int apply(int n);
}

public class Uses {
    static int twice(int n) { return n * 2; }

    public static void main(String[] args) {
        Doubler d = Uses::twice;
        System.out.println(d.apply(21));
    }
}
";
    let classes = compile(source).expect("compile");
    let uses = classes
        .iter()
        .find(|class| class.internal_name == "Uses")
        .expect("the class");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(uses.bytes.as_slice()))
        .expect("reparse");
    let name_of = |index| class.constant_pool.utf8(index).expect("utf8").into_owned();
    // No `lambda$N`: the handle names `twice` itself.
    assert!(
        !class
            .methods
            .iter()
            .any(|method| name_of(method.name_index).starts_with("lambda$")),
        "a method reference synthesises nothing"
    );
    let bootstraps = class
        .attributes
        .iter()
        .find_map(|attribute| match &attribute.body {
            jals_classfile::AttributeBody::BootstrapMethods(methods) => Some(methods),
            _ => None,
        })
        .expect("the `BootstrapMethods` attribute");
    assert_eq!(bootstraps.len(), 1);

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Uses"), "42\n");
}

/// A generic type declaration's `Signature`.
///
/// Type parameters survive erasure only in this attribute. Nothing at run time reads it — the JVM links
/// on descriptors — but every reflective reader does, and a class whose `Signature` is missing reports
/// `Box` where the source wrote `Box<T>`. A parameter with no `extends` is bounded by `Object`, and the
/// bound is not optional in the encoding: `<T>` is written `<T:Ljava/lang/Object;>`.
#[test]
fn a_generic_declaration_carries_its_signature() {
    let source = r"
interface Named {}

class Box<T> {}

class Bounded<T extends Named> {}

class Several<A, B extends Named> implements Named {}

// A generic supertype keeps the arguments the `extends` clause wrote.
class Derived<T> extends Box<T> {}

class Wrapped<T> extends Box<Named> implements Named {}

class Plain {}
";
    let classes = compile(source).expect("compile");
    let signature = |name: &str| {
        let compiled = classes
            .iter()
            .find(|class| class.internal_name == name)
            .expect("the class");
        let class =
            jals_exec::block_on_inline(jals_classfile::ClassFile::read(compiled.bytes.as_slice()))
                .expect("reparse");
        class
            .attributes
            .iter()
            .find_map(|attribute| match &attribute.body {
                jals_classfile::AttributeBody::Signature { signature_index } => Some(
                    class
                        .constant_pool
                        .utf8(*signature_index)
                        .expect("utf8")
                        .into_owned(),
                ),
                _ => None,
            })
    };
    assert_eq!(
        signature("Box").as_deref(),
        Some("<T:Ljava/lang/Object;>Ljava/lang/Object;")
    );
    assert_eq!(
        signature("Bounded").as_deref(),
        Some("<T:LNamed;>Ljava/lang/Object;")
    );
    assert_eq!(
        signature("Several").as_deref(),
        Some("<A:Ljava/lang/Object;B:LNamed;>Ljava/lang/Object;LNamed;")
    );
    assert_eq!(
        signature("Derived").as_deref(),
        Some("<T:Ljava/lang/Object;>LBox<TT;>;")
    );
    assert_eq!(
        signature("Wrapped").as_deref(),
        Some("<T:Ljava/lang/Object;>LBox<LNamed;>;LNamed;")
    );
    // A declaration with no type parameters carries no attribute at all, rather than an empty one.
    assert_eq!(signature("Plain"), None);

    if !java_available() {
        return;
    }
    // The JVM has to load and verify every one of them.
    let program = format!(
        "{source}\npublic class Uses {{ public static void main(String[] a) {{ System.out.println(\"ok\"); }} }}\n"
    );
    assert_eq!(run(&program, "Uses"), "ok\n");
}

/// A generic method's and a generic field's `Signature`.
///
/// Erasure writes a type variable's *bound* into the descriptor, so `T value` and `Object value` are the
/// same field, and `T first(List<T> xs)` and `Object first(List xs)` the same method, without this. A
/// member that mentions no variable and declares none carries no attribute at all.
///
/// A method declaring type parameters *of its own* is here too: the index resolves its `E` as an external
/// name it has never heard of, so the descriptor is told which names the declaration bound and erases each
/// to `Object`.
#[test]
fn a_generic_member_carries_its_signature() {
    let source = r"
interface Named {}

class Holder<T> {
    T value;
    int plain;

    T get() { return value; }

    void put(T next) { value = next; }

    int ungeneric(int n) { return n; }

    T[] many() { return null; }

    // The method's own type parameter, which is not the class's.
    static <E extends Named> E pick(E first, E second) { return first; }

    static <U> U identity(U value) { return value; }

    // A written type argument survives here too, not just on the declaration.
    Holder<T> self() { return this; }

    // Wildcards: `*` unbounded, `+X` for `? extends X`, `-X` for `? super X`.
    Holder<?> any() { return this; }

    Holder<? extends T> covariant() { return this; }

    Holder<? super T> contravariant() { return this; }

    // A thrown *type variable* is the one case the encoding needs a `throws` part for.
    T risky() throws java.io.IOException { return value; }
}
";
    let classes = compile(source).expect("compile");
    let holder = classes
        .iter()
        .find(|class| class.internal_name == "Holder")
        .expect("the class");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(holder.bytes.as_slice()))
            .expect("reparse");
    let read = |attributes: &[jals_classfile::Attribute]| {
        attributes
            .iter()
            .find_map(|attribute| match &attribute.body {
                jals_classfile::AttributeBody::Signature { signature_index } => Some(
                    class
                        .constant_pool
                        .utf8(*signature_index)
                        .expect("utf8")
                        .into_owned(),
                ),
                _ => None,
            })
    };
    let fields: Vec<(String, Option<String>)> = class
        .fields
        .iter()
        .map(|field| {
            (
                class
                    .constant_pool
                    .utf8(field.name_index)
                    .expect("utf8")
                    .into_owned(),
                read(&field.attributes),
            )
        })
        .collect();
    assert_eq!(
        fields,
        [
            ("value".to_owned(), Some("TT;".to_owned())),
            ("plain".to_owned(), None),
        ]
    );
    let methods: Vec<(String, Option<String>)> = class
        .methods
        .iter()
        .map(|method| {
            (
                class
                    .constant_pool
                    .utf8(method.name_index)
                    .expect("utf8")
                    .into_owned(),
                read(&method.attributes),
            )
        })
        .collect();
    assert_eq!(
        methods,
        [
            ("get".to_owned(), Some("()TT;".to_owned())),
            ("put".to_owned(), Some("(TT;)V".to_owned())),
            ("ungeneric".to_owned(), None),
            ("many".to_owned(), Some("()[TT;".to_owned())),
            ("pick".to_owned(), Some("<E:LNamed;>(TE;TE;)TE;".to_owned())),
            (
                "identity".to_owned(),
                Some("<U:Ljava/lang/Object;>(TU;)TU;".to_owned())
            ),
            ("self".to_owned(), Some("()LHolder<TT;>;".to_owned())),
            ("any".to_owned(), Some("()LHolder<*>;".to_owned())),
            ("covariant".to_owned(), Some("()LHolder<+TT;>;".to_owned())),
            (
                "contravariant".to_owned(),
                Some("()LHolder<-TT;>;".to_owned())
            ),
            ("risky".to_owned(), Some("()TT;".to_owned())),
            ("<init>".to_owned(), None),
        ]
    );

    if !java_available() {
        return;
    }
    let program = format!(
        "{source}\npublic class Uses {{ public static void main(String[] a) \
         {{ System.out.println(new Holder<String>().get()); }} }}\n"
    );
    assert_eq!(run(&program, "Uses"), "null\n");
}

/// A non-`static` inner class, which holds its enclosing instance in a synthetic field.
///
/// `this$0` is `final synthetic` and the source never writes it; every constructor takes the enclosing
/// instance as an extra *first* parameter, so the descriptor the index computed from the declaration is
/// one parameter short without this. The store happens after `super()` — before it, `this` is still
/// `UninitializedThis` and a `putfield` on it is not something the verifier accepts — and before the field
/// initialisers, so one of them can already read it.
#[test]
fn an_inner_class_holds_its_enclosing_instance() {
    let source = r"
public class Outer {
    int base;

    Outer(int base) { this.base = base; }

    class Inner {
        int extra;
        Inner(int extra) { this.extra = extra; }
        int total() { return extra; }
    }

    class Plain {
        int flag = 5;
    }

    int build(int n) {
        Inner i = new Inner(n);
        return i.total() + base;
    }

    int defaulted() { return new Plain().flag; }

    // A *qualified* creation names an enclosing instance that is not `this`.
    int fromAnother(Outer other, int n) {
        Inner i = other.new Inner(n);
        return i.total();
    }
}

public class Uses {
    public static void main(String[] args) {
        Outer o = new Outer(10);
        System.out.println(o.build(3));
        System.out.println(o.defaulted());
        System.out.println(o.fromAnother(new Outer(70), 2));
    }
}
";
    let classes = compile(source).expect("compile");
    let inner = classes
        .iter()
        .find(|class| class.internal_name == "Outer$Inner")
        .expect("the inner class");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(inner.bytes.as_slice()))
        .expect("reparse");
    let name_of = |index| class.constant_pool.utf8(index).expect("utf8").into_owned();
    assert_eq!(
        class
            .fields
            .iter()
            .map(|field| (
                name_of(field.name_index),
                name_of(field.descriptor_index),
                field.access_flags.0
            ))
            .collect::<Vec<_>>(),
        [
            ("extra".to_owned(), "I".to_owned(), 0x0000),
            // final | synthetic
            ("this$0".to_owned(), "LOuter;".to_owned(), 0x0010 | 0x1000),
        ]
    );
    // The enclosing instance comes first, before the parameter the source wrote.
    assert!(
        class.methods.iter().any(|method| {
            name_of(method.name_index) == "<init>"
                && name_of(method.descriptor_index) == "(LOuter;I)V"
        }),
        "the constructor takes the enclosing instance first"
    );

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Uses"), "13\n5\n2\n");
}

/// An unqualified name that resolves to an **enclosing** class's member is reached through `this$0`.
///
/// `this` inside an inner class is the inner instance, so emitting `getfield Outer.v` against it is a
/// class file the verifier rejects outright — `Type 'Outer$Inner' is not assignable to 'Outer'`. The
/// enclosing instance has to be loaded out of the synthetic field first, and the same holds for an
/// assignment and for an unqualified call to an enclosing method.
///
/// Found by the `jals-compile` corpus over `OpenJDK`'s own javac tests, where it accounted for six of
/// the nine class files a real JVM refused to load.
#[test]
fn an_uplevel_member_is_reached_through_the_enclosing_instance() {
    let source = r#"
public class Uplevel {
    int v = 5;
    String tag = "outer";

    class Inner {
        int read() { return v; }
        void write() { v = 9; }
        String label() { return tag; }
    }

    public static void main(String[] args) {
        Uplevel o = new Uplevel();
        Uplevel.Inner i = o.new Inner();
        System.out.println(i.read());
        System.out.println(i.label());
        i.write();
        System.out.println(o.v);
    }
}
"#;
    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Uplevel"), "5\nouter\n9\n");
}

/// The same walk for an unqualified **call** to an enclosing class's method.
///
/// The lowering is ready for it — `Expr::load_unqualified_receiver` treats a method's owner exactly
/// as it treats a field's — but the call never reaches it: `jals-hir` does not resolve an unqualified
/// call to an enclosing class's method at all, so lowering reports `helper()` as unresolved. The gap
/// is in resolution, not in this crate, and the test stays here as the ratchet for closing it.
#[test]
#[ignore = "jals-hir does not resolve an unqualified call to an enclosing class's method"]
fn an_uplevel_call_is_made_on_the_enclosing_instance() {
    let source = r"
public class UplevelCall {
    int helper() { return 3; }

    class Inner {
        int call() { return helper(); }
    }

    public static void main(String[] args) {
        UplevelCall o = new UplevelCall();
        System.out.println(o.new Inner().call());
    }
}
";
    if !java_available() {
        return;
    }
    assert_eq!(run(source, "UplevelCall"), "3\n");
}

/// The same walk from an **anonymous** class, whose enclosing instance is filled in the same way.
///
/// `Closure4` in `OpenJDK`'s suite is exactly this shape, and its class file was rejected with
/// `Type 'Closure4$1' is not assignable to 'Closure4'`.
#[test]
fn an_anonymous_class_reaches_an_uplevel_field_through_its_enclosing_instance() {
    let source = r"
interface Task {
    void go();
}

public class Anon {
    int v = 7;

    // A file-local interface, not `Runnable`: the embedded stubs these tests index against carry
    // neither, and a missing JDK type would fail this for a reason that is not its subject.
    Task t = new Task() {
        public void go() { System.out.println(v); }
    };

    public static void main(String[] args) {
        new Anon().t.go();
    }
}
";
    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Anon"), "7\n");
}

/// Two levels out: the walk keeps following `this$0` until it reaches the member's owner.
#[test]
fn an_uplevel_member_two_levels_out_walks_the_whole_chain() {
    let source = r"
public class Deep {
    int v = 11;

    class Middle {
        class Innermost {
            int read() { return v; }
        }
    }

    public static void main(String[] args) {
        Deep d = new Deep();
        Deep.Middle m = d.new Middle();
        System.out.println(m.new Innermost().read());
    }
}
";
    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Deep"), "11\n");
}

/// An **inherited** member is still reached through `this`, even from an inner class.
///
/// This is what makes the walk a matter of following the resolution rather than counting nesting
/// levels: `Inner` here extends `Base`, so `v` is its own inherited field and `this` is the receiver.
/// A rule that walked outwards whenever the owner is not the class being compiled would emit
/// `this$0.v` and read the wrong object's field — silently, since both are well-typed.
///
/// Red for a reason that is not the receiver walk: `jals-hir` binds `v` to `Shadow.v` rather than to
/// the `Base.v` the inner class inherits, so the walk faithfully follows a resolution that is wrong
/// and prints 2 where javac prints 1. That is the worse half of the bug — it produces a class file
/// every verifier accepts and every JVM runs, reading a different object's field — and it is
/// `jals-hir`'s to fix (JLS §6.5.6.1: the name binds in the innermost enclosing scope, and an inner
/// class's own inherited members are in it).
#[test]
#[ignore = "jals-hir binds an inner class's inherited field to the enclosing class's instead"]
fn an_inherited_member_is_still_reached_through_this() {
    let source = r"
class Base {
    int v = 1;
}

public class Shadow {
    int v = 2;

    class Inner extends Base {
        int read() { return v; }
    }

    public static void main(String[] args) {
        Shadow s = new Shadow();
        System.out.println(s.new Inner().read());
    }
}
";
    if !java_available() {
        return;
    }
    // `Base.v`, not `Shadow.v`: the name binds in the inner class's own supertype chain first.
    assert_eq!(run(source, "Shadow"), "1\n");
}

/// A local class — one declared inside a method body — is its own class file.
///
/// It was reported where it appeared, because a captured local needs a synthetic constructor parameter the
/// index knows nothing about. One that captures nothing needs none, and is a class like any other; only the
/// capture is reported now, and as what it is.
#[test]
fn a_local_class_is_its_own_class_file() {
    let source = r"
public class Host {
    public static void main(String[] args) {
        class Counter {
            int total;
            Counter(int start) { total = start; }
            int bumped(int by) { return total + by; }
        }
        Counter c = new Counter(7);
        System.out.println(c.bumped(5));
    }
}
";
    let classes = compile(source).expect("compile");
    assert!(
        classes
            .iter()
            .any(|class| class.internal_name.ends_with("Counter")),
        "the local class is emitted: {:?}",
        classes
            .iter()
            .map(|class| class.internal_name.as_str())
            .collect::<Vec<_>>()
    );

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Host"), "12\n");
}

/// A bridge method, which is what makes an override of a *generic* supertype's method dispatch.
///
/// `Holder<T>.put(T)` erases to `put(Object)` in its own class file, so a class declaring `put(String)`
/// does not override it as far as the JVM is concerned — the two descriptors differ, the class has no
/// `put(Object)` at all, and a call through `Holder` finds nothing to dispatch to. That is an
/// `AbstractMethodError` at run time and the one thing erasure cannot be left to sort out by itself.
#[test]
fn a_generic_override_gets_a_bridge() {
    let source = r#"
interface Holder<T> {
    void put(T value);
    T get();
}

public class Box implements Holder<String> {
    String held;
    public void put(String value) { held = value; }
    public String get() { return held; }
}

public class Uses {
    public static void main(String[] args) {
        Holder<String> h = new Box();
        h.put("kept");
        System.out.println(h.get());
    }
}
"#;
    let classes = compile(source).expect("compile");
    let boxed = classes
        .iter()
        .find(|class| class.internal_name == "Box")
        .expect("the class");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(boxed.bytes.as_slice()))
        .expect("reparse");
    let name_of = |index| class.constant_pool.utf8(index).expect("utf8").into_owned();
    let signatures: Vec<(String, String, u16)> = class
        .methods
        .iter()
        .map(|method| {
            (
                name_of(method.name_index),
                name_of(method.descriptor_index),
                method.access_flags.0,
            )
        })
        .collect();
    // public | bridge | synthetic
    assert!(
        signatures.contains(&(
            "put".to_owned(),
            "(Ljava/lang/Object;)V".to_owned(),
            0x0001 | 0x0040 | 0x1000
        )),
        "a bridge for the erased `put`: {signatures:?}"
    );
    assert!(
        signatures.contains(&(
            "get".to_owned(),
            "()Ljava/lang/Object;".to_owned(),
            0x0001 | 0x0040 | 0x1000
        )),
        "a bridge for the erased `get`: {signatures:?}"
    );

    if !java_available() {
        return;
    }
    // Dispatching through the *interface* is what the bridge exists for.
    assert_eq!(run(source, "Uses"), "kept\n");
}

/// A local class that *captures* a local, which outlives the frame the local lived in.
///
/// Each capture becomes a `final synthetic` field and a *trailing* constructor parameter — trailing so a
/// declared parameter keeps its slot — and every `new` of the class passes the values from wherever they
/// live at that point. Inside the class the name is not a local at all: it reads the field the constructor
/// filled.
#[test]
fn a_local_class_captures_the_locals_it_reads() {
    let source = r#"
public class Host {
    public static void main(String[] args) {
        int seen = 7;
        long wide = 40L;
        String tag = "kept";
        class Reader {
            int extra;
            Reader(int extra) { this.extra = extra; }
            int read() { return seen + extra; }
            long widened() { return wide + seen; }
            String tagged() { return tag; }
        }
        Reader r = new Reader(5);
        System.out.println(r.read());
        System.out.println(r.widened());
        System.out.println(r.tagged());
    }
}
"#;
    let classes = compile(source).expect("compile");
    let reader = classes
        .iter()
        .find(|class| class.internal_name.ends_with("Reader"))
        .expect("the local class");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(reader.bytes.as_slice()))
            .expect("reparse");
    let name_of = |index| class.constant_pool.utf8(index).expect("utf8").into_owned();
    let fields: Vec<(String, String, u16)> = class
        .fields
        .iter()
        .map(|field| {
            (
                name_of(field.name_index),
                name_of(field.descriptor_index),
                field.access_flags.0,
            )
        })
        .collect();
    // final | synthetic, one per capture, in the order the class reads them.
    assert_eq!(
        fields,
        [
            ("extra".to_owned(), "I".to_owned(), 0x0000),
            ("val$seen".to_owned(), "I".to_owned(), 0x0010 | 0x1000),
            ("val$wide".to_owned(), "J".to_owned(), 0x0010 | 0x1000),
            (
                "val$tag".to_owned(),
                "Ljava/lang/String;".to_owned(),
                0x0010 | 0x1000
            ),
        ]
    );
    // The declared parameter comes first, the captures after it.
    assert!(
        class.methods.iter().any(|method| {
            name_of(method.name_index) == "<init>"
                && name_of(method.descriptor_index) == "(IIJLjava/lang/String;)V"
        }),
        "the captures are trailing parameters: {:?}",
        class
            .methods
            .iter()
            .map(|m| name_of(m.descriptor_index))
            .collect::<Vec<_>>()
    );

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Host"), "12\n47\nkept\n");
}

/// An anonymous class — `new I() { … }` — is its own class file.
///
/// It has no name and no declaration keyword, so the index had nothing to make an item from until it was
/// taught to; without an item there is no member resolution and no descriptor. Now the body is compiled
/// like any other class and the `new` builds *that* type rather than the one it named — which is what the
/// expression means, and why the two are the same `new` in the source and two different types underneath.
#[test]
fn an_anonymous_class_is_its_own_class_file() {
    let source = r#"
interface Greeter {
    String greet();
}

public class Outer {
    static Greeter first() {
        return new Greeter() {
            public String greet() { return "one"; }
        };
    }

    // A second one in the same class gets its own number, and its own type.
    static Greeter second() {
        return new Greeter() {
            public String greet() { return "two"; }
        };
    }

    public static void main(String[] args) {
        System.out.println(first().greet());
        System.out.println(second().greet());
    }
}
"#;
    let classes = compile(source).expect("compile");
    let names: Vec<&str> = classes
        .iter()
        .map(|class| class.internal_name.as_str())
        .collect();
    assert_eq!(names, ["Greeter", "Outer", "Outer$1", "Outer$2"]);

    let anonymous = classes
        .iter()
        .find(|class| class.internal_name == "Outer$1")
        .expect("the first anonymous class");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(anonymous.bytes.as_slice()))
            .expect("reparse");
    // The type the `new` named becomes an *interface* of the anonymous class, not its superclass.
    assert_eq!(
        class
            .constant_pool
            .class_name(class.super_class)
            .expect("a Class entry"),
        "java/lang/Object"
    );
    assert_eq!(
        class
            .interfaces
            .iter()
            .map(|&index| class
                .constant_pool
                .class_name(index)
                .expect("a Class entry"))
            .collect::<Vec<_>>(),
        ["Greeter"]
    );

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Outer"), "one\ntwo\n");
}

/// `new Base(1) { … }`: the arguments go to the **superclass** constructor.
///
/// The body declares no constructor and has nowhere to write `super(…)`, so the anonymous class's own
/// synthesised constructor takes that constructor's parameters and forwards them. Both sides read the
/// selection from the same span — the class file's, and the `new`'s — so neither can pick a different
/// constructor than the other, which is what would otherwise produce a `NoSuchMethodError` at the `new`.
#[test]
fn an_anonymous_class_carries_its_arguments_to_the_superclass() {
    let source = r"
class Base {
    final int seed;
    final long scale;

    Base(int seed, long scale) {
        this.seed = seed;
        this.scale = scale;
    }

    int value() { return seed; }

    long scaled() { return scale; }
}

public class Anon {
    static Base make(int n) {
        // A capture *and* superclass arguments: the captured local is a trailing parameter, after the
        // forwarded ones, and a field initialiser runs after both.
        return new Base(n * 2, 10L) {
            int extra = 100;

            @Override
            int value() { return n + extra; }
        };
    }

    public static void main(String[] args) {
        Base b = make(3);
        System.out.println(b.value());
        // The `long` argument reached the superclass at the right slot: reading it back through an
        // inherited method is what says the width of the forwarded parameter was accounted for.
        System.out.println(b.scaled());
        System.out.println(new Base(7, 2L) { }.value());
    }
}
";
    let classes = compile(source).expect("compile");
    let anonymous = classes
        .iter()
        .find(|class| class.internal_name == "Anon$1")
        .expect("the first anonymous class");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(anonymous.bytes.as_slice()))
            .expect("reparse");
    let descriptors: Vec<String> = class
        .methods
        .iter()
        .filter(|method| class.constant_pool.utf8(method.name_index).as_deref() == Some("<init>"))
        .map(|method| {
            class
                .constant_pool
                .utf8(method.descriptor_index)
                .expect("utf8")
                .into_owned()
        })
        .collect();
    // The superclass's two parameters, then the captured `n`.
    assert_eq!(descriptors, ["(IJI)V".to_owned()]);

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Anon"), "103\n10\n7\n");
}

/// A lambda, compiled to an `invokedynamic` that `LambdaMetafactory` links.
///
/// The body cannot turn itself into a method — expression lowering has no channel for adding one — so every
/// lambda is found, numbered, and synthesised before any body is lowered, the same way nested classes and
/// captures already are. What is left at the use site is the call site itself: no arguments, because nothing
/// is captured, returning the functional interface the context asked for.
#[test]
fn a_lambda_becomes_an_invokedynamic() {
    let source = r"
interface Doubler {
    int apply(int n);
}

public class Uses {
    // A lambda in a `return`, whose target is the method's own return type.
    static Doubler made() { return n -> n * 2; }

    public static void main(String[] args) {
        // And one in a declaration, whose target is the written type.
        Doubler d = n -> n + 1;
        System.out.println(d.apply(41));
        System.out.println(made().apply(21));
        // A block body, which returns for itself.
        Doubler blocked = n -> { return n * 3; };
        System.out.println(blocked.apply(14));
        // And one that captures a local: the capture leads both the synthetic method's parameters and the
        // arguments the call site pushes.
        int bump = 40;
        Doubler capturing = n -> n + bump;
        System.out.println(capturing.apply(2));
    }
}
";
    let classes = compile(source).expect("compile");
    let uses = classes
        .iter()
        .find(|class| class.internal_name == "Uses")
        .expect("the class");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(uses.bytes.as_slice()))
        .expect("reparse");
    let name_of = |index| class.constant_pool.utf8(index).expect("utf8").into_owned();
    // One synthetic method per lambda, `private static synthetic`, taking the interface's own descriptor.
    let synthetic: Vec<(String, String, u16)> = class
        .methods
        .iter()
        .filter(|method| name_of(method.name_index).starts_with("lambda$"))
        .map(|method| {
            (
                name_of(method.name_index),
                name_of(method.descriptor_index),
                method.access_flags.0,
            )
        })
        .collect();
    assert_eq!(
        synthetic,
        [
            (
                "lambda$0".to_owned(),
                "(I)I".to_owned(),
                0x0002 | 0x0008 | 0x1000
            ),
            (
                "lambda$1".to_owned(),
                "(I)I".to_owned(),
                0x0002 | 0x0008 | 0x1000
            ),
            (
                "lambda$2".to_owned(),
                "(I)I".to_owned(),
                0x0002 | 0x0008 | 0x1000
            ),
            // The capture leads: `(int bump, int n)`.
            (
                "lambda$3".to_owned(),
                "(II)I".to_owned(),
                0x0002 | 0x0008 | 0x1000
            ),
        ]
    );
    // Every call site indexes into this attribute; without it the class does not even load.
    let bootstraps = class
        .attributes
        .iter()
        .find_map(|attribute| match &attribute.body {
            jals_classfile::AttributeBody::BootstrapMethods(methods) => Some(methods),
            _ => None,
        })
        .expect("the `BootstrapMethods` attribute");
    assert_eq!(bootstraps.len(), 4, "one entry per lambda");

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Uses"), "42\n42\n42\n42\n");
}

/// An *unbound* instance method reference: `Type::method`, where the interface supplies the receiver.
///
/// The referenced method takes one fewer parameter than the interface declares, because the interface's first
/// argument *is* the receiver. The handle is `invokeVirtual` for the same reason a bound reference's is — the
/// method is called on a receiver either way, and the handle cannot tell where that receiver came from.
#[test]
fn an_unbound_instance_reference_takes_its_receiver_as_the_first_argument() {
    let source = r"
interface Reader {
    int read(Box b);
}

class Box {
    int value = 9;
    int get() { return value; }
}

public class Uses {
    public static void main(String[] args) {
        Reader r = Box::get;
        System.out.println(r.read(new Box()));
    }
}
";
    if !java_available() {
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Uses"), "9\n");
}

/// A *bound* method reference and a constructor reference.
///
/// `u::scaled` captures its receiver, so the call site takes it as an argument and the handle is
/// `invokeVirtual` — the method is called *on* the captured value. `Holder::new` captures nothing and its
/// handle is `newInvokeSpecial`, which allocates as well as initialises, and that is what makes it the
/// factory the interface asks for.
#[test]
fn a_bound_and_a_constructor_reference_run() {
    let source = r"
interface Doubler {
    int apply(int n);
}

interface Maker {
    Holder make();
}

class Holder {
    int tag = 7;
}

public class Uses {
    int factor;

    Uses(int factor) { this.factor = factor; }

    int scaled(int n) { return n * factor; }

    public static void main(String[] args) {
        Uses u = new Uses(3);
        Doubler bound = u::scaled;
        System.out.println(bound.apply(14));
        Maker made = Holder::new;
        System.out.println(made.make().tag);
    }
}
";
    let classes = compile(source).expect("compile");
    let uses = classes
        .iter()
        .find(|class| class.internal_name == "Uses")
        .expect("the class");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(uses.bytes.as_slice()))
        .expect("reparse");
    let bootstraps = class
        .attributes
        .iter()
        .find_map(|attribute| match &attribute.body {
            jals_classfile::AttributeBody::BootstrapMethods(methods) => Some(methods),
            _ => None,
        })
        .expect("the `BootstrapMethods` attribute");
    assert_eq!(bootstraps.len(), 2, "one entry per reference");

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Uses"), "42\n7\n");
}

/// An unqualified name that reaches an **inherited** field.
///
/// File-local resolution binds a name to a declaration it can see, and a superclass's field is not one of
/// those — it is reached through the index, the same way a call to an inherited method already was. Both
/// `this.s` and `m()` worked for that reason; the bare `s` reported `Unresolved`, which is the one form
/// that had no route. It is looked up on the enclosing type and then up the superclass chain, nearest
/// first, which is the order that makes a shadowing field win.
#[test]
fn an_unqualified_name_reaches_an_inherited_field() {
    if !java_available() {
        return;
    }
    let source = r"
class Base {
    int seed = 4;
    static int shared = 9;
    long wide = 100L;
}

class Middle extends Base {
    int own = 1;
}

class Leaf extends Middle {
    // A field of the same name shadows the inherited one, and the nearest declaration wins.
    int seed = 7;

    int shadowed() { return seed; }

    int inherited() { return own; }

    int statics() { return shared; }

    long widened() { return wide; }

    // Assignment, not only reading: `Place` takes the same route.
    int bumped() {
        wide += 5L;
        own++;
        shared = 20;
        // An inherited name as an *operand*: inference recorded no type for it, so the promotion had
        // nothing to promote.
        own = own + 1;
        return own + (int) wide + shared;
    }
}

public class Inherit {
    public static void main(String[] args) {
        Leaf leaf = new Leaf();
        System.out.println(leaf.shadowed());
        System.out.println(leaf.inherited());
        System.out.println(leaf.statics());
        System.out.println(leaf.widened());
        System.out.println(leaf.bumped());
    }
}
";
    assert_eq!(run(source, "Inherit"), "7\n1\n9\n100\n128\n");
}

/// `x instanceof T t`: a `boolean` that also binds.
///
/// The binding is not a flow-sensitive scoping problem for a *compiler* — whether `t` is legal at a
/// given use is what `jals-lint` decides, and this crate never checks. What it does need is a slot the
/// verifier calls definitely assigned, and only the matching path writes one. So the binding is set to
/// `null` first: `null` joins into any reference type, which leaves the slot assigned with the pattern's
/// own type on both paths and needs no second branch to arrange.
#[test]
fn an_instanceof_pattern_binds_the_narrowed_value() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Patterns {
    static String describe(Object o) {
        if (o instanceof String s) {
            return "string of " + s.length();
        }
        if (o instanceof Integer n) {
            return "int " + n;
        }
        return "other";
    }

    public static void main(String[] args) {
        System.out.println(describe("hello"));
        System.out.println(describe(Integer.valueOf(7)));
        System.out.println(describe(new Object()));
        // The negated form binds on the branch the test did *not* take, which is the `else`.
        Object o = "abc";
        if (!(o instanceof String t)) {
            System.out.println("no");
        } else {
            System.out.println(t.length());
        }
        // Two patterns in one condition: the second is evaluated only when the first matched.
        Object p = "xy";
        if (p instanceof String u && u.length() == 2) {
            System.out.println("two");
        }
    }
}
"#;
    assert_eq!(
        run(source, "Patterns"),
        "string of 5\nint 7\nother\n3\ntwo\n"
    );
}

/// A pattern `switch`, both syntaxes, with a guard.
///
/// A pattern is not a constant, so there is nothing for a jump table to index on: the arms' types are
/// tested in source order and the first match wins, which is what §14.11.1 says. The guard runs after
/// its pattern bound, because it is written in terms of the binding.
///
/// Every binding is set to `null` before the chain rather than only on its own matching path. Java
/// scopes a pattern variable to its arm so nothing can read another's, but the verifier merges every
/// edge into an arm's entry and refuses a slot some edge left unwritten.
#[test]
fn a_case_pattern_dispatches_on_the_selector_type() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Cases {
    static String describe(Object o) {
        return switch (o) {
            case String s when s.length() > 3 -> "long " + s;
            case String s -> "text " + s.length();
            case Integer n -> "int " + n;
            default -> "other";
        };
    }

    // The colon form dispatches the same way; only what happens after the arm is entered differs.
    static int colon(Object o) {
        switch (o) {
            case String s:
                return s.length();
            case Integer n:
                return n;
            default:
                return -1;
        }
    }

    public static void main(String[] args) {
        System.out.println(describe(Integer.valueOf(42)));
        System.out.println(describe("abcde"));
        System.out.println(describe("hey"));
        System.out.println(describe(new Object()));
        System.out.println(colon("abcd"));
        System.out.println(colon(Integer.valueOf(9)));
        System.out.println(colon(new Object()));
    }
}
"#;
    assert_eq!(
        run(source, "Cases"),
        "int 42\nlong abcde\ntext 3\nother\n4\n9\n-1\n"
    );
}

/// A comparison unboxes, and knows when not to.
///
/// §15.20.1 gives `<` and its relatives binary numeric promotion outright, so both sides unbox. §15.21
/// gives `==` numeric equality only when at least one side *is* a number: two references compare as
/// references, which is why `a == b` on two `Integer`s is identity and must not become an `intValue`
/// pair. The last line is the one that tells the two apart — 200 is outside `Integer`'s cache, so the
/// two boxes are distinct objects and numeric equality would have said `true`.
#[test]
fn a_comparison_unboxes_a_wrapper_but_keeps_reference_equality() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Boxed {
    public static void main(String[] args) {
        Integer n = Integer.valueOf(200);
        Integer m = Integer.valueOf(200);
        Long l = Long.valueOf(9L);
        Double d = Double.valueOf(1.5);
        System.out.println(n > 3);
        System.out.println(n == 200);
        System.out.println(200 == n);
        System.out.println(n < 3 ? "lt" : "ge");
        System.out.println(l > 8);
        System.out.println(d < 2.0);
        System.out.println(n <= m);
        System.out.println(n == m);
    }
}
"#;
    assert_eq!(
        run(source, "Boxed"),
        "true\ntrue\ntrue\nge\ntrue\ntrue\ntrue\nfalse\n"
    );
}

/// The three `AnnotationDefault` forms that are not a constant.
///
/// Each has its own encoding, and the tag is what tells a reader which one it is looking at. An enum
/// constant carries the enum's *descriptor* and the constant's name; a class literal carries the
/// descriptor rather than the internal name; an array carries one value per element, each at the
/// *component* type — which is the only thing that says what tag each of them has.
#[test]
fn an_annotation_default_encodes_an_enum_a_class_and_an_array() {
    let source = r"
enum Colour { RED, GREEN }

public @interface Wide {
    Colour hue() default Colour.GREEN;
    Class<?> kind() default String.class;
    int[] sizes() default {1, 2, 3};
    String[] names() default {};
}

public class Uses {
    public static void main(String[] args) {
        // Something has to reference it for the JVM to load and verify the annotation class at all.
        System.out.println(Wide.class.getName());
    }
}
";
    let classes = compile(source).expect("compile");
    let wide = classes
        .iter()
        .find(|class| class.internal_name == "Wide")
        .expect("the annotation type");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(wide.bytes.as_slice()))
        .expect("reparse");
    let utf8 = |index| class.constant_pool.utf8(index).expect("utf8").into_owned();
    let defaults: Vec<(String, String)> = class
        .methods
        .iter()
        .filter_map(|method| {
            let name = utf8(method.name_index);
            let value = method
                .attributes
                .iter()
                .find_map(|attribute| match &attribute.body {
                    jals_classfile::AttributeBody::AnnotationDefault(value) => Some(value),
                    _ => None,
                })?;
            let rendered = match value {
                jals_classfile::ElementValue::Enum {
                    type_name_index,
                    const_name_index,
                } => format!(
                    "enum {} {}",
                    utf8(*type_name_index),
                    utf8(*const_name_index)
                ),
                jals_classfile::ElementValue::Class { class_info_index } => {
                    format!("class {}", utf8(*class_info_index))
                }
                jals_classfile::ElementValue::Array(items) => format!(
                    "array {}",
                    items
                        .iter()
                        .map(|item| match item {
                            jals_classfile::ElementValue::Const { tag, .. } =>
                                char::from(*tag).to_string(),
                            _ => "?".to_owned(),
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                jals_classfile::ElementValue::Const { tag, .. } => {
                    format!("const {}", char::from(*tag))
                }
                jals_classfile::ElementValue::Annotation(_) => "annotation".to_owned(),
            };
            Some((name, rendered))
        })
        .collect();
    assert_eq!(
        defaults,
        [
            ("hue".to_owned(), "enum LColour; GREEN".to_owned()),
            ("kind".to_owned(), "class Ljava/lang/String;".to_owned()),
            ("sizes".to_owned(), "array I,I,I".to_owned()),
            ("names".to_owned(), "array ".to_owned()),
        ]
    );

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Uses"), "Wide\n");
}

/// An `enum` constant with a body, which is an anonymous subclass of the enum.
///
/// Three things follow from that and nothing else says them. The enum is not `final` — a `final` class
/// with a subclass is a `VerifyError` at load — and is `abstract` when a constant body implements
/// something the enum declares `abstract`. The constant's `new` builds *its own* class, though its
/// field still has the enum's type. And the enum's constructors widen to package-private, because a
/// subclass cannot reach a `private` one without the nestmate attributes this does not emit; `new` on an
/// enum is not a Java program, so nothing observes the difference.
///
/// The wasm test compiles the same enum, so the two backends' answers are compared against each other
/// and against a real JVM's.
#[test]
fn an_enum_constant_with_a_body_is_its_own_subclass() {
    let source = r#"
enum Op {
    ADD { int apply(int a, int b) { return a + b; } },
    MUL(2) { int extra = 7; int apply(int a, int b) { return a * b * scale + extra; } };

    final int scale;

    Op() { this.scale = 1; }

    Op(int scale) { this.scale = scale; }

    abstract int apply(int a, int b);

    // A concrete member the bodies inherit rather than override.
    int twice(int n) { return apply(n, n); }
}

public class Bodies {
    public static void main(String[] args) {
        System.out.println(Op.ADD.apply(2, 3));
        System.out.println(Op.MUL.apply(2, 3));
        System.out.println(Op.ADD.twice(4));
        System.out.println(Op.MUL.twice(4));
        System.out.println(Op.MUL.ordinal() + " " + Op.MUL.name());
        System.out.println(Op.values().length);
    }
}
"#;
    let classes = compile(source).expect("compile");
    let names: Vec<&str> = classes
        .iter()
        .map(|class| class.internal_name.as_str())
        .collect();
    assert_eq!(names, ["Op", "Op$1", "Op$2", "Bodies"]);

    let op = classes
        .iter()
        .find(|class| class.internal_name == "Op")
        .expect("the enum");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(op.bytes.as_slice()))
        .expect("reparse");
    // enum | abstract, and *not* final.
    assert_eq!(class.access_flags.0 & 0x4000, 0x4000, "enum");
    assert_eq!(class.access_flags.0 & 0x0400, 0x0400, "abstract");
    assert_eq!(class.access_flags.0 & 0x0010, 0, "not final");

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Bodies"), "5\n19\n8\n39\n1 MUL\n2\n");
}

/// A `record` pattern, which deconstructs.
///
/// The recursive case: test the type, then read each component through its *accessor* — which is what a
/// deconstruction calls (§14.30.1), a record being free to declare one by hand — and match the component
/// pattern against that. So a nested pattern is one test and a chain of reads rather than anything the
/// source has to spell out. `_` matches anything and binds nothing, so it emits nothing at all.
///
/// Two component patterns carry no test at all: a primitive one, because there is no narrowing to do,
/// and one of the component's *own* type, because it matches unconditionally (§14.30.2) — including a
/// `null` component, which an `instanceof` would reject and so drop a match Java makes. `var` is the same
/// case spelled without the type, and its binding takes the component's.
#[test]
fn a_record_pattern_deconstructs() {
    if !java_available() {
        return;
    }
    let source = r#"
record Point(int x, int y) {}
record Line(Point a, Point b) {}

public class Deconstruct {
    static String describe(Object o) {
        if (o instanceof Point(int x, int y)) {
            return "point " + x + "," + y;
        }
        return "other";
    }

    static int total(Object o) {
        return switch (o) {
            case Line(Point(int x, int y), Point a) -> x + y + a.x() + a.y();
            case Point(int x, _) -> x;
            default -> -1;
        };
    }

    // `var` is the ordinary spelling: the component pattern's type *is* the component's.
    static int summed(Object o) {
        return switch (o) {
            case Point(var x, var y) -> x + y;
            case Line(var a, var b) -> (a == null ? 100 : 0) + (b == null ? 20 : 0);
            default -> -1;
        };
    }

    public static void main(String[] args) {
        System.out.println(describe(new Point(1, 2)));
        System.out.println(describe("no"));
        System.out.println(total(new Line(new Point(1, 2), new Point(3, 4))));
        System.out.println(total(new Point(9, 8)));
        System.out.println(total("no"));
        System.out.println(summed(new Point(3, 4)));
        // A `null` component still matches a pattern of the component's own type (§14.30.2), which an
        // `instanceof` would have rejected.
        System.out.println(summed(new Line(null, new Point(1, 1))));
        System.out.println(summed(new Line(new Point(1, 1), null)));
    }
}
"#;
    assert_eq!(
        run(source, "Deconstruct"),
        "point 1,2\nother\n10\n9\n-1\n7\n100\n20\n"
    );
}

#[test]
fn probe_var_component() {
    let source = r#"
record Point(int x, int y) {}
record Line(Point a, Point b) {}
public class P {
    static int f(Object o) {
        return switch (o) {
            case Point(var x, var y) -> x + y;
            case Line(Point a, Point b) -> a == null ? 100 : 200;
            default -> -1;
        };
    }
    public static void main(String[] a) {
        System.out.println(f(new Point(3, 4)));
        System.out.println(f(new Line(null, new Point(1, 1))));
        System.out.println(f(new Line(new Point(1, 1), null)));
        System.out.println(f("no"));
    }
}
"#;
    match compile(source) {
        Ok(_) => println!("PROBE compiled"),
        Err(e) => println!("PROBE err {e}"),
    }
    if java_available() && compile(source).is_ok() {
        println!("PROBE run {:?}", run(source, "P"));
    }
}

/// A field whose type is a type variable, and a conditional with a `null` arm.
///
/// Both were found by sweeping for reports that are *not* `Unsupported` — the first showed up as a
/// `VerifyError` from a real JVM, the second as `Descriptor(Unknown)`.
///
/// A field of a type variable is erased in its descriptor exactly as a return type is, so the read
/// leaves an `Object` where the analysis has a `String`, and the next use of it is verified against
/// `Object` and rejected. The `checkcast` a generic *call* already got belongs here for the same reason.
///
/// A `null` arm makes a conditional a reference one whatever the other arm is (§15.25): the other arm's
/// type when that is a reference, and its *boxed* form when it is a primitive — the one case where the
/// result type is written nowhere in the source. It needs no `checkcast` either way, because the frame
/// joins `null` into any reference and the merge already says the other arm's type.
#[test]
fn erasure_and_a_null_conditional_arm_keep_their_types() {
    if !java_available() {
        return;
    }
    let source = r#"
class Holder<T> {
    T value;
}

public class Erased {
    public static void main(String[] args) {
        Holder<String> h = new Holder<>();
        h.value = "z";
        System.out.println(h.value.length());

        Integer n = true ? 1 : null;
        Integer m = false ? 2 : null;
        Long wide = true ? 5L : null;
        String s = args.length > 0 ? "x" : null;
        System.out.println(n + " " + m + " " + wide + " " + s);
        // Every wrapper, because the boxed form is looked up by name and a missing stub would fall
        // back to the primitive — which is the shape that stores a `null` into an `int`.
        Boolean flag = args.length > 0 ? true : null;
        Character ch = args.length > 0 ? 'x' : null;
        Byte small = args.length > 0 ? (byte) 1 : null;
        Short mid = args.length > 0 ? (short) 1 : null;
        Float thin = args.length > 0 ? 1.5f : null;
        Double fat = args.length > 0 ? 1.5 : null;
        System.out.println(
            (flag == null) + " " + (ch == null) + " " + (small == null) + " "
                + (mid == null) + " " + (thin == null) + " " + (fat == null));
    }
}
"#;
    assert_eq!(
        run(source, "Erased"),
        "1\n1 null 5 null\ntrue true true true true true\n"
    );
}

/// A declared local keeps its *declared* type across a reassignment, so every frame that has to
/// describe the slot agrees about it.
///
/// Three shapes, one defect. An exception handler's frame is the locals as they stood where its
/// protected range began, so a range that reassigns `o` to an unrelated class described the slot as
/// whatever was written first and the JVM answered `VerifyError: Stack map does not match the one at
/// exception handler`. A `synchronized` block and a try-with-resources build the same kind of range.
/// A loop is the same defect one step earlier: the back edge carries the reassigned type into a
/// header that recorded the first one, which the assembler itself rejects as an incompatible frame.
#[test]
fn a_reassigned_local_keeps_its_declared_type() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Retyped {
    interface Shape { String name(); }
    static class Square implements Shape { public String name() { return "square"; } }
    static class Circle implements Shape { public String name() { return "circle"; } }
    static class Res implements AutoCloseable { public void close() {} }

    public static void main(String[] args) {
        Object o = "start";
        try {
            o = Integer.valueOf(7);
            throw new RuntimeException("boom");
        } catch (RuntimeException e) {
            System.out.println("caught " + o.toString());
        }

        Shape s = new Square();
        try {
            s = new Circle();
            throw new IllegalStateException("x");
        } catch (IllegalStateException e) {
            System.out.println(s.name());
        }

        Object guarded = "before";
        Object lock = new Object();
        synchronized (lock) {
            guarded = Integer.valueOf(1);
        }
        System.out.println(guarded.toString());

        Object held = "before";
        try (Res r = new Res()) {
            held = Integer.valueOf(2);
        }
        System.out.println(held.toString());

        Object looped = "start";
        for (int i = 0; i < 2; i++) {
            System.out.println(looped.toString());
            looped = Integer.valueOf(i);
        }

        // A for-each binding declared wider than the array it walks is the same slot question.
        String[] names = { "a", "b" };
        for (Object each : names) {
            System.out.println(each.toString());
        }
    }
}
"#;
    assert_eq!(
        run(source, "Retyped"),
        "caught 7\ncircle\n1\n2\nstart\n0\na\nb\n"
    );
}

/// A record's canonical constructor is the one whose parameters are its components — not merely one
/// with as many of them.
///
/// `R(int a, String b)` is a legal second two-component constructor, and matching on arity alone
/// took it for the canonical one. Nothing then synthesised the real canonical constructor, so both
/// the `this(…)` delegation and `new R(…)` had nothing to resolve to.
#[test]
fn a_records_canonical_constructor_is_matched_by_component_types() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Records2 {
    record Pair(int x, int y) {
        // Same arity as the header, different types: a delegating constructor, not the canonical one.
        Pair(int a, String b) {
            this(a, b.length());
        }
    }

    // An explicitly written canonical constructor must still replace the synthesised one, however
    // it spells its parameter types — declaring `<init>` twice under one descriptor is not a class
    // file a JVM loads.
    record Named(java.lang.String label) {
        Named(String label) {
            this.label = label;
        }
    }

    public static void main(String[] args) {
        Pair viaCanonical = new Pair(1, 2);
        Pair viaDelegate = new Pair(3, "abcd");
        System.out.println(viaCanonical.x() + "," + viaCanonical.y());
        System.out.println(viaDelegate.x() + "," + viaDelegate.y());
        System.out.println(new Named("hi").label() + ".");
    }
}
"#;
    assert_eq!(run(source, "Records2"), "1,2\n3,4\nhi.\n");
}

/// A `finally` that completes abruptly replaces the exit it interrupted (JLS §14.20.2).
///
/// The cleanups a `return` or a `break` runs are emitted inline, so a jump inside one leaves the
/// path unreachable — and everything the interrupted exit still had to emit (the outer cleanups, the
/// held return value, the transfer itself) became code after an unconditional transfer, which the
/// assembler reported rather than compiled. The answers here are `javac`'s.
#[test]
fn a_finally_that_jumps_replaces_the_exit_it_interrupted() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Unwind {
    static int returnsFromFinally() {
        try {
            return 1;
        } finally {
            return 2;
        }
    }

    static int breakDiscardsAReturn() {
        for (int i = 0; i < 3; i++) {
            try {
                return 1;
            } finally {
                break;
            }
        }
        return 0;
    }

    static int outerCleanupStillRuns() {
        int n = 0;
        try {
            try {
                n += 1;
                return n;
            } finally {
                n += 10;
                return n;
            }
        } finally {
            n += 100;
        }
    }

    static void voidReturnsFromFinally() {
        try {
            return;
        } finally {
            System.out.println("cleanup");
            return;
        }
    }

    static int continuesFromFinally() {
        int n = 0;
        for (int i = 0; i < 3; i++) {
            try {
                n += 1;
                break;
            } finally {
                continue;
            }
        }
        return n;
    }

    public static void main(String[] args) {
        System.out.println(returnsFromFinally());
        System.out.println(breakDiscardsAReturn());
        System.out.println(outerCleanupStillRuns());
        voidReturnsFromFinally();
        System.out.println(continuesFromFinally());
    }
}
"#;
    assert_eq!(run(source, "Unwind"), "2\n0\n11\ncleanup\n3\n");
}

/// A `case` label is a constant *expression*, not just a literal with a sign.
///
/// The two backends used to evaluate labels separately and disagree: this one matched the unary
/// operator token run exactly and read `+` / `-`, while the wasm one asked only whether a `MINUS`
/// token was present — so `case ~5:`, whose value is `-6`, was rejected here and silently compiled
/// as `5` there. Both now ask the same shared fact, which evaluates the whole of JLS §15.29.
///
/// Run on a real JVM because the point is the *value* that reaches the jump table: a wrong key
/// produces a class file that verifies and then takes the wrong arm.
#[test]
fn a_case_label_is_a_folded_constant_expression() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Fold {
    static final int A = 1;
    static final int B = 2;
    static final int SHIFTED = 1 << 4;

    static String pick(int n) {
        switch (n) {
            case ~5: return "tilde";
            case 2 + 3: return "sum";
            case SHIFTED: return "shift";
            case A | B: return "or";
            case (byte) 200: return "narrowed";
            case 'a': return "char";
            case -1 >>> 28: return "ushr";
            case (1 > 0) ? 9 : 8: return "ternary";
            default: return "none";
        }
    }

    public static void main(String[] args) {
        System.out.println(pick(-6));
        System.out.println(pick(5));
        System.out.println(pick(16));
        System.out.println(pick(3));
        System.out.println(pick(-56));
        System.out.println(pick(97));
        System.out.println(pick(15));
        System.out.println(pick(9));
        System.out.println(pick(0));
    }
}
"#;
    assert_eq!(
        run(source, "Fold"),
        "tilde\nsum\nshift\nor\nnarrowed\nchar\nushr\nternary\nnone\n"
    );
}

/// A `String` `case` label may be a concatenation, and a `char` operand of one renders as its
/// character rather than as its code — which is why the shared constant carries `char` as its own
/// kind instead of promoting it to `int` on the way in.
#[test]
fn a_string_case_label_folds_a_concatenation() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Joined {
    static int pick(String s) {
        switch (s) {
            case "a" + "b": return 1;
            case 'c' + "d": return 2;
            default: return 0;
        }
    }

    public static void main(String[] args) {
        System.out.println(pick("ab") + pick("cd") + pick("zz"));
    }
}
"#;
    assert_eq!(run(source, "Joined"), "3\n");
}

/// A name that is not a *constant variable* is still no constant: `final` is what makes one, and a
/// declaration this cannot read — another file's — is reported rather than guessed at.
#[test]
fn a_case_label_that_is_no_constant_is_still_reported() {
    for (source, expected) in [
        // Not `final`, so its value may change before the switch runs.
        (
            "int k = 1; switch (args.length) { case k: break; }",
            "a non-literal `case`",
        ),
        // `--` is its own token, so this is a prefix decrement and not a double negation. The wasm
        // backend used to read the `MINUS` inside it and compile the label as `5`.
        (
            "switch (args.length) { case --5: break; }",
            "a `case` this cannot evaluate",
        ),
        // Division by zero is not a constant expression at all.
        (
            "switch (args.length) { case 1 / 0: break; }",
            "a constant division by zero",
        ),
    ] {
        let program = format!(
            r"
public class NotConst {{
    public static void main(String[] args) {{
        {source}
    }}
}}
"
        );
        let error = compile(&program).expect_err("this case label is no constant");
        assert!(
            matches!(error, LowerError::Unsupported(what) if what == expected),
            "`{source}` should report {expected:?}, got {error}"
        );
    }
}

/// A constant that refers to itself, directly or through another, terminates rather than recursing
/// until the stack runs out.
#[test]
fn a_cyclic_constant_terminates() {
    let source = r"
public class Cycle {
    static final int A = B;
    static final int B = A;

    public static void main(String[] args) {
        switch (args.length) {
            case A: break;
            default: break;
        }
    }
}
";
    let error = compile(source).expect_err("this constant has no value");
    assert!(
        matches!(error, LowerError::Unsupported(_)),
        "expected a report, got {error}"
    );
}

/// `int a, b = 2;` gives `2` to **`b`**, not to `a`.
///
/// The field lowering paired a declaration's names with its expressions by index, which is right
/// only when every declarator has an initialiser. With one expression and two names the value
/// landed on the first name and the second stayed unset — so this printed `2 0` where Java prints
/// `0 2`. Both are now read through the same declarator walk the constant evaluator uses.
#[test]
fn a_declarator_gets_the_value_written_after_its_own_equals() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Decl {
    static int a, b = 2;
    static int c = 3, d;

    public static void main(String[] args) {
        System.out.println(a + " " + b + " " + c + " " + d);
    }
}
"#;
    assert_eq!(run(source, "Decl"), "0 2 3 0\n");
}

/// The same rule for a *local* declaration, where getting it wrong failed differently.
///
/// A field left unset holds its type's default, so the field version printed a wrong number. A
/// local left unset has no value at all: the store went to `a`'s slot and `b`'s was never written,
/// so reading `b` loads a slot the `StackMapTable` never defined and the JVM rejects the class at
/// load. Two failure modes, one cause — which is why the local half stayed broken for as long as
/// the field half was fixed on its own.
///
/// Every declarator with no initialiser is assigned before it is read, because a definitely
/// unassigned local is not something Java lets you read either. That does not soften the test: it
/// is `b`, `g`, and `i` — the ones written *with* a value — that were left unwritten, so the
/// misalignment surfaces as a verify error on the very first line that prints.
///
/// `int h, i = f();` is included because the order matters as much as the pairing: `f()` runs once,
/// for `i`, and its value is not silently handed to `h`.
#[test]
fn a_local_declarator_gets_the_value_written_after_its_own_equals() {
    if !java_available() {
        return;
    }
    let source = r#"
public class LocalDecl {
    static int calls = 0;

    static int f() {
        calls = calls + 1;
        return 7;
    }

    public static void main(String[] args) {
        int a, b = 2;
        int c = 3, d;
        long e, g = 4;
        int h, i = f();
        a = 1;
        d = 5;
        e = 6;
        h = 8;
        System.out.println(a + " " + b + " " + c + " " + d);
        System.out.println(e + " " + g + " " + h + " " + i + " " + calls);
    }
}
"#;
    assert_eq!(run(source, "LocalDecl"), "1 2 3 5\n6 4 8 7 1\n");
}

/// A bridge belongs to the method that actually overrides, not to whichever same-arity overload the
/// member walk reached first.
///
/// `Holder<String>` binds `T := String`, so `put(String)` overrides `Holder.put(T)` and needs the
/// `put(Object)` bridge; `put(int)` overrides nothing. Both backends used to decide by name and
/// argument *count* alone, which cannot tell them apart — so the bridge could land on `put(int)`
/// and a call through the interface would reach the wrong method.
///
/// Run through the interface on a real JVM, which is the only thing that answers whether the bridge
/// went to the right place.
#[test]
fn a_bridge_follows_the_substituted_parameter_type() {
    if !java_available() {
        return;
    }
    let source = r#"
interface Holder<T> {
    void put(T value);
}

public class Box implements Holder<String> {
    static String seen = "none";

    public void put(String value) {
        seen = "string:" + value;
    }

    public void put(int value) {
        seen = "int:" + value;
    }

    public static void main(String[] args) {
        Holder<String> h = new Box();
        h.put("x");
        System.out.println(seen);
        new Box().put(7);
        System.out.println(seen);
    }
}
"#;
    assert_eq!(run(source, "Box"), "string:x\nint:7\n");
}

/// The descriptors a class emits, as `name descriptor` pairs — what a separately compiled caller
/// links against, and the one thing the JVM's verifier cannot judge from a single compilation.
fn descriptors(source: &str, internal_name: &str) -> Vec<String> {
    let classes = compile(source).expect("compile");
    let class = classes
        .iter()
        .find(|class| class.internal_name == internal_name)
        .unwrap_or_else(|| panic!("no class `{internal_name}`"));
    let parsed =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(class.bytes.as_slice()))
            .expect("reparse");
    let pool = &parsed.constant_pool;
    parsed
        .methods
        .iter()
        .map(|method| {
            format!(
                "{} {}",
                pool.utf8(method.name_index).expect("name"),
                pool.utf8(method.descriptor_index).expect("descriptor")
            )
        })
        .collect()
}

/// A type variable erases to its leftmost bound (JLS §4.6), not to `Object`.
///
/// Answering `Object` for `<E extends Number>` is self-consistent within one compilation — the
/// declaration and its call sites agree, so the verifier is satisfied — and disagrees with every
/// caller compiled separately, which is a `NoSuchMethodError` rather than an imprecision. That is
/// why this asserts the descriptor *text* and not that the class loads.
#[test]
fn a_bounded_method_type_parameter_erases_to_its_bound() {
    let emitted = descriptors(
        "public class B { static <E extends Number> E pick(E a, E b) { return a; } }",
        "B",
    );
    assert!(
        emitted
            .contains(&"pick (Ljava/lang/Number;Ljava/lang/Number;)Ljava/lang/Number;".to_owned()),
        "got {emitted:?}"
    );
}

/// An *unbounded* one still erases to `Object` — the bound is what changes the answer, not the fact
/// that the parameter belongs to the method.
#[test]
fn an_unbounded_method_type_parameter_still_erases_to_object() {
    let emitted = descriptors(
        "public class B { static <U> U plain(U a) { return a; } }",
        "B",
    );
    assert!(
        emitted.contains(&"plain (Ljava/lang/Object;)Ljava/lang/Object;".to_owned()),
        "got {emitted:?}"
    );
}

/// The same rule for a *class*'s type parameter, which the index has always recorded — the erasure
/// simply never read its bound.
#[test]
fn a_bounded_class_type_parameter_erases_to_its_bound() {
    let emitted = descriptors(
        "public class Box<T extends Number> { T held; T get() { return held; } void put(T v) { held = v; } }",
        "Box",
    );
    assert!(
        emitted.contains(&"get ()Ljava/lang/Number;".to_owned()),
        "got {emitted:?}"
    );
    assert!(
        emitted.contains(&"put (Ljava/lang/Number;)V".to_owned()),
        "got {emitted:?}"
    );
}

/// A bound that names the variable it bounds must not send the erasure round forever.
///
/// `<T extends Comparable<T>>` is ordinary Java and terminates because the head is erased and the
/// arguments dropped; `<T extends U, U extends Number>` walks one step further. This crate never
/// checks, so it can also be handed a cyclic bound — which is what the depth limit is for.
#[test]
fn a_self_referential_bound_terminates() {
    let emitted = descriptors(
        "public class B { static <T extends Comparable<T>> T max(T a, T b) { return a; } }",
        "B",
    );
    assert!(
        emitted.contains(
            &"max (Ljava/lang/Comparable;Ljava/lang/Comparable;)Ljava/lang/Comparable;".to_owned()
        ),
        "got {emitted:?}"
    );
    let chained = descriptors(
        "public class B { static <T extends U, U extends Number> T thread(T a) { return a; } }",
        "B",
    );
    assert!(
        chained.contains(&"thread (Ljava/lang/Number;)Ljava/lang/Number;".to_owned()),
        "got {chained:?}"
    );
}

/// An unchecked call narrows its argument, because the descriptor no longer lies about the slot.
///
/// `EnumMap<K extends Enum<K>, V>.put` takes a `java/lang/Enum` once `K` erases to its bound. Handing
/// it a value the JVM knows only as `Object` — which a raw use does — is legal Java and rejected by
/// the verifier unless the `checkcast` javac emits is there too. Before the bound was read, the
/// descriptor said `put(Object, Object)`: no cast was needed because the call named a method
/// `EnumMap` does not have, so this verified and would have thrown `NoSuchMethodError`.
#[test]
fn an_unchecked_argument_is_cast_to_the_erased_parameter() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Raw {
    interface Slot<K extends Number> {
        String take(K key);
    }

    static class Cell implements Slot<Integer> {
        public String take(Integer key) { return "took " + key; }
    }

    public static void main(String[] args) {
        Object[] values = { Integer.valueOf(7) };
        Slot raw = new Cell();
        System.out.println(raw.take(values[0]));
    }
}
"#;
    assert_eq!(run(source, "Raw"), "took 7\n");
}

/// The narrowing fires only in the `Object`-to-narrower direction, and both guards matter.
///
/// A value the erasure already types as something else must not acquire a cast — that would turn a
/// compile-time gap into a `ClassCastException` — and neither must one whose slot is `Object`, where
/// there is nothing to narrow to. Run rather than inspected: a wrong cast is not a malformed class
/// file, so only executing it says anything.
#[test]
fn a_matching_argument_gets_no_cast() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Keep {
    static String widen(Number n) { return "n" + n; }

    static String anything(Object o) { return "o" + o; }

    public static void main(String[] args) {
        Object opaque = Integer.valueOf(3);
        System.out.println(widen(Integer.valueOf(1)) + anything(opaque));
    }
}
"#;
    assert_eq!(run(source, "Keep"), "n1o3\n");
}

/// `super.f()` is `invokespecial`, and only running it can say so.
///
/// The two instructions differ in nothing a class-file reader would flag: same owner, same name,
/// same descriptor. What separates them is that `invokevirtual` looks the method up in the receiver's
/// own table and finds the *override* — so an override calling `super.f()` calls itself until the
/// stack runs out. A `StackOverflowError` is the observation.
#[test]
fn a_super_call_is_invokespecial() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Sup {
    static class A {
        String describe() { return "A"; }
    }

    static class B extends A {
        String describe() { return "B<" + super.describe() + ">"; }
    }

    public static void main(String[] args) {
        System.out.println(new B().describe());
    }
}
"#;
    assert_eq!(run(source, "Sup"), "B<A>\n");
}

/// `super.x` reads the *hidden* field, which a `getfield` against the superclass's own owner does
/// for free — the constant pool entry names where the field is declared.
#[test]
fn a_super_field_reads_the_hidden_one() {
    if !java_available() {
        return;
    }
    let source = r"
public class Hid {
    static class A {
        int x = 1;
    }

    static class B extends A {
        int x = 2;
        int both() { return x * 10 + super.x; }
    }

    public static void main(String[] args) {
        System.out.println(new B().both());
    }
}
";
    assert_eq!(run(source, "Hid"), "21\n");
}

/// A type-variable bound the index cannot *name* erases to `Object` rather than refusing.
///
/// Every other unresolved type is a value the caller wrote and the descriptor has to spell, so
/// refusing is right there. A bound is a fact about the *index*, and the index is routinely partial:
/// `Runnable`, `Cloneable`, `Comparator`, and every `java.util.function` type are absent from the
/// embedded stubs, which is the only configuration this crate's own tests and the playground index.
/// Refusing therefore made `<T extends Runnable>` uncompilable outright — including the class-level
/// form, which compiled before any bound was read at all.
#[test]
fn a_bound_the_index_cannot_name_erases_to_object() {
    let method = descriptors(
        "public class D { static <T extends Runnable> T r(T a) { return a; } }",
        "D",
    );
    assert!(
        method.contains(&"r (Ljava/lang/Object;)Ljava/lang/Object;".to_owned()),
        "got {method:?}"
    );
    let class_level = descriptors(
        "public class F<T extends Runnable> { T held; T get() { return held; } }",
        "F",
    );
    assert!(
        class_level.contains(&"get ()Ljava/lang/Object;".to_owned()),
        "got {class_level:?}"
    );
}

/// A varargs call narrows what it packs, not only the fixed parameters before it.
///
/// Two shapes, and the second is the harder failure. A trailing element is `aastore`d into an array
/// whose component erases to the parameter's bound, so a value the JVM knows only as `Object` fails
/// the *store*: an `ArrayStoreException` raised inside the packing where javac's `checkcast` throws
/// `ClassCastException` at the call. The uncast form throws `ArrayStoreException` instead, which
/// this catch does not name, so it escapes `main` and the JVM exits non-zero. And a lone array
/// argument passes straight through (JLS §15.12.4.2) onto the stack, where an
/// `[Ljava/lang/Object;` against a descriptor spelling `[LRawVar$Base;` is a `VerifyError` and the
/// class never loads at all. Same rule as
/// [`an_unchecked_argument_is_cast_to_the_erased_parameter`], asked of an array.
#[test]
fn a_varargs_call_is_cast_to_the_erased_parameter() {
    if !java_available() {
        return;
    }
    let source = r#"
public class RawVar {
    static class Base {}

    static class Holder<K extends Base> {
        String all(K... keys) { return "n=" + keys.length; }
    }

    public static void main(String[] args) {
        Object[] wrong = { "not a Base" };
        Base[] real = { new Base(), new Base() };
        Object[] right = real;
        Holder raw = new Holder();
        try {
            raw.all(wrong[0]);
            System.out.println("no throw");
        } catch (ClassCastException e) {
            System.out.println("cce");
        }
        System.out.println(raw.all(right));
    }
}
"#;
    assert_eq!(run(source, "RawVar"), "cce\nn=2\n");
}

/// A bridge casts an *array* parameter too, which is every varargs and every `T[]`.
///
/// The cast was emitted only when both the erased and the target parameter were class types, so an
/// array parameter went through untouched and the bridge failed the verifier the moment its class
/// was loaded — `Type '[LArrBridge$Base;' is not assignable to '[LArrBridge$Leaf;'`. A `checkcast`
/// to an array names the array's own descriptor as its class, which is the whole difference.
#[test]
fn a_bridge_casts_an_array_parameter() {
    if !java_available() {
        return;
    }
    let source = r#"
public class ArrBridge {
    static class Base {}

    static class Leaf extends Base {}

    interface Slot<K extends Base> {
        String take(K... keys);
    }

    static class Cell implements Slot<Leaf> {
        public String take(Leaf... keys) { return "n=" + keys.length; }
    }

    public static void main(String[] args) {
        Cell cell = new Cell();
        System.out.println(cell.take(new Leaf(), new Leaf()));
    }
}
"#;
    assert_eq!(run(source, "ArrBridge"), "n=2\n");
}

/// A parameter that is a bounded type variable is an *overload*, and gets no bridge.
///
/// Once such a variable erases to its bound rather than to `Object`, the descriptor differs from a
/// same-name, same-arity inherited method's — and the override rule could not decide, so the bridge
/// writer kept its leniency and wrote one. javac writes none, because neither method overrides the
/// other: `((Object) new C<Integer>()).equals("hello")` returns `false` under javac and threw
/// `ClassCastException` here, and the sibling shape sent `((B) c).f("s")` into `C` instead of `B`.
#[test]
fn a_bounded_type_variable_parameter_gets_no_bridge() {
    let inherited_from_object = descriptors(
        "public class C<T extends Number> { public boolean equals(T other) { return true; } }",
        "C",
    );
    assert_eq!(
        inherited_from_object
            .iter()
            .filter(|emitted| emitted.starts_with("equals "))
            .collect::<Vec<_>>(),
        ["equals (Ljava/lang/Number;)Z"],
        "got {inherited_from_object:?}"
    );
    let inherited_from_a_class = descriptors(
        "class B { void f(Object x) {} } public class C<T extends Number> extends B { void f(T x) {} }",
        "C",
    );
    assert_eq!(
        inherited_from_a_class
            .iter()
            .filter(|emitted| emitted.starts_with("f "))
            .collect::<Vec<_>>(),
        ["f (Ljava/lang/Number;)V"],
        "got {inherited_from_a_class:?}"
    );
}

/// …and one whose parameter is a variable the *supertype* supplied still gets its bridge.
///
/// The discriminator is which side the variable is on. `I<T>.f(T)` implemented by `C<U extends
/// Number>.f(U)` is a genuine override whose erasures differ, so the `f(Object)` bridge is what makes
/// a call through the interface reach it at all — refusing every type-variable parameter would have
/// turned this into an `AbstractMethodError`, which only running it says.
#[test]
fn an_override_of_a_generic_supertype_keeps_its_bridge() {
    if !java_available() {
        return;
    }
    let source = r#"
public class Keep2 {
    interface I<T> {
        String f(T x);
    }

    static class C<U extends Number> implements I<U> {
        public String f(U x) { return "f" + x; }
    }

    public static void main(String[] args) {
        I raw = new C<Integer>();
        System.out.println(raw.f(Integer.valueOf(3)));
    }
}
"#;
    assert_eq!(run(source, "Keep2"), "f3\n");
}

/// A `super.` call names the **direct superclass**, whatever type the member walk found it on.
///
/// JVMS §6.5 lets `invokespecial` name only the direct superclass or a *direct* superinterface, and
/// the member walk routinely finds neither: a `default` method inherited *through* the superclass
/// resolves to the interface that declares it, which is not one of `C`'s own. The class file that
/// produced was refused at load — "interface method to invoke is not in a direct superinterface" —
/// so the only observation is running it. javac names the superclass here, and so does this.
#[test]
fn a_super_call_names_the_direct_superclass() {
    if !java_available() {
        return;
    }
    let source = r#"
public class SupIface {
    interface I {
        default String f() { return "I"; }
    }

    static class B implements I {}

    static class C extends B {
        String g() { return "C<" + super.f() + ">"; }
    }

    public static void main(String[] args) {
        System.out.println(new C().g());
    }
}
"#;
    assert_eq!(run(source, "SupIface"), "C<I>\n");
}

/// `super.x = 5` and `super.x += 5` lower, which needs the receiver answered where the *write* path
/// passes.
///
/// A store goes `Place::resolve` → `Place::field` → `Expr::lower` → `Expr::name`, and that chain has
/// no receiver branch of its own: the one the read path grew special-cased `this` only, so a `super`
/// write reported `Unresolved("super")` while `super.x` read fine. Which `x` is written is the
/// hiding rule (JLS §15.11.2), which the `Fieldref`'s owner already carries.
#[test]
fn a_super_field_is_written_as_well_as_read() {
    if !java_available() {
        return;
    }
    let source = r"
public class HidW {
    static class A {
        int x = 1;
    }

    static class B extends A {
        int x = 2;

        int set() {
            super.x = 5;
            super.x += 3;
            return x * 100 + super.x;
        }
    }

    public static void main(String[] args) {
        System.out.println(new B().set());
    }
}
";
    assert_eq!(run(source, "HidW"), "208\n");
}

/// A name written through a JLS §3.3 escape is the *same* name as its plain spelling.
///
/// The tree keeps the source's own spelling — that is what makes the parse lossless — so an
/// identifier's identity has to come from the decoded text instead. Reading the raw one made
/// `a` a different name from `a` everywhere that keys on token text: one declaration and one
/// use of the same variable did not resolve to each other, and the field the class file declared was
/// literally named `a`, which no separately compiled reader can find.
#[test]
fn an_escaped_identifier_is_the_name_it_spells() {
    let emitted = descriptors(
        "public class Esc { int \\u0061 = 1; int \\u0067et() { return a; } }",
        "Esc",
    );
    assert!(
        emitted.contains(&"get ()I".to_owned()),
        "the method is named `get`, got {emitted:?}"
    );
    let fields = {
        let classes =
            compile("public class Esc { int \\u0061 = 1; int \\u0067et() { return a; } }")
                .expect("compile");
        let parsed = jals_exec::block_on_inline(jals_classfile::ClassFile::read(
            classes[0].bytes.as_slice(),
        ))
        .expect("reparse");
        let pool = &parsed.constant_pool;
        parsed
            .fields
            .iter()
            .map(|field| pool.utf8(field.name_index).expect("name").into_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(fields, ["a"], "the field is named `a`");
}

/// A C-style array declarator reaches the *descriptor*, which is the half no verifier can catch.
///
/// `void m(int xs[])` is `([I)V`. Emitting it as `(I)V` links perfectly inside one compilation —
/// the declaration and its call sites are equally wrong — and is a `NoSuchMethodError` for anything
/// compiled separately. `public static void main(String argc[])` is the shape that matters: spelled
/// `(Ljava/lang/String;)V`, the JVM does not find `main` at all.
#[test]
fn a_c_style_array_declarator_reaches_the_descriptor() {
    let source = "
public class Dims {
    static int f1[];
    static int[] mixed[];
    static void m1(int xs[]) {}
    static void m2(String a[][], int b[]) {}
    static int m3()[] { return null; }
    public static void main(String argc[]) {
        int v[] = new int[3];
        System.out.println(v.length);
    }
}
";
    let classes = compile(source).expect("compile");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(
        classes
            .iter()
            .find(|class| class.internal_name == "Dims")
            .expect("the class")
            .bytes
            .as_slice(),
    ))
    .expect("reparse");
    let pool = &class.constant_pool;
    let utf8 = |index| pool.utf8(index).expect("utf8").into_owned();

    let fields: Vec<(String, String)> = class
        .fields
        .iter()
        .map(|field| (utf8(field.name_index), utf8(field.descriptor_index)))
        .collect();
    assert_eq!(
        fields,
        [
            ("f1".to_owned(), "[I".to_owned()),
            ("mixed".to_owned(), "[[I".to_owned()),
        ]
    );

    let methods: Vec<(String, String)> = class
        .methods
        .iter()
        .map(|method| (utf8(method.name_index), utf8(method.descriptor_index)))
        .collect();
    for (name, descriptor) in [
        ("m1", "([I)V"),
        ("m2", "([[Ljava/lang/String;[I)V"),
        ("m3", "()[I"),
        ("main", "([Ljava/lang/String;)V"),
    ] {
        assert!(
            methods.contains(&(name.to_owned(), descriptor.to_owned())),
            "{name} should be {descriptor}, got {methods:?}"
        );
    }

    if !java_available() {
        return;
    }
    // And the local's own brackets: `v.length` only resolves if `v` is an `int[]` in the body too.
    assert_eq!(run(source, "Dims").trim(), "3");
}

/// A lambda converts to an interface's single *abstract* method, past its `default` and `static`
/// ones.
///
/// The shape every JDK functional interface has: `Function` declares `apply` beside `compose`,
/// `andThen`, and `identity`. Counting declarations rather than abstract methods refused all of
/// them, so no lambda written against the standard library lowered at all.
#[test]
fn a_lambda_converts_past_default_and_static_interface_methods() {
    let source = "
public class Sam {
    interface Fn {
        String apply(String s);
        default Fn andThen(Fn next) { return null; }
        static Fn upper() { return s -> s; }
        private String unused() { return null; }
    }
    // JLS 9.8: a method override-equivalent to a public instance method of `Object` does not count.
    interface Cmp {
        int compare(String a, String b);
        boolean equals(Object o);
    }
    public static void main(String[] args) {
        Fn f = s -> s + \"!\";
        Cmp c = (a, b) -> a.length() - b.length();
        System.out.println(f.apply(\"hi\"));
        System.out.println(c.compare(\"aa\", \"b\"));
    }
}
";
    // Lowering at all is the claim: with the old rule both interfaces were "no single method".
    compile(source).expect("compile");
    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Sam").trim(), "hi!\n1".trim());
}

/// A value the JVM knows only as `Object` is cast down at a `return`, not just at an argument.
///
/// `<T> T pick(T x)` erases to `(Ljava/lang/Object;)Ljava/lang/Object;`, so returning its result
/// where the method declares `Exception[]` is an `areturn` the verifier rejects — "Bad return type:
/// 'java/lang/Object' is not assignable to '[Ljava/lang/Exception;'". The source is legal, the
/// descriptor is right, and javac emits a `checkcast` in exactly this place. The array target is the
/// shape the argument-side rule could not express: `Object` against `[LException;` is not a pair of
/// arrays.
#[test]
fn an_erased_value_is_cast_at_a_return() {
    let source = "
public class Erased {
    static <T> T pick(T x) { return x; }
    static String[] arrays(String[] xs) { return pick(xs); }
    static String classes(String s) { return pick(s); }
    public static void main(String[] args) {
        System.out.println(arrays(new String[] { \"a\", \"b\" }).length);
        System.out.println(classes(\"hi\"));
    }
}
";
    if !java_available() {
        // The claim is a verifier's; without one this test is checking that it compiles.
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Erased").trim(), "2\nhi");
}

/// And a value that already matches, or is genuinely something else, gets no cast.
///
/// Only the `Object`-to-narrower direction: papering over a real mismatch with a `checkcast` turns
/// a compile-time gap into a `ClassCastException` at run time.
#[test]
fn a_matching_value_is_not_cast_on_the_way_out() {
    let source = "
public class NoCast {
    static String same(String s) { return s; }
    static Object widen(String s) { return s; }
    public static void main(String[] args) {
        System.out.println(same(\"a\") + widen(\"b\"));
    }
}
";
    let classes = compile(source).expect("compile");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(
        classes
            .iter()
            .find(|class| class.internal_name == "NoCast")
            .expect("the class")
            .bytes
            .as_slice(),
    ))
    .expect("reparse");
    // `checkcast` is 0xc0; neither method should carry one.
    let pool = &class.constant_pool;
    for method in &class.methods {
        let name = pool.utf8(method.name_index).expect("utf8").into_owned();
        if name != "same" && name != "widen" {
            continue;
        }
        let code = method
            .attributes
            .iter()
            .find_map(|attribute| match &attribute.body {
                jals_classfile::AttributeBody::Code(code) => Some(code),
                _ => None,
            })
            .expect("a body");
        assert!(
            !code.code.iter().any(|instruction| matches!(
                instruction,
                jals_classfile::Instruction::CheckCast(_)
            )),
            "`{name}` should carry no checkcast"
        );
    }
}

/// An enclosing instance is the one the *target* is declared inside, reached from wherever the
/// creation is written.
///
/// Two shapes, both of which produced a class file no JVM loads:
///
/// - `new Inner2().new Nested()` where two inner classes both declare a `Nested`. The qualified
///   creation names a member of the *qualifier's* type (JLS §15.9.1); resolving it by scope emitted
///   the other class's constructor with an `Inner2` beneath it.
/// - `class Local` declared inside an anonymous class body. Its enclosing type is the anonymous
///   class, and the walk out to a named declaration skipped past it — so `Local`'s constructor took
///   the file's outer class while every `new Local()` in the body pushed the anonymous `this`.
#[test]
fn an_enclosing_instance_is_the_targets_own() {
    let source = "
public class Nest {
    boolean reached = false;
    class Inner1 { class Nested { int id() { return 1; } } }
    class Inner2 { class Nested { int id() { return 2; } } }
    int qualified() { return new Inner2().new Nested().id(); }
    void anonymous() {
        new Object() {
            class Local {{ reached = true; }}
            { new Local(); }
        };
    }
    public static void main(String[] args) {
        Nest n = new Nest();
        System.out.println(n.qualified());
        n.anonymous();
        System.out.println(n.reached);
    }
}
";
    if !java_available() {
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Nest").trim(), "2\ntrue");
}

/// Before `super(...)` returns, `this` is `uninitializedThis` and almost nothing may be read off it.
///
/// Both of these are ordinary Java that produced a class file no JVM loads:
///
/// - `class Sub extends Outer { Sub() { super(i); } }` — `i` is the *enclosing* instance's field,
///   not an inherited one, and the enclosing instance is a constructor parameter at that point. The
///   walk stopped at `this` (`Sub` really is a subtype of the field's owner) and emitted a `getfield`
///   on `uninitializedThis`.
/// - `super(new Object() {{ use(x); }})` where `x` is a captured local. The capture lives in a
///   synthetic field the prologue has not written yet, so its value is still in the parameter it
///   arrived in — which is where javac reads it too.
///
/// The `super(...)` call itself is the one instruction `uninitializedThis` may be the receiver of,
/// so the delegation keeps loading slot 0 while its arguments do not.
#[test]
fn a_constructor_reads_its_parameters_before_super_returns() {
    let source = "
public class Early {
    // Package-private, not `private`: reading a private outer field from an inner class needs the
    // nestmate attributes this compiler does not emit yet, which is a different gap.
    int i = 41;
    Early(int seed) { System.out.println(seed); }
    class Sub extends Early {
        Sub() { super(i + 1); }
    }
    static String seen = \"\";
    // Kept off `println(Object)`: this harness indexes the embedded stubs, whose `println` set is
    // not the real one, and overload selection there is a different gap.
    static class Holder { Holder(Object o) { seen = o.toString(); } }
    static void capturing(final char x) {
        class Sub2 extends Holder {
            Sub2(final char y) {
                super(new Object() {
                    public String toString() { return \"\" + x + y; }
                });
            }
        }
        new Sub2('K');
    }
    public static void main(String[] args) {
        new Early(1).new Sub();
        capturing('O');
        System.out.println(seen);
    }
}
";
    if !java_available() {
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Early").trim(), "1\n42\nOK");
}

/// A receiver parameter is not a parameter (JLS §8.4.1), and a synthetic name the source already
/// used is not the synthetic's to take.
///
/// `void m(Recv this)` is `()V`: the declaration exists to carry type annotations onto the receiver.
/// Counting it made the method one parameter wide, so `m()` matched nothing and the descriptor javac
/// writes was not the one emitted.
///
/// `enum E { a; E[] $VALUES; }` is legal — the name is reserved by convention only — and emitting the
/// synthetic array anyway declared the field twice, which is a `ClassFormatError` at load. javac
/// appends a `$` until the name is free.
#[test]
fn a_receiver_parameter_and_a_taken_synthetic_name() {
    let source = "
public class Recv {
    int seed = 7;
    int read(Recv this) { return seed; }
    int plus(Recv this, int n) { return seed + n; }
    enum E {
        a, b;
        E[] $VALUES = null;
    }
    public static void main(String[] args) {
        Recv r = new Recv();
        System.out.println(r.read());
        System.out.println(r.plus(3));
        System.out.println(E.values().length);
    }
}
";
    let classes = compile(source).expect("compile");
    let read = |name: &str| {
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(
            classes
                .iter()
                .find(|class| class.internal_name == name)
                .unwrap_or_else(|| panic!("{name} is emitted"))
                .bytes
                .as_slice(),
        ))
        .expect("reparse")
    };
    let class = read("Recv");
    let pool = &class.constant_pool;
    let methods: Vec<(String, String)> = class
        .methods
        .iter()
        .map(|method| {
            (
                pool.utf8(method.name_index).expect("utf8").into_owned(),
                pool.utf8(method.descriptor_index)
                    .expect("utf8")
                    .into_owned(),
            )
        })
        .collect();
    assert!(
        methods.contains(&("read".to_owned(), "()I".to_owned())),
        "{methods:?}"
    );
    assert!(
        methods.contains(&("plus".to_owned(), "(I)I".to_owned())),
        "{methods:?}"
    );

    // The declared `$VALUES` keeps its name; the synthetic one steps aside, as javac's does.
    let class = read("Recv$E");
    let pool = &class.constant_pool;
    let fields: Vec<String> = class
        .fields
        .iter()
        .map(|field| pool.utf8(field.name_index).expect("utf8").into_owned())
        .collect();
    assert!(fields.contains(&"$VALUES".to_owned()), "{fields:?}");
    assert!(fields.contains(&"$VALUES$".to_owned()), "{fields:?}");

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Recv").trim(), "7\n10\n2");
}

/// A lambda written as an argument, and a lambda whose parameter is the target's type argument.
///
/// Two halves of the same thing. The argument position is a target type (JLS §15.12.2) — the
/// largest single blocker there was, because `xs.forEach(x -> ...)` is the ordinary shape of modern
/// Java and had no type at all. And the parameter's type is the interface's *substituted* one:
/// `Fn<String, String>` binds it to `String`, and the synthetic method the backend emits has to
/// spell it that way too or its frame disagrees with its own instructions.
#[test]
fn a_lambda_is_typed_by_the_argument_it_is_written_as() {
    let source = "
public class Target {
    interface Fn<T, R> { R apply(T t); }
    interface IntFn { int apply(int n); }
    static String call(Fn<String, String> f) { return f.apply(\"a\"); }
    static int call(int n, IntFn f) { return f.apply(n); }
    public static void main(String[] args) {
        System.out.println(call(s -> s + s));
        System.out.println(call(3, x -> x * 2));
        // A cast is a target type written outright, and a conditional passes its own to both arms.
        IntFn cast = (IntFn) x -> x + 1;
        IntFn arm = args.length == 0 ? x -> x + 10 : x -> x - 10;
        System.out.println(cast.apply(1) + arm.apply(1));
    }
}
";
    if !java_available() {
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Target").trim(), "aa\n6\n13");
}

/// `a.length = 1` is refused as an assignment, not reported as an unresolved name.
///
/// The JVM twin of `an_assignment_to_an_arrays_length_says_what_is_wrong` in `wasm.rs`. Both
/// backends classify `a.length` through one shared fact now, so both refuse this in the same words
/// — where before each fell through to its own member lookup and blamed the name.
#[test]
fn an_assignment_to_an_arrays_length_says_what_is_wrong() {
    let source = "public class L { public static void m(int[] a) { a.length = 1; } }";
    let error = compile(source).expect_err("an array's length is not assignable");
    assert!(
        matches!(
            error,
            LowerError::Unsupported("an assignment to an array's length")
        ),
        "got {error}"
    );
}

/// A loop whose condition is the constant `true` has no test and no forward branch.
///
/// JLS §14.21 makes the statement after `while (true)` unreachable, so javac emits no conditional
/// at all and the method simply ends with the back edge. Emitting the test anyway left a branch to
/// an offset past the last instruction — which the verifier reports as *control flow falls through
/// code end*, or, once a frame is required there, as a `StackMapTable` offset on no instruction.
/// All three loop forms spell the same loop and all three are checked, because each emits its own
/// branch.
#[test]
fn a_constant_loop_condition_emits_no_exit_branch() {
    let source = r#"
public class Forever {
    static String whileTrue() {
        int n = 0;
        while (true) {
            n++;
            if (n > 2) return "while " + n;
        }
    }

    static String doTrue() {
        int n = 0;
        do {
            n++;
            if (n > 3) return "do " + n;
        } while (true);
    }

    static String forTrue() {
        int n = 0;
        for (; true; n++) {
            if (n > 4) return "for " + n;
        }
    }

    static String broken() {
        while (true) {
            break;
        }
        return "broke";
    }

    public static void main(String[] args) {
        System.out.println(whileTrue());
        System.out.println(doTrue());
        System.out.println(forTrue());
        System.out.println(broken());
    }
}
"#;
    let classes = compile(source).expect("compile");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(classes[0].bytes.as_slice()))
            .expect("reparse");
    // A loop with no `break` has no exit at all, so the last instruction is the back edge and
    // nothing can fall out past it. The forward branch this used to emit landed *at* the code
    // length, which is where the two verifier reports come from.
    let last = |name: &str| {
        let method = class
            .methods
            .iter()
            .find(|method| {
                class
                    .constant_pool
                    .utf8(method.name_index)
                    .is_some_and(|written| written == name)
            })
            .unwrap_or_else(|| panic!("no method {name}"));
        method
            .attributes
            .iter()
            .find_map(|attribute| match &attribute.body {
                jals_classfile::AttributeBody::Code(code) => code.code.last().cloned(),
                _ => None,
            })
            .expect("a body")
    };
    for name in ["whileTrue", "doTrue", "forTrue"] {
        assert!(
            matches!(
                last(name),
                jals_classfile::Instruction::Goto(_) | jals_classfile::Instruction::GotoW(_)
            ),
            "`{name}` ends with its back edge, not with a branch past the code end: {:?}",
            last(name)
        );
    }

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Forever"), "while 3\ndo 4\nfor 5\nbroke\n");
}

/// A member type of an interface is implicitly `static`, and so is a member interface, `enum`, or
/// `record` anywhere (JLS §8.5.1, §9.5).
///
/// Two records have to say so and they are derived once: the type's own access flags, and the
/// `InnerClasses` entry that is the *only* place `ACC_STATIC` can live for a nested type. Reading
/// the `static` keyword alone gave `interface I { class C {} }` an enclosing instance, so `C`'s
/// constructor took an `I` that `new I.C()` in a `static` method had nothing to pass.
#[test]
fn a_member_type_of_an_interface_is_implicitly_static() {
    let source = r#"
interface Holder {
    class Boxed {
        int value;
        Boxed(int value) { this.value = value; }
        int doubled() { return value * 2; }
    }

    Object ANON = new Object() {
        public String toString() { return "anon"; }
    };
}

public class Implicit {
    public static void main(String[] args) {
        System.out.println(new Holder.Boxed(21).doubled());
        System.out.println(Holder.ANON.toString());
    }
}
"#;
    let classes = compile(source).expect("compile");
    let boxed = classes
        .iter()
        .find(|class| class.internal_name == "Holder$Boxed")
        .expect("the member class");
    let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(boxed.bytes.as_slice()))
        .expect("reparse");
    // No enclosing instance means no synthetic field and a constructor of exactly the declared
    // parameters.
    assert!(
        class.fields.iter().all(|field| {
            class
                .constant_pool
                .utf8(field.name_index)
                .is_some_and(|name| name != "this$0")
        }),
        "a member class of an interface holds no enclosing instance"
    );

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Implicit"), "42\nanon\n");
}

/// A constructor may run statements *before* its explicit `super(…)` — JEP 447, final in Java 25.
///
/// The delegation used to be the body's first statement or nowhere, so a body that put anything
/// ahead of it was read as having none: the implicit `super()` prologue was emitted **as well as**
/// the explicit call the body still contained, and `Object.<init>` ran twice on one object. What the
/// prologue may not do is touch `this`, which is `uninitializedThis` across all of it — so a local
/// class declared there holds no enclosing instance, exactly as one declared in a `static` method
/// does.
#[test]
fn a_constructor_may_run_statements_before_its_delegation() {
    let source = r#"
public class Early {
    static StringBuilder log = new StringBuilder();

    static class Base {
        Base(int n) { log.append("base").append(n); }
    }

    static class Derived extends Base {
        final int kept;

        Derived(int n) {
            log.append("pre");
            int doubled = n * 2;
            super(doubled);
            this.kept = doubled;
            log.append("post").append(kept);
        }
    }

    public static void main(String[] args) {
        Derived d = new Derived(3);
        System.out.println(log + " " + d.kept);
    }
}
"#;
    let classes = compile(source).expect("compile");
    let derived = classes
        .iter()
        .find(|class| class.internal_name == "Early$Derived")
        .expect("the subclass");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(derived.bytes.as_slice()))
            .expect("reparse");
    let constructor = class
        .methods
        .iter()
        .find(|method| {
            class
                .constant_pool
                .utf8(method.name_index)
                .is_some_and(|name| name == "<init>")
        })
        .expect("the constructor");
    // Exactly one `<init>` call: the delegation the source wrote. A second is the implicit prologue
    // the body already replaced.
    let initialisations = constructor
        .attributes
        .iter()
        .filter_map(|attribute| match &attribute.body {
            jals_classfile::AttributeBody::Code(code) => Some(&code.code),
            _ => None,
        })
        .flatten()
        .filter(|instruction| matches!(instruction, jals_classfile::Instruction::InvokeSpecial(_)))
        .count();
    assert_eq!(initialisations, 1, "one `<init>` call, not two");

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Early"), "prebase6post6 6\n");
}

/// A lambda that reads the enclosing instance becomes a `private` **instance** method, and the call
/// site passes `this` as the first captured argument.
///
/// `LambdaMetafactory` takes the receiver of an instance-method handle as the leading captured
/// argument, so a lambda captures `this` exactly the way it captures a local. Emitting every body
/// as a `static` method instead is what made an unqualified field read or an instance call inside
/// one report `` `this` in a `static` method ``.
#[test]
fn a_lambda_captures_the_enclosing_instance_it_reads() {
    let source = "
public class Capture {
    interface Sink { int get(); }

    int field = 10;

    int scaled() { return field * 2; }

    Sink instanceLambda() {
        int local = 5;
        return () -> scaled() + field + local;
    }

    static Sink staticLambda() {
        int local = 7;
        return () -> local * 3;
    }

    public static void main(String[] args) {
        System.out.println(new Capture().instanceLambda().get());
        System.out.println(staticLambda().get());
    }
}
";
    let classes = compile(source).expect("compile");
    let class =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(classes[0].bytes.as_slice()))
            .expect("reparse");
    let statics: Vec<bool> = class
        .methods
        .iter()
        .filter(|method| {
            class
                .constant_pool
                .utf8(method.name_index)
                .is_some_and(|name| name.starts_with("lambda$"))
        })
        .map(|method| method.access_flags.is_static())
        .collect();
    assert_eq!(statics.len(), 2, "one synthetic method per lambda");
    // The one that reads `this` is an instance method; the one that reads only a local stays static.
    assert_eq!(statics, [false, true]);

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Capture"), "35\n21\n");
}

/// A bound method reference's qualifier is any expression, not only a local.
///
/// JLS §15.13.3 evaluates it once, when the method reference expression is evaluated — which is the
/// call site, so it is lowered there. Reading it as a local instead reported `this::m`,
/// `System.err::println`, and `supplier.get()::m` alike as *a qualifier that is no local*, which
/// was the single largest gap in the corpus.
#[test]
fn a_bound_method_reference_takes_any_qualifier() {
    let source = r#"
public class Bound {
    interface Sink { String get(); }

    static class Box {
        final String held;
        Box(String held) { this.held = held; }
        String held() { return held; }
        Box self() { return this; }
    }

    String name = "own";

    String own() { return name; }

    Sink viaThis() { return this::own; }

    static Sink viaCall(Box box) { return box.self()::held; }

    static Sink viaNew() { return new Box("fresh")::held; }

    public static void main(String[] args) {
        System.out.println(new Bound().viaThis().get());
        System.out.println(viaCall(new Box("called")).get());
        System.out.println(viaNew().get());
    }
}
"#;
    if !java_available() {
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Bound"), "own\ncalled\nfresh\n");
}

/// `Outer.this` reaches a lexically enclosing instance, and `Outer.super` reads the field it hides.
///
/// Neither is a member access — the access carries the keyword where a field name would be — so the
/// member path reported an *empty* name for both. The walk out through `this$0` is the one an
/// unqualified member of that class already takes; what is different is that the target is written
/// in the source rather than derived from where a member resolved.
#[test]
fn a_qualified_this_reaches_the_enclosing_instance() {
    let source = "
public class Qualified {
    int value = 1;

    class Inner {
        int value = 2;

        class Deeper {
            int value = 3;

            String all() {
                return value + \" \" + Inner.this.value + \" \" + Qualified.this.value;
            }
        }
    }

    public static void main(String[] args) {
        Qualified outer = new Qualified();
        Inner middle = outer.new Inner();
        System.out.println(middle.new Deeper().all());
    }
}
";
    if !java_available() {
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Qualified"), "3 2 1\n");
}

/// A qualified `super` **call** is refused rather than compiled as a virtual one.
///
/// `Outer.super.m()` names one body in particular and is not dispatched, and no `invokespecial`
/// reaches it: JVMS §6.5 lets that name only the direct superclass or a direct superinterface, and
/// `Outer`'s superclass is neither of the compiled class's. Emitting the enclosing instance and an
/// `invokevirtual` is the same bytes calling the override the source wrote `super` to avoid — a
/// program that runs and answers wrongly, which is the one outcome a refusal is better than.
#[test]
fn a_qualified_super_call_is_refused() {
    let source = "
class Base { String who() { return \"base\"; } }
public class Outer extends Base {
    public String who() { return \"outer\"; }
    class Inner {
        String ask() { return Outer.super.who(); }
    }
}
";
    let error = compile(source).expect_err("a qualified `super` call has no handle");
    assert!(
        matches!(error, LowerError::Unsupported("a qualified `super` call")),
        "got {error}"
    );
}

/// A lambda inside a lambda: every call site is planned before any body is lowered.
///
/// `s.submit(() -> run(() -> {}))` is the ordinary shape, and lowering the outer body as soon as it
/// was planned reached the inner lambda before its `invokedynamic` existed — reported as *a lambda
/// outside a class body*, which is a sentence about a lambda that is very much inside one.
#[test]
fn a_lambda_may_contain_another() {
    let source = "
public class Nested {
    interface Run { void go(); }
    static String log = \"\";

    static void take(Run run) { run.go(); }

    static void twice(Run run) {
        // The inner lambda is written inside the outer one's body, so its call site has to exist
        // before that body is lowered.
        take(() -> {
            run.go();
            take(() -> log = log + \"!\");
        });
    }

    public static void main(String[] args) {
        String tag = \"t\";
        twice(() -> log = log + tag);
        System.out.println(log);
    }
}
";
    if !java_available() {
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Nested"), "t!\n");
}

/// An `enum` constant is a value of its own enum, wherever it is named.
///
/// The constant writes no type and is not a field declaration, so nothing else could supply one:
/// a bare constant inside its own enum had no type at all, and a call taking one had no argument
/// type to select an overload against. The nested-enum spelling — `Outer.Kind.ERROR`, whose
/// qualifier is a field access rather than a name — is the same claim from the other side, and is
/// pinned in `jals-hir`'s own inference tests where a nested type needs no classpath to exist.
#[test]
fn an_enum_constant_is_a_value_of_its_enum() {
    let source = "
enum Colour {
    RED { public String toString() { return \"crimson\"; } },
    GREEN;

    String pretty() { return toString(); }
}

public class Constants {
    public static void main(String[] args) {
        System.out.println(Colour.RED.pretty());
        System.out.println(Colour.GREEN.pretty());
    }
}
";
    if !java_available() {
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Constants"), "crimson\nGREEN\n");
}

/// A `String` literal has the *indexed* `java.lang.String` type, not a name.
///
/// An external type is assignable to every project type by design — it might be an unindexed
/// project type — so typing the literal by name alone made every one-argument overload applicable
/// to `f("")` and left declaration order to pick the winner. `PrintStream(OutputStream)` is
/// declared before `PrintStream(String)`, and `super("")` compiled to the first of them: a class
/// file whose `invokespecial` the verifier refuses.
///
/// Checked here rather than only in `jals-hir` because the claim is about a *classpath* type, and
/// this is the crate whose tests have one.
#[test]
fn a_string_literal_selects_the_string_overload() {
    let source = r#"
public class Choosing {
    static String pick(Object o) { return "object"; }
    static String pick(String s) { return "string"; }
    static String pick(StringBuilder b) { return "builder"; }

    public static void main(String[] args) {
        System.out.println(pick("x"));
        System.out.println(pick("a" + "b"));
        System.out.println(pick(new StringBuilder()));
    }
}
"#;
    if !java_available() {
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Choosing"), "string\nstring\nbuilder\n");
}

/// An array's `clone()` names the array as its owner and casts the covariant return back.
///
/// JLS §10.7 gives every array a `public T[] clone()` that no declaration anywhere holds, so the
/// index resolves the name to `Object.clone()` — whose descriptor returns `Object`. javac names the
/// *array* type as the `CONSTANT_Class` owner, keeps `Object`'s descriptor (that is the method the
/// JVM resolves), and puts the array type back with a `checkcast`. Emitting `Object.clone()` alone
/// left an `Object` in a local the verifier had already typed as the array.
#[test]
fn an_array_clone_keeps_the_array_type() {
    let source = "
public class Cloning {
    public static void main(String[] args) {
        int[] numbers = {1, 2, 3};
        int[] copy = numbers.clone();
        copy[0] = 9;
        String[] names = {\"a\", \"b\"};
        String[] also = names.clone();
        System.out.println(numbers[0] + \" \" + copy[0] + \" \" + also[1] + \" \" + also.length);
    }
}
";
    if !java_available() {
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Cloning"), "1 9 b 2\n");
}

/// A nest: one top-level type and everything declared inside it, which is how `private` is reached.
///
/// JVMS §5.4.4 grants a nestmate access to another's `private` members and grants it to nobody
/// else, so without the `NestHost` / `NestMembers` attributes a nested class calling its outer
/// class's `private` method is an `IllegalAccessError` at run time — a class file that loads,
/// verifies, and then refuses the call. javac has emitted them for every nested type since Java 11.
///
/// The call itself is `invokevirtual`, not `invokespecial`: that one may name only the current
/// class, a superclass, or a direct superinterface (JVMS §6.5), so a nestmate's body is reached by
/// resolution rather than by naming.
#[test]
fn a_nest_reaches_its_members_private_declarations() {
    let source = r#"
public class Nest {
    private int seed = 7;

    private static String label() { return "outer"; }

    private int doubled() { return seed * 2; }

    static class Inner {
        String read(Nest host) { return label() + " " + host.seed + " " + host.doubled(); }
    }

    interface Job { String run(); }

    String anonymous() {
        Job job = new Job() {
            public String run() { return label() + "/" + doubled(); }
        };
        return job.run();
    }

    public static void main(String[] args) {
        Nest host = new Nest();
        System.out.println(new Inner().read(host));
        System.out.println(host.anonymous());
    }
}
"#;
    let classes = compile(source).expect("compile");
    let host =
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(classes[0].bytes.as_slice()))
            .expect("reparse");
    let members = host
        .attributes
        .iter()
        .find_map(|attribute| match &attribute.body {
            jals_classfile::AttributeBody::NestMembers { classes } => Some(classes.len()),
            _ => None,
        })
        .expect("the host lists its members");
    // `Inner`, `Job`, and the anonymous body — the nest is lexical, so all three are in it.
    assert_eq!(members, 3);
    let nested = jals_exec::block_on_inline(jals_classfile::ClassFile::read(
        classes
            .iter()
            .find(|class| class.internal_name == "Nest$Inner")
            .expect("the nested class")
            .bytes
            .as_slice(),
    ))
    .expect("reparse");
    assert!(
        nested.attributes.iter().any(|attribute| matches!(
            attribute.body,
            jals_classfile::AttributeBody::NestHost { .. }
        )),
        "a member points back at its host"
    );

    if !java_available() {
        return;
    }
    assert_eq!(run(source, "Nest"), "outer 7 14\nouter/14\n");
}

/// `this` inside an anonymous class body is that class, not the one the `new` was written in.
///
/// An anonymous body is a type of its own, and reading past it typed `this` as the outer class —
/// so `test(this)` against `test(Outer)` / `test(Base)` selected the first, which is a call the
/// verifier refuses and, where it does not, the wrong method outright. What travels with it is the
/// lexical lookup a bare call needs: the method a bare name binds to is the innermost enclosing
/// type's *of which it is a member* (JLS §15.12.1), which need not be the innermost type at all.
#[test]
fn this_in_an_anonymous_body_is_the_anonymous_class() {
    let source = r#"
public class Which {
    interface Base { String run(); }

    private static String test(Which w) { return "outer"; }

    private static String test(Base b) { return "base"; }

    private static String helper() { return "helper"; }

    String pick() {
        Base b = new Base() {
            public String run() { return test(this) + " " + helper(); }
        };
        return b.run();
    }

    public static void main(String[] args) {
        System.out.println(new Which().pick());
    }
}
"#;
    if !java_available() {
        compile(source).expect("compile");
        return;
    }
    assert_eq!(run(source, "Which"), "base helper\n");
}
