//! End-to-end: Java source in, a WebAssembly module a real engine validates and runs out.
//!
//! The counterpart of `compile.rs`. Where that one hands a class file to a JVM, this one hands a
//! module to `wasm-tools` (the specification's own validator) and to `wasmtime` (an engine), which
//! together are the only authority on whether the bytes mean what the compiler intended.

use std::io::Write as _;
use std::process::{Command, Stdio};

use jals_hir::{FileId, ProjectIndex, Resolved, TypeInference};
use jals_javac::wasm::{CompileWasm, WasmError, WasmInput};
use jals_syntax::SyntaxNode;

/// Whether `name` is on this host. A missing engine is a missing *oracle*, not a broken compiler,
/// so the tests that need one stand down — but they say so. A silent stand-down reads as "passed",
/// and the defects these tests exist to catch are exactly the ones only an engine sees.
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

/// Compile every source as one module — which is what "the whole project" means for a target with
/// no dynamic loading and no classpath.
fn compile(sources: &[&str]) -> Result<Vec<u8>, WasmError> {
    let roots: Vec<(FileId, SyntaxNode)> = sources
        .iter()
        .enumerate()
        .map(|(index, text)| {
            (
                FileId(u32::try_from(index).unwrap()),
                jals_exec::block_on_inline(jals_syntax::Parse::parse(text)).syntax(),
            )
        })
        .collect();
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&roots).with_stdlib().build());

    let analyses: Vec<(Resolved, TypeInference)> = roots
        .iter()
        .map(|(file, root)| {
            let resolved = jals_exec::block_on_inline(Resolved::resolve_node(root));
            let inference =
                jals_exec::block_on_inline(TypeInference::infer(root, &resolved, &index, *file));
            (resolved, inference)
        })
        .collect();
    let inputs: Vec<WasmInput<'_>> = roots
        .iter()
        .zip(&analyses)
        .map(|((file, root), (resolved, inference))| WasmInput {
            file: *file,
            root,
            resolved,
            inference,
        })
        .collect();
    CompileWasm::project(&inputs, &index)
}

/// `wasm-tools validate` is the specification's own answer to "is this a module".
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

/// Compile, validate, run `function` on `args`, and assert what it returned.
///
/// A host without `wasmtime` returns early rather than failing, the same shape `javac_available`
/// uses elsewhere in this workspace: a missing engine is a missing *oracle*, not a broken compiler.
/// The compile and the `wasm-tools` validation still run, so the test keeps its teeth either way.
fn assert_invoke(sources: &[&str], function: &str, args: &[&str], expected: &str) {
    let Some(output) = invoke(sources, function, args) else {
        return;
    };
    assert_eq!(output, expected);
}

/// Compile, validate, then call the exported `function` with `args` and return what it printed.
/// `None` when no engine is installed.
fn invoke(sources: &[&str], function: &str, args: &[&str]) -> Option<String> {
    let bytes = compile(sources).unwrap_or_else(|error| panic!("compile: {error}"));
    validate(&bytes);
    if !tool("wasmtime") {
        return None;
    }
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("project.wasm");
    std::fs::write(&path, &bytes).expect("write module");

    let output = Command::new("wasmtime")
        .args(["run", "--invoke", function])
        .arg(&path)
        .args(args)
        .output()
        .expect("run wasmtime");
    assert!(
        output.status.success(),
        "wasmtime rejected the module:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A `public static` method is the module's surface: a wasm host has no `main` convention, so
/// every one of them is exported and callable by name.
#[test]
fn a_static_method_is_exported_and_callable() {
    let source = r"
public class Math2 {
    public static int square(int n) {
        return n * n;
    }
}
";
    assert_invoke(&[source], "square", &["7"], "49");
}

/// Control flow: `while` becomes a `block` around a `loop`, `if` becomes wasm's own instruction.
/// The source's nesting is the output's, which is why this backend lowers from the tree.
#[test]
fn loops_and_conditionals_run() {
    let source = r"
public class Sum {
    public static int upTo(int n) {
        int total = 0;
        int i = 0;
        while (i < n) {
            i = i + 1;
            if (i > 2) {
                total = total + i;
            }
        }
        return total;
    }
}
";
    // 3 + 4 + 5 = 12.
    assert_invoke(&[source], "upTo", &["5"], "12");
}

/// The point of targeting the GC proposal: `new` allocates on the *host's* heap, the object's
/// fields are struct fields, and nothing in the emitted module frees anything.
#[test]
fn objects_are_allocated_and_collected_by_the_host() {
    let source = r"
public class Point {
    int x;
    int y;

    Point(int x, int y) {
        this.x = x;
        this.y = y;
    }

    int sum() {
        return x + y;
    }

    public static int make(int a, int b) {
        Point p = new Point(a, b);
        return p.sum();
    }
}
";
    assert_invoke(&[source], "make", &["20", "22"], "42");
}

/// `new` has to leave exactly one value. Inside a `block` that is the only thing keeping the module
/// well-formed: a function body's trailing `return` discards a surplus, so an extra copy of the
/// object survived every test until one sat inside an `if`.
#[test]
fn a_new_inside_a_block_leaves_the_stack_balanced() {
    let source = r"
public class Guarded {
    int x;

    Guarded(int v) {
        this.x = v;
    }

    public static int run(int n) {
        int r = 0;
        if (n > 0) {
            Guarded g = new Guarded(n);
            r = g.x;
        }
        while (n > 100) {
            Guarded g = new Guarded(n);
            r = g.x;
            n = 0;
        }
        return r;
    }
}
";
    assert_invoke(&[source], "run", &["7"], "7");
}

/// Which constructor a `new` runs is the analysis's answer, not a re-derivation. Matching on the
/// argument *count* alone took the first of any same-arity pair, so `new Pair(1.5)` ran the `int`
/// constructor.
#[test]
fn an_overloaded_constructor_is_selected_by_type() {
    let source = r"
public class Pair {
    int tag;

    Pair(int value) {
        this.tag = 1;
    }

    Pair(double value) {
        this.tag = 2;
    }

    public static int fromDouble() {
        Pair p = new Pair(1.5);
        return p.tag;
    }

    public static int fromInt() {
        Pair p = new Pair(3);
        return p.tag;
    }
}
";
    assert_invoke(&[source], "fromDouble", &[], "2");
    assert_invoke(&[source], "fromInt", &[], "1");
}

/// Inheritance becomes *declared* subtyping, so a subclass instance flows where the superclass is
/// expected with no conversion — the host checks it, not the generator.
#[test]
fn inheritance_becomes_declared_subtyping() {
    let source = r"
public class Shape {
    int width;

    int area() {
        return width;
    }
}
";
    let subclass = r"
public class Square extends Shape {
    int height;

    public static int area(int side) {
        Square s = new Square();
        s.width = side;
        s.height = side;
        return widen(s);
    }

    static int widen(Shape shape) {
        return shape.area();
    }
}
";
    assert_invoke(&[source, subclass], "area", &["6"], "6");
}

/// Every source compiles into *one* module: a call from one file to another is a plain `call`,
/// which only resolves because both were compiled together.
#[test]
fn the_whole_project_is_one_module() {
    let helper = r"
public class Helper {
    static int twice(int n) {
        return n + n;
    }
}
";
    let main = r"
public class App {
    public static int run(int n) {
        return Helper.twice(n) + 1;
    }
}
";
    assert_invoke(&[helper, main], "run", &["20"], "41");
}

/// A library type has no wasm representation, and saying so is the honest answer — there is no
/// `java.base` on a wasm host, and inventing one is a separate decision from compiling.
#[test]
fn a_library_type_is_reported_rather_than_guessed() {
    let source = r#"
public class Greeter {
    public static void greet() {
        System.out.println("hi");
    }
}
"#;
    let error = compile(&[source]).expect_err("library types are out of scope");
    assert!(
        matches!(
            error,
            WasmError::NoRepresentation(_) | WasmError::Unsupported(_)
        ),
        "expected a scope error, got {error}"
    );
}

/// `i += 1` shares its node kind with `i = 1`. Lowering it as a plain `local.set` produces a module
/// that validates and runs, and computes the wrong number — which is why the operator is read.
/// Read-modify-write, which has no `dup` to lean on.
///
/// The JVM backend duplicates an address under a value with `dup_x1`; wasm has no such instruction, so
/// a field's receiver and an array's index are spilled into scratch locals and pushed again for the
/// store. Both of §15.26.2's conversions still apply — the operator runs at the promoted type and the
/// result is narrowed back — which is why `byte b; b += 200` wraps rather than storing 200.
#[test]
fn compound_assignment_and_the_increments_run() {
    let source = r"
public class Compound {
    int field;

    public static int local(int n) {
        int i = n;
        i += 5;
        i *= 2;
        i -= 1;
        i /= 3;
        i %= 7;
        i <<= 2;
        i >>= 1;
        i |= 8;
        i &= 30;
        i ^= 3;
        return i;
    }

    // `int i; i += 1L` widens to `i64`, adds, and wraps back; `byte b; b += 200` adds as `i32` and
    // keeps the low byte, sign-extended. Dropping either conversion stores an out-of-range value.
    public static int widened(int n) {
        int i = n;
        i += 4294967296L;
        i += 3L;
        return i;
    }

    public static int narrowed(int n) {
        byte b = (byte) n;
        b += 200;
        return b;
    }

    public static int stepped(int n) {
        int i = n;
        int a = i++;
        int b = i--;
        int c = ++i;
        int d = --i;
        return a * 1000 + b * 100 + c * 10 + d;
    }

    // A field and an array element go through the same protocol: the receiver is spilled to a local
    // once, and both the read and the write take it from there.
    public static int through_a_field(int n) {
        Compound it = new Compound();
        it.field = n;
        it.field += 10;
        it.field++;
        return it.field;
    }

    public static int through_an_element(int n) {
        int[] cells = new int[3];
        cells[1] = n;
        cells[1] += 10;
        cells[1]++;
        int taken = cells[1]--;
        return taken * 100 + cells[1];
    }

    // An assignment is an expression: its value is what was stored, after conversion.
    public static long chained(int n) {
        long a;
        int b;
        a = b = n;
        return a;
    }
}
";
    assert_invoke(&[source], "local", &["9"], "15");
    assert_invoke(&[source], "widened", &["1"], "4");
    assert_invoke(&[source], "narrowed", &["0"], "-56");
    assert_invoke(&[source], "stepped", &["5"], "5665");
    assert_invoke(&[source], "through_a_field", &["1"], "12");
    assert_invoke(&[source], "through_an_element", &["5"], "1615");
    assert_invoke(&[source], "chained", &["7"], "7");
}

/// `?:`, `&&`, and `||` all evaluate one side conditionally, so all three are a typed `if`.
///
/// Not `select`: it pops *both* value operands, so both arms would already have run. The test is a
/// guarded division — under `select` it runs whatever the guard said and traps on a zero divisor.
#[test]
fn the_conditional_operators_evaluate_one_side() {
    let source = r"
public class Choose {
    public static int pick(int n) {
        return n > 0 ? n : -n;
    }

    // The arms have different types, so the whole conditional is one `i64` block.
    public static long widened(int n) {
        return n > 0 ? 1 : 2L;
    }

    public static int guarded(int n) {
        return (n != 0 && 10 / n > 1) ? 1 : 0;
    }

    public static int shorted(int n) {
        return (n == 0 || 10 / n > 1) ? 1 : 0;
    }

    public static int nested(int n) {
        return n < 0 ? -1 : n == 0 ? 0 : 1;
    }
}
";
    assert_invoke(&[source], "pick", &["-4"], "4");
    assert_invoke(&[source], "widened", &["0"], "2");
    // Zero would trap the division if the right operand ran unconditionally.
    assert_invoke(&[source], "guarded", &["0"], "0");
    assert_invoke(&[source], "guarded", &["3"], "1");
    assert_invoke(&[source], "shorted", &["0"], "1");
    assert_invoke(&[source], "nested", &["-9"], "-1");
    assert_invoke(&[source], "nested", &["9"], "1");
}

/// Every loop form, and both jumps out of them.
///
/// A branch names a *relative* depth, and an `if` between a loop's header and a `continue` shifts that
/// depth — so the target comes from the emitter's count of open structures, never from the source. The
/// three-structure shape is the other half: a `continue` in a `for` runs the update first (§14.14.1.3)
/// and one in a `do` reaches the bottom test, and neither point is the top of the loop.
#[test]
fn every_loop_form_and_both_jumps_run() {
    let source = r"
public class Loops {
    public static int summed(int n) {
        int total = 0;
        for (int i = 0; i < n; i++) {
            total += i;
        }
        return total;
    }

    // `continue` has to reach the update, or this never terminates.
    public static int odds(int n) {
        int total = 0;
        for (int i = 0; i < n; i++) {
            if (i % 2 == 0) {
                continue;
            }
            total += i;
        }
        return total;
    }

    public static int counted(int n) {
        int i = 0;
        int total = 0;
        do {
            if (i == 2) {
                i++;
                continue;
            }
            total += i;
            i++;
        } while (i < n);
        return total;
    }

    public static int over_an_array(int n) {
        int[] cells = new int[4];
        for (int i = 0; i < 4; i++) {
            cells[i] = i * n;
        }
        int total = 0;
        for (int cell : cells) {
            if (cell == 2 * n) {
                continue;
            }
            total += cell;
        }
        return total;
    }

    // A labelled `break` leaves the *outer* loop from inside the inner one.
    public static int labelled(int n) {
        int found = -1;
        outer:
        for (int i = 0; i < n; i++) {
            for (int j = 0; j < n; j++) {
                if (i * j > 6) {
                    found = i * 100 + j;
                    break outer;
                }
            }
        }
        return found;
    }

    // A labelled `continue` restarts the outer loop, update included.
    public static int labelled_continue(int n) {
        int total = 0;
        outer:
        for (int i = 0; i < n; i++) {
            for (int j = 0; j < n; j++) {
                if (j == 1) {
                    continue outer;
                }
                total += 1;
            }
            total += 100;
        }
        return total;
    }

    // A label on a plain block is a forward jump and nothing else (§14.7).
    public static int out_of_a_block(int n) {
        int result = 0;
        done: {
            result = n;
            if (n > 0) {
                break done;
            }
            result = -1;
        }
        return result;
    }

    public static int whiled(int n) {
        int i = 0;
        while (true) {
            i++;
            if (i >= n) {
                break;
            }
        }
        return i;
    }
}
";
    assert_invoke(&[source], "summed", &["5"], "10");
    assert_invoke(&[source], "odds", &["6"], "9");
    assert_invoke(&[source], "counted", &["5"], "8");
    assert_invoke(&[source], "over_an_array", &["3"], "12");
    // i = 2, j = 4 is the first product over six.
    assert_invoke(&[source], "labelled", &["5"], "204");
    assert_invoke(&[source], "labelled_continue", &["3"], "3");
    assert_invoke(&[source], "out_of_a_block", &["7"], "7");
    assert_invoke(&[source], "out_of_a_block", &["-7"], "-1");
    assert_invoke(&[source], "whiled", &["4"], "4");
}

/// `switch`, both syntaxes, statement and expression.
///
/// One `block` per arm nested inside a `block` for the whole `switch`, so `br i` lands where arm `i`'s
/// body starts — and falling out of arm `i`'s block end runs arm `i+1`, which *is* the colon form's
/// fallthrough with no branch of its own. `br_table` reads its index unsigned, so subtracting the
/// lowest key is the whole bounds check: a key below it wraps past 2³¹ onto the default.
#[test]
fn a_switch_dispatches_both_densely_and_sparsely() {
    let source = r"
public class Pick {
    // Dense and zero-based: a straight `br_table`.
    public static int arrowed(int n) {
        return switch (n) {
            case 0 -> 10;
            case 1 -> 11;
            case 2 -> 12;
            default -> -1;
        };
    }

    // Negative and offset keys, still dense: the subtraction is what makes them indexable, and a key
    // below the lowest one has to reach the default rather than an arm.
    public static int offset(int n) {
        return switch (n) {
            case -2 -> 1;
            case -1 -> 2;
            case 0 -> 3;
            default -> 9;
        };
    }

    // Far apart: a comparison chain, wasm having no `lookupswitch`.
    public static int sparse(int n) {
        return switch (n) {
            case 1 -> 1;
            case 1000 -> 2;
            case 1000000 -> 3;
            default -> 0;
        };
    }

    // Several keys on one arm.
    public static int grouped(int n) {
        return switch (n) {
            case 1, 2, 3 -> 100;
            case 4 -> 200;
            default -> 0;
        };
    }

    // The colon form, whose arms fall through until one says `break`.
    public static int fallen(int n) {
        int total = 0;
        switch (n) {
            case 0:
                total += 1;
            case 1:
                total += 10;
                break;
            case 2:
                total += 100;
                break;
            default:
                total += 1000;
        }
        return total;
    }

    // A `switch` inside a loop: `break` leaves the `switch`, and `continue` looks straight past it.
    public static int inside_a_loop(int n) {
        int total = 0;
        for (int i = 0; i < n; i++) {
            switch (i) {
                case 0:
                    break;
                case 1:
                    total += 10;
                    break;
                default:
                    continue;
            }
            total += 1;
        }
        return total;
    }

    // A `char` selector is an `i32`, like every integral type narrower than `long`.
    public static int chars(int n) {
        char c = (char) n;
        return switch (c) {
            case 'a' -> 1;
            case 'b' -> 2;
            default -> 0;
        };
    }
}
";
    assert_invoke(&[source], "arrowed", &["1"], "11");
    assert_invoke(&[source], "arrowed", &["7"], "-1");
    assert_invoke(&[source], "offset", &["-1"], "2");
    assert_invoke(&[source], "offset", &["-3"], "9");
    assert_invoke(&[source], "offset", &["4"], "9");
    assert_invoke(&[source], "sparse", &["1000"], "2");
    assert_invoke(&[source], "sparse", &["999"], "0");
    assert_invoke(&[source], "grouped", &["3"], "100");
    assert_invoke(&[source], "grouped", &["4"], "200");
    assert_invoke(&[source], "fallen", &["0"], "11");
    assert_invoke(&[source], "fallen", &["1"], "10");
    assert_invoke(&[source], "fallen", &["9"], "1000");
    assert_invoke(&[source], "inside_a_loop", &["4"], "12");
    assert_invoke(&[source], "chars", &["98"], "2");
}

/// One opcode names one operand type, and wasm converts nothing implicitly.
///
/// So Java's numeric promotions are instructions here as much as on the JVM, and choosing the family
/// from the left operand alone emitted `i64.add` over an `i32` for `n + 1` on a `long` — a module
/// `wasm-tools` rejects with "expected i64, found i32". The shift is the one place wasm disagrees with
/// the JVM outright: `i64.shl` takes **two** `i64`s where `lshl` takes a `long` and an `int`, so the
/// count is converted to the result's width rather than to `int`.
#[test]
fn numeric_promotion_and_the_unary_operators_run() {
    let source = r"
public class Mixed {
    public static long widened(long n) {
        long x = n + 1;
        return x;
    }

    public static double mixedFloat(int n) {
        return n + 0.5;
    }

    public static long shifted(long n, int by) {
        return n << by;
    }

    public static int masked(int n) {
        return (n & 0xF0) | 1;
    }

    public static int complemented(int n) {
        return ~n;
    }

    public static int negated(int n) {
        return -n;
    }

    public static double negatedFloat(double n) {
        return -n;
    }

    public static int flipped(int b) {
        boolean flag = b != 0;
        if (!flag) { return 1; }
        return 0;
    }

    public static int narrowed(double n) {
        return (int) n;
    }

    public static int truncated(int n) {
        return (byte) n;
    }
}
";
    assert_invoke(&[source], "widened", &["9"], "10");
    assert_invoke(&[source], "mixedFloat", &["3"], "3.5");
    assert_invoke(&[source], "shifted", &["1", "40"], "1099511627776");
    assert_invoke(&[source], "masked", &["255"], "241");
    assert_invoke(&[source], "complemented", &["5"], "-6");
    assert_invoke(&[source], "negated", &["7"], "-7");
    assert_invoke(&[source], "negatedFloat", &["1.5"], "-1.5");
    assert_invoke(&[source], "flipped", &["0"], "1");
    // The *saturating* truncation JLS §5.1.3 requires. wasm's plain `i32.trunc_f64_s` traps on a NaN
    // or an out-of-range value, where Java wants a 0 or the nearest representable one.
    assert_invoke(&[source], "narrowed", &["3.9"], "3");
    assert_invoke(&[source], "truncated", &["200"], "-56");
}

/// Arrays are wasm array types, allocated by the host like every other object: `new int[n]` is one
/// instruction whose elements start at their type's default, which is Java's own rule.
#[test]
fn arrays_are_host_allocated() {
    let source = r"
public class Sieve {
    public static int total(int n) {
        int[] values = new int[n];
        int i = 0;
        while (i < n) {
            values[i] = i * 2;
            i = i + 1;
        }
        int sum = 0;
        int j = 0;
        while (j < values.length) {
            sum = sum + values[j];
            j = j + 1;
        }
        return sum;
    }
}
";
    // 0 + 2 + 4 + 6 + 8 = 20.
    assert_invoke(&[source], "total", &["5"], "20");
}

/// A `static` field is module state, which is what a wasm global is.
///
/// Its initialiser has to live in the global's *constant expression*, where the format allows only a
/// handful of instructions — so a field with no initialiser gets its type's default (§4.12.5) and one
/// that would need computing is reported rather than quietly initialised to zero. A `static` field also
/// has no receiver: `Counter.total` reads the global whether it is written bare or qualified.
#[test]
fn a_static_field_is_a_global() {
    let source = r"
public class Counter {
    static int total;
    static int step = 3;
    static long wide = 7L;
    // No suffix: an `int` literal into a `long` field, which the constant expression *folds* rather
    // than converting — there is no `i64.extend` to be had in one.
    static long narrow = 2;
    static double whole = 4;
    static char sign = 'a';
    static double scaled = 1.5;
    static boolean on = true;

    public static int bumped(int times) {
        total = 0;
        for (int i = 0; i < times; i++) {
            total += step;
        }
        return total;
    }

    // Qualified by the class name rather than written bare: the same global, and the receiver is not
    // a value at all.
    public static int qualified(int n) {
        Counter.total = n;
        Counter.total++;
        return Counter.total;
    }

    public static long widths(int n) {
        return wide + narrow + n;
    }

    public static double folded() {
        return whole;
    }

    public static int signed() {
        return sign;
    }

    public static double scaled_by(int n) {
        return scaled * n;
    }

    public static int flagged() {
        return on ? 1 : 0;
    }

    // An instance method reads a `static` field with no `this` involved.
    public int through_an_instance() {
        return step;
    }
}
";
    assert_invoke(&[source], "bumped", &["4"], "12");
    assert_invoke(&[source], "qualified", &["5"], "6");
    assert_invoke(&[source], "widths", &["1"], "10");
    assert_invoke(&[source], "folded", &[], "4");
    assert_invoke(&[source], "signed", &[], "97");
    assert_invoke(&[source], "scaled_by", &["4"], "6");
    assert_invoke(&[source], "flagged", &[], "1");
}

/// A `static` initialiser that a constant expression cannot hold runs in the module's start function.
///
/// A global's own initialiser is a constant expression — no calls, no arithmetic, no conversions — so a
/// computed one has nowhere to live there. The start function is where it goes: an engine calls it
/// before anything else, exactly as a JVM runs `<clinit>` before the first use of the class. The field
/// holds its default until then, which is the same order.
#[test]
fn a_computed_static_initialiser_runs_in_the_start_function() {
    let source = r"
public class Setup {
    static int computed = 1 + 2;
    static int fromMethod = seed();
    static long widened = 5;
    static int stamped = 1;

    // Runs after the field initialisers above it and before the one below.
    static { stamped = stamped * 10; }

    static int last;

    static { last = computed + stamped; }

    static int seed() { return 40; }

    public static int all() {
        return computed + fromMethod + stamped + last;
    }

    public static long wide() { return widened; }
}
";
    // 3 + 40 + 10 + 13
    assert_invoke(&[source], "all", &[], "66");
    assert_invoke(&[source], "wide", &[], "5");
}

/// Every statement form is either compiled or reports *itself*.
///
/// `assert` compiles, and to nothing.
///
/// Java evaluates one only when assertions are *enabled*, they are disabled by default, and a wasm host
/// has no `-ea` to turn them on. So nothing is exactly what a JVM does with one by default — the
/// condition is still parsed, resolved, and linted, it simply has no run-time effect. A trap would be
/// stricter than Java rather than more faithful to it.
#[test]
fn an_assert_compiles_to_nothing() {
    let source = "public class S { public static int run(int n) { assert n > 0; return n; } }";
    assert_invoke(&[source], "run", &["-5"], "-5");
}

/// `throw` and `try`/`catch`, on the exception-handling proposal's `tag` and `try_table`.
///
/// One tag carries every Java exception, because every one of them is a reference: what a `catch` tests
/// is the *class* of the payload, not which tag raised it. `try_table` delivers that payload to one
/// label, so the class tests happen after it — the caught reference is spilled into a local and each
/// handler is a `ref.test` against its declared type, in source order, because §14.20 gives the first
/// matching clause. A payload no clause accepts is re-thrown, which is what makes an unhandled exception
/// leave the frame instead of vanishing.
#[test]
fn throw_and_catch_run() {
    let source = r"
public class Boom extends RuntimeException {
    int code;
    Boom(int code) { this.code = code; }
}

public class Other extends RuntimeException {
    Other() {}
}

public class Risky {
    static int raise(int n) {
        if (n > 0) { throw new Boom(n); }
        return 1;
    }

    // Thrown across a call boundary and caught here.
    public static int caught(int n) {
        try {
            return raise(n);
        } catch (Boom b) {
            return b.code * 10;
        }
    }

    // The first *matching* clause wins, not the first clause.
    public static int firstMatch() {
        try {
            throw new Other();
        } catch (Boom b) {
            return 1;
        } catch (Other o) {
            return 2;
        }
    }

    // A `try` whose body completes normally must skip every handler.
    public static int fellThrough(int n) {
        int total = 0;
        try {
            total = n;
        } catch (Boom b) {
            total = 99;
        }
        return total + 1;
    }

    // The caught variable has the type the source wrote, not the top of the hierarchy: reading
    // `b.code` needs the narrowing the handler applies.
    public static int narrowed(int n) {
        try {
            throw new Boom(n);
        } catch (Boom b) {
            return b.code + 1;
        }
    }
}
";
    assert_invoke(&[source], "caught", &["3"], "30");
    assert_invoke(&[source], "caught", &["0"], "1");
    assert_invoke(&[source], "firstMatch", &[], "2");
    assert_invoke(&[source], "fellThrough", &["7"], "8");
    assert_invoke(&[source], "narrowed", &["4"], "5");
}

/// `finally`, and `synchronized`.
///
/// A structured cleanup costs a *duplicate*: the exceptional path runs it before re-throwing and the
/// normal path runs it after the block, so the source's one `finally` becomes two copies. What it cannot
/// intercept is a `return` / `break` / `continue` inside the protected code — that branches straight
/// past the block the cleanup sits after — so one is reported rather than emitted with the cleanup
/// skipped, which would be silent.
///
/// `synchronized` has no monitor to take: a module here is single-threaded, so there is nothing to
/// exclude and nothing for a cleanup to release. Its two observable effects remain — the lock is
/// evaluated, and a `null` one fails, by trapping rather than throwing, the same trade a failed
/// `ref.cast` already makes.
#[test]
fn a_finally_and_a_synchronized_run() {
    let source = r"
public class Boom extends RuntimeException {
    Boom() {}
}

public class Cleanly {
    static int trace;

    public static int normally(int n) {
        trace = 0;
        try {
            trace = n;
        } finally {
            trace = trace + 100;
        }
        return trace;
    }

    // The cleanup has to run on the way out of the handler too.
    public static int afterCatching(int n) {
        trace = 0;
        try {
            throw new Boom();
        } catch (Boom b) {
            trace = n;
        } finally {
            trace = trace + 100;
        }
        return trace;
    }

    // Nothing catches it, so the cleanup runs and the exception carries on out of this frame.
    static void uncaught() {
        try {
            throw new Boom();
        } finally {
            trace = 7;
        }
    }

    public static int onTheWayOut() {
        trace = 0;
        try {
            uncaught();
        } catch (Boom b) {
            return trace;
        }
        return -1;
    }

    // §14.20.2: the returned value is computed, *then* the cleanup runs — so a cleanup can observe the
    // value's side effects and cannot change what is returned.
    public static int returnedThrough(int n) {
        trace = 0;
        try {
            return n;
        } finally {
            trace = 42;
        }
    }

    public static int cleanedFirst() {
        returnedThrough(5);
        return trace == 42 ? 1 : 0;
    }

    // A `break` out of protected code leaves the cleanup behind, so the cleanup runs on the way.
    public static int brokeOut(int n) {
        trace = 0;
        int seen = 0;
        while (true) {
            try {
                seen = n;
                break;
            } finally {
                trace = trace + 42;
            }
        }
        // 42 once, not twice: the `break` jumped past the normal-path copy of the cleanup.
        return trace + seen;
    }

    // A `continue` leaves it behind once per iteration, and a cleanup opened *outside* the loop must
    // not run for a jump that stays inside it.
    public static int continued(int n) {
        trace = 0;
        for (int i = 0; i < n; i++) {
            try {
                continue;
            } finally {
                trace = trace + 1;
            }
        }
        return trace;
    }

    static int shared;

    public static int locked(int n) {
        Cleanly it = new Cleanly();
        synchronized (it) {
            shared = n * 2;
        }
        return shared;
    }
}
";
    assert_invoke(&[source], "normally", &["5"], "105");
    assert_invoke(&[source], "afterCatching", &["5"], "105");
    assert_invoke(&[source], "onTheWayOut", &[], "7");
    assert_invoke(&[source], "locked", &["4"], "8");
    assert_invoke(&[source], "returnedThrough", &["5"], "5");
    assert_invoke(&[source], "cleanedFirst", &[], "1");
    assert_invoke(&[source], "brokeOut", &["3"], "45");
    assert_invoke(&[source], "continued", &["3"], "3");
}

/// Constructor delegation, `this(…)` and `super(…)`.
///
/// Both are calls to a constructor, and a constructor has no return type at all — `resolved_member_ty`
/// reports `Unknown` for one, which is not a type this backend could represent even in principle. Asking
/// for its wasm type reported that `?` had no representation, which is true and useless: the fix is that
/// a constructor call produces no value. wasm needs no `invokespecial`/`invokevirtual` distinction for
/// either form, and declared subtyping is what lets a subclass reference reach the superclass
/// constructor's parameter 0.
#[test]
fn constructor_delegation_runs() {
    let source = r"
public class Base {
    int a;
    Base(int a) { this.a = a; }
}

public class Derived extends Base {
    int b;
    // `super(a)` reaches `Base`'s constructor with `this` as its receiver.
    Derived(int a, int b) { super(a); this.b = b; }
    // `this(a, 100)` reaches this class's own two-argument one.
    Derived(int a) { this(a, 100); }

    public static int delegated(int n) {
        Derived d = new Derived(n);
        return d.a + d.b;
    }

    public static int direct(int n) {
        Derived d = new Derived(n, 2);
        return d.a + d.b;
    }
}
";
    assert_invoke(&[source], "delegated", &["5"], "105");
    assert_invoke(&[source], "direct", &["5"], "7");
}

/// A class with *no* declared constructor still runs its field initialisers.
///
/// They run in a constructor, and a class without one had no function to run them in — so `class Box { int value
/// = 9; }` read back as 0, a wrong value in a module that validates. One is *synthesised* for such a class and
/// the `new` calls it, which is what gives an initialiser block a slot 0 to read its own fields through.
#[test]
fn a_class_with_no_constructor_still_runs_its_field_initialisers() {
    let source = r"
public class Seeded {
    int fixed = 7;
    int stamped = 2;

    public static int summed() {
        Seeded it = new Seeded();
        return it.fixed + it.stamped;
    }
}
";
    assert_invoke(&[source], "summed", &[], "9");

    // A `{ … }` block reads its own fields through `this`, which only a *function* has a slot 0 to be — so one
    // is synthesised for a class that declares no constructor, and the `new` calls it.
    let blocked = concat!(
        "public class S { int n = 1; { n = n + 4; } ",
        "public static int run() { return new S().n; } }"
    );
    assert_invoke(&[blocked], "run", &[], "5");
}

/// Both instance initialiser forms, in the order they are written.
///
/// A field's `= …` and a bare `{ … }` block interleave into one sequence that runs before the
/// constructor's body (§12.5). Neither is a statement anywhere in the source, so a constructor that
/// emitted only its own body left every one of them unrun — a field reading back as 0 in a module that
/// validates. Order is observable: the block below overwrites what the field initialiser above it set
/// and is then overwritten by the one below.
#[test]
fn every_instance_initialiser_runs_in_source_order() {
    let source = r"
public class Seeded {
    int fixed = 7;
    int stamped = 1;
    int given;

    // Runs after `stamped = 1` and before `stamped = 3`.
    { stamped = stamped * 2; }

    int later = 0;

    { stamped = stamped + 3; }

    Seeded(int given) { this.given = given; }

    public static int summed(int n) {
        Seeded it = new Seeded(n);
        return it.fixed + it.given;
    }

    // 1, doubled to 2, then + 3.
    public static int ordered() {
        return new Seeded(0).stamped;
    }
}
";
    assert_invoke(&[source], "summed", &["3"], "10");
    assert_invoke(&[source], "ordered", &[], "5");
}

/// A `static` nested class is simply another struct type.
///
/// wasm's type space is flat and has no naming convention to satisfy, so there is nothing for a nested
/// class to be nested *in*. Walking only the root's children dropped every one of them silently — the
/// type never existed, and a call to one of its methods reported an unresolved name pointing nowhere
/// useful. (A non-`static` one carries an enclosing instance; that is
/// `an_inner_class_holds_its_enclosing_instance`.)
#[test]
fn a_static_nested_class_is_its_own_struct_type() {
    let source = r"
public class Outer {
    static class Inner {
        int value;
        Inner(int value) { this.value = value; }
        int doubled() { return value * 2; }
    }

    static class Deeper extends Inner {
        Deeper(int value) { super(value + 1); }
    }

    public static int through_a_nested_class(int n) {
        Inner inner = new Inner(n);
        return inner.doubled();
    }

    // A nested subclass of a nested class: the supertype-first ordering has to reach both.
    public static int through_a_nested_subclass(int n) {
        Deeper deeper = new Deeper(n);
        return deeper.doubled();
    }
}
";
    assert_invoke(&[source], "through_a_nested_class", &["5"], "10");
    assert_invoke(&[source], "through_a_nested_subclass", &["5"], "12");
}

/// A type declaration this backend lays out nothing for reports *itself*.
///
/// Dropping one is what the class walk used to do to every nested declaration: the type never exists,
/// and the first use of it reports an unresolved name pointing at nothing a reader can act on. An
/// interface needs a dispatch mechanism (a function table or a per-type vtable struct); an `enum` and a
/// `record` need the synthesised members the JVM backend builds. Saying which is missing is the
/// difference between a compiler with a gap and one that looks broken.
#[test]
fn each_unrepresentable_type_declaration_names_itself() {
    for (source, expected) in [
        ("@interface M {}", "an `@interface` declaration"),
        // Nested, which is where the silent drop used to happen.
        (
            "public class O { @interface M {} }",
            "an `@interface` declaration",
        ),
    ] {
        let error = compile(&[source]).expect_err("this declaration is not laid out yet");
        assert!(
            matches!(error, WasmError::Unsupported(what) if what == expected),
            "`{source}` should report {expected:?}, got {error}"
        );
    }
}

/// `{1, 2, 3}`, whose elements are written rather than defaulted.
///
/// An array initialiser has no type of its own — it is an array of whatever it is assigned to — so the
/// element type comes from what inference recorded for the declaration. One instruction takes the
/// values straight off the stack, so there is no allocate-then-fill sequence and no index to keep.
#[test]
fn an_array_initialiser_builds_its_array() {
    let source = r"
public class Filled {
    static int[] shared = {7, 8};

    public static int summed(int n) {
        int[] cells = {n, n * 2, n * 3};
        int total = 0;
        for (int cell : cells) {
            total += cell;
        }
        return total + cells.length;
    }

    // A widening conversion per element: an `int` literal into a `long[]`.
    public static long widened() {
        long[] cells = {1, 2, 3};
        return cells[0] + cells[1] + cells[2];
    }

    // Nested: the inner initialiser's own recorded type is the inner array's, so nothing has to know
    // how deep it is.
    public static int nested(int n) {
        int[][] grid = {{n, 1}, {2}};
        return grid[0][0] + grid[0][1] + grid[1][0] + grid.length;
    }

    // A `static` field's initialiser is computed, so it runs in the start function.
    public static int fromAStatic() {
        return shared[0] + shared[1];
    }
}
";
    assert_invoke(&[source], "summed", &["2"], "15");
    assert_invoke(&[source], "widened", &[], "6");
    assert_invoke(&[source], "nested", &["5"], "10");
    assert_invoke(&[source], "fromAStatic", &[], "15");
}

/// An overridden method dispatches on the receiver's *actual* type.
///
/// Every call used to be a direct one to the statically-selected member, so `a.legs()` on a `Bird` held
/// in an `Animal` called `Animal.legs()` — 4 where Java says 2, in a module that validates. The
/// self-call inside `describe()` was wrong the same way, which is the half that a test of the call site
/// alone would miss.
///
/// There is no vtable and no `call_ref`. wasm has no dynamic loading and no classpath, and this backend
/// compiles the whole project as one module — so the set of classes that can override a method is
/// closed and known, and a chain of `ref.test` most-derived-first answers exactly what a vtable would.
/// The receiver and the arguments are spilled into locals because each arm re-pushes them and Java
/// evaluates them once.
#[test]
fn an_overridden_method_dispatches_on_the_runtime_type() {
    let source = r"
public class Animal {
    int legs() { return 4; }
    // A self-call has to dispatch too: `this` is a `Bird` here even though the method is `Animal`'s.
    int described() { return legs() * 10; }
    int scaled(int n) { return legs() * n; }
}

public class Bird extends Animal {
    int legs() { return 2; }
}

public class Penguin extends Bird {
    int legs() { return 1; }
}

public class Snake extends Animal {
    int legs() { return 0; }
}

public class Zoo {
    public static int through_a_supertype() {
        Animal a = new Bird();
        return a.legs();
    }

    // The most-derived override has to be tested first: testing `Bird` before `Penguin` would answer 2.
    public static int through_two_levels() {
        Animal a = new Penguin();
        return a.legs();
    }

    public static int through_a_self_call() {
        Animal a = new Bird();
        return a.described();
    }

    public static int with_an_argument(int n) {
        Animal a = new Snake();
        return a.scaled(n) + 1;
    }

    // A class that overrides nothing keeps its own method, and one held as itself needs no test.
    public static int without_overriding() {
        Snake s = new Snake();
        return s.described();
    }
}
";
    assert_invoke(&[source], "through_a_supertype", &[], "2");
    assert_invoke(&[source], "through_two_levels", &[], "1");
    assert_invoke(&[source], "through_a_self_call", &[], "20");
    assert_invoke(&[source], "with_an_argument", &["6"], "1");
    assert_invoke(&[source], "without_overriding", &[], "0");
}

/// An interface, and a call dispatched through one.
///
/// An interface gets no struct type: wasm's declared subtyping is single-inheritance, so it could not be
/// a supertype of two unrelated classes. A value of interface type is held at the *top* of the reference
/// hierarchy (`anyref`) and narrowed with `ref.cast` at each use — and the dispatch is the same
/// `ref.test` chain a class override uses, for the same reason it is sound: the whole project is one
/// module, so the set of implementing classes is closed and known at the call site.
///
/// Its methods declare no function at all. An abstract method has no body, so putting a signature with
/// a result type over an empty one is a module no engine accepts; every class that could satisfy the
/// call is in the chain instead, and falling off the end traps.
#[test]
fn a_call_through_an_interface_reaches_the_implementation() {
    let source = r"
public interface Shape {
    int area();
}

public class Square implements Shape {
    int side;
    Square(int side) { this.side = side; }
    public int area() { return side * side; }
}

public class Rect implements Shape {
    int w;
    int h;
    Rect(int w, int h) { this.w = w; this.h = h; }
    public int area() { return w * h; }
}

public class Areas {
    public static int through_an_interface(int n) {
        Shape s = new Square(n);
        return s.area();
    }

    // The other implementation, reached through the same static type.
    public static int the_other_one(int n) {
        Shape s = new Rect(n, 3);
        return s.area();
    }

    // An interface as a parameter type, which is where the `anyref` representation has to hold up.
    static int sum(Shape a, Shape b) { return a.area() + b.area(); }

    public static int as_a_parameter(int n) {
        return sum(new Square(n), new Rect(n, 2));
    }

    // An interface-typed field, so the struct layout has to name the representation too.
    static Shape held;

    public static int through_a_field(int n) {
        held = new Rect(n, 5);
        return held.area();
    }
}
";
    assert_invoke(&[source], "through_an_interface", &["4"], "16");
    assert_invoke(&[source], "the_other_one", &["4"], "12");
    assert_invoke(&[source], "as_a_parameter", &["3"], "15");
    assert_invoke(&[source], "through_a_field", &["2"], "10");
}

/// An `enum`: laid out like a class, with its constants as globals the start function builds.
///
/// A constant is a `static final` field whose value the source never writes — it is an *allocation*,
/// which no constant expression can hold — so the global starts as `null` and the start function fills
/// it, before any user initialiser, because §8.9.3 builds the constants first. Its declared fields and
/// methods are a class's, and `==` on two constants is `ref.eq`, which is exactly what enum identity is.
///
/// What an enum cannot have here is anything from `java.lang.Enum`: `name()`, `toString()`, and
/// `valueOf(String)` all involve a `String`, which has no wasm representation by this backend's existing
/// design. A call to one reports rather than being guessed at.
#[test]
fn an_enum_gets_its_constants_as_globals() {
    let source = r"
public enum Colour {
    RED, GREEN, BLUE;

    int brightness;

    int bright() { return brightness + 1; }
}

public class Palette {
    // A declared field and a declared method, which are a class's.
    public static int through_a_constant() {
        Colour c = Colour.GREEN;
        c.brightness = 4;
        return c.bright();
    }

    // Enum identity is reference identity, and each constant is built exactly once.
    public static int identity() {
        return Colour.BLUE == Colour.BLUE ? 1 : 0;
    }

    public static int distinct() {
        return Colour.RED == Colour.GREEN ? 1 : 0;
    }
}
";
    assert_invoke(&[source], "through_a_constant", &[], "5");
    assert_invoke(&[source], "identity", &[], "1");
    assert_invoke(&[source], "distinct", &[], "0");
}

/// An `enum` constant carries its arguments to the constructor the source declared.
///
/// The two synthetic parameters a JVM `enum` constructor takes (`name`, `ordinal`) have nothing to carry
/// here: both of the methods that read them come from `java.lang.Enum` and involve a `String`, which this
/// backend has no representation for. So a constant's arguments go to the declared constructor with
/// nothing ahead of them — and the constructor *runs*, which the plain allocation the start function used
/// to emit did not: every field read back as its default, in a module that validates.
#[test]
fn an_enum_constant_carries_its_arguments() {
    let source = r"
public enum Coin {
    PENNY(1), NICKEL(5), QUARTER(25);

    final int cents;
    // A field initialiser runs from the constructor, so a constant gets it too.
    final int tag = 7;

    Coin(int cents) {
        this.cents = cents;
    }

    int doubled() {
        return cents * 2;
    }
}

// Constants with no arguments at all still need the synthesised constructor to run the initialiser.
public enum Flag {
    ON, OFF;
    int mark = 3;
}

public class Money {
    public static int total() {
        return Coin.PENNY.cents + Coin.NICKEL.cents + Coin.QUARTER.cents;
    }

    public static int doubled() {
        return Coin.QUARTER.doubled();
    }

    public static int tag() {
        return Coin.NICKEL.tag;
    }

    public static int mark() {
        return Flag.ON.mark + Flag.OFF.mark;
    }
}
";
    assert_invoke(&[source], "total", &[], "31");
    assert_invoke(&[source], "doubled", &[], "50");
    assert_invoke(&[source], "tag", &[], "7");
    assert_invoke(&[source], "mark", &[], "6");
}

/// A constant whose arguments match no constructor is reported: there is no descriptor to pick.
#[test]
fn the_enum_shapes_that_need_more_are_reported() {
    let source = "public enum E { A(1, 2); E(int a) {} }";
    let error = compile(&[source]).expect_err("this enum has no constructor to build with");
    assert!(
        matches!(
            error,
            WasmError::Unsupported("an `enum` constant with no matching constructor")
        ),
        "got {error}"
    );
}

/// A type inside an `enum` constant's body reports that its owner has no name.
///
/// The JVM backend's twin (`a_type_in_an_enum_constant_body_has_no_enclosing_name`) covers the same
/// shape through `Compile::enclosing_name`; this one reaches `Layout::owner_of` through `is_inner`,
/// which takes `parent().parent()` of a nested class and so lands on the `ENUM_CONSTANT`. That is
/// not one of the seven forms `ast::Decl` casts, so it has no name to key the layout on.
///
/// What is pinned is the narrowing. Before `Decl::name_token_of`, the scan here took the constant's
/// own `IDENT` and looked an **item** up at a **member**'s offset; adding a variant to `ast::Decl`
/// would put that back with nothing else in the suite disagreeing.
#[test]
fn a_type_in_an_enum_constant_body_has_no_owning_type() {
    let source = "public enum E { A { class Inner {} }; }";
    let error = compile(&[source]).expect_err("an `enum` constant is not an owning type");
    assert!(
        matches!(
            error,
            WasmError::Unsupported("a type declaration with no name")
        ),
        "got {error}"
    );
}

/// A `record`: fields from the header, plus a canonical constructor and accessors written out.
///
/// A component is declared once, in the header, and stands for three things — a field, an accessor, and
/// a constructor parameter — none of which the body writes. The index already synthesises all three
/// (that is what makes `p.x()` resolve), so what was missing was only the code, and it is short enough
/// to write directly: the constructor stores each parameter into its slot and an accessor reads one back.
///
/// `equals`, `hashCode`, and `toString` are *not* synthesised here. All three come from
/// `java.lang.Record`, and two of them involve a `String`, which has no wasm representation by this
/// backend's design — a call to one reports rather than being guessed at.
#[test]
fn a_record_gets_a_constructor_and_accessors() {
    let source = r"
public record Point(int x, long span) {}

public record Wrapped(Point inner) {}

public class Places {
    public static int through_a_record(int n) {
        Point p = new Point(n, 4L);
        return p.x() + (int) p.span();
    }

    // A `long` component, whose accessor's result type has to be the component's rather than an `i32`.
    public static long widths(int n) {
        Point p = new Point(n, 40L);
        return p.span() + p.x();
    }

    // A record component of record type: the reference representation has to hold up through both.
    public static int nested(int n) {
        Wrapped w = new Wrapped(new Point(n, 1L));
        return w.inner().x() * 2;
    }
}
";
    assert_invoke(&[source], "through_a_record", &["3"], "7");
    assert_invoke(&[source], "widths", &["2"], "42");
    assert_invoke(&[source], "nested", &["5"], "10");
}

/// `yield`, which is how a colon-form `switch` *expression* produces its value.
///
/// The whole `switch` is already a typed block, so a `yield` is a `br` out of it carrying the value —
/// there is nothing else to it once the block has a result type. A colon-form arm leaves that way, so the
/// last group's end carries no value: Java's own rule is that every arm yields or throws, and the
/// trailing `unreachable` is there so the validator does not have to infer that rule.
#[test]
fn a_yield_leaves_a_switch_expression() {
    let source = r"
public class Pick {
    public static int colonForm(int n) {
        return switch (n) {
            case 0:
                yield 10;
            case 1:
                yield 11;
            default:
                yield -1;
        };
    }

    // A block-bodied arrow arm yields too, and a `yield` from inside an `if` has to reach the same
    // place — the branch depth comes from the emitter, not from how deeply the source nested it.
    public static int nested(int n) {
        return switch (n) {
            case 0: {
                if (n == 0) {
                    yield 100;
                }
                yield 200;
            }
            default:
                yield 300;
        };
    }

    // A `switch` expression inside another one: the inner `yield` must reach the *inner* block.
    public static int layered(int n) {
        return switch (n) {
            case 0:
                yield switch (n + 1) {
                    case 1:
                        yield 7;
                    default:
                        yield 8;
                };
            default:
                yield 9;
        };
    }
}
";
    assert_invoke(&[source], "colonForm", &["1"], "11");
    assert_invoke(&[source], "colonForm", &["5"], "-1");
    assert_invoke(&[source], "nested", &["0"], "100");
    assert_invoke(&[source], "layered", &["0"], "7");
    assert_invoke(&[source], "layered", &["3"], "9");
}

/// An arrow arm whose body is a block, in a `switch` *expression*.
///
/// The arm leaves by `yield`, which has already branched to the `switch`'s block carrying the value —
/// so the arm's own end must not branch again, having nothing to branch with. A `throw` arm is the other
/// form that never reaches its end. Both stand for Java's rule that every arm yields or throws.
#[test]
fn an_arrow_arm_with_a_block_body_yields() {
    let source = r"
public class Unknown extends RuntimeException {
    Unknown() {}
}

public class Grade {
    public static int of(int n) {
        return switch (n) {
            case 0 -> {
                int doubled = n + 40;
                yield doubled + 2;
            }
            case 1 -> {
                // A `yield` from inside an `if` reaches the same place: the depth comes from the
                // emitter, not from how deeply the source nested it.
                if (n > 0) {
                    yield 7;
                }
                yield 8;
            }
            // An expression arm still branches with its own value, alongside the block ones.
            case 2 -> 99;
            default -> throw new Unknown();
        };
    }
}
";
    assert_invoke(&[source], "of", &["0"], "42");
    assert_invoke(&[source], "of", &["1"], "7");
    assert_invoke(&[source], "of", &["2"], "99");
}

/// A multi-catch, lowered as one arm *per declared type*.
///
/// The variable's type is the least upper bound of the declared types, which this backend does not
/// compute — and there is no struct type for a bound it cannot name. So each declared type gets its own
/// copy of the handler with the variable narrowed to that type. It is sound because any member the source
/// can legally reach through the variable is declared on the bound, and a struct's fields start with its
/// supertype's, so the slot is the same in every one of them.
#[test]
fn a_multi_catch_binds_each_declared_type() {
    let source = r"
public class Base extends RuntimeException {
    int code;
    Base(int code) { this.code = code; }
}

public class Left extends Base {
    Left(int code) { super(code); }
}

public class Right extends Base {
    Right(int code) { super(code); }
}

public class Risky {
    static void raise(int n) {
        if (n > 0) { throw new Left(n); }
        throw new Right(-n);
    }

    // `e.code` is declared on `Base`, so it is at the same slot in `Left` and in `Right`.
    public static int caught(int n) {
        try {
            raise(n);
            return 0;
        } catch (Left | Right e) {
            return e.code + 100;
        }
    }
}
";
    assert_invoke(&[source], "caught", &["3"], "103");
    assert_invoke(&[source], "caught", &["-4"], "104");
}

/// A non-`static` inner class, which holds its enclosing instance in a synthetic field.
///
/// The field goes *after* the class's own, so every real field keeps the slot the layout computes for it
/// — and that is why a subclass of an inner class is reported instead: its own fields would start where
/// the synthetic one sits. Each constructor takes the enclosing instance right after `this` and writes it
/// before anything else runs, so an initialiser or the body can already reach it. A class with no declared
/// constructor has no function to write it in, so the `new` writes it.
///
/// `outer.new Inner()` names the enclosing instance explicitly, and it is not the same as `this`: the
/// qualifier is an expression sitting *before* the `new` keyword, which is the only thing that
/// distinguishes the two forms in the tree.
#[test]
fn an_inner_class_holds_its_enclosing_instance() {
    let source = r"
public class Outer {
    int base;

    class Inner {
        int extra;
        Inner(int extra) { this.extra = extra; }
        int total() { return extra; }
    }

    // No declared constructor: the `new` writes the synthetic field itself.
    class Plain {
        int flag;
    }

    Outer(int base) { this.base = base; }

    int build(int n) {
        Inner i = new Inner(n);
        return i.total() + base;
    }

    int defaulted() {
        Plain p = new Plain();
        p.flag = 5;
        return p.flag;
    }

    public static int run(int n) {
        Outer o = new Outer(10);
        return o.build(n);
    }

    public static int implicit() {
        Outer o = new Outer(1);
        return o.defaulted();
    }

    // A *qualified* `new` names a different enclosing instance than `this`.
    int fromAnother(Outer other, int n) {
        Inner i = other.new Inner(n);
        return i.total() + other.base;
    }

    public static int qualified(int n) {
        Outer host = new Outer(1);
        Outer named = new Outer(70);
        return host.fromAnother(named, n);
    }
}
";
    assert_invoke(&[source], "run", &["3"], "13");
    assert_invoke(&[source], "implicit", &[], "5");
    assert_invoke(&[source], "qualified", &["2"], "72");

    // A `new` of an inner class needs an enclosing instance, which a `static` method does not have.
    let outside = concat!(
        "public class O { int f; class I { int g; } ",
        "public static int run(int n) { I i = new I(); return n; } }"
    );
    let error = compile(&[outside]).expect_err("a `static` method has no enclosing instance");
    assert!(
        matches!(
            error,
            WasmError::Unsupported("a `new` of an inner class outside an instance method")
        ),
        "got {error}"
    );

    // A subclass of an inner class would place its first field on top of the synthetic one.
    let extended = "public class O { int f; class I { int g; } class J extends I {} }";
    let error = compile(&[extended]).expect_err("a subclass of an inner class is not laid out");
    assert!(
        matches!(
            error,
            WasmError::Unsupported("a subclass of an inner class")
        ),
        "got {error}"
    );
}

/// A local class — one declared inside a method body.
///
/// wasm's type space is flat and has nothing to say about where a class was written, so a local class is
/// laid out like any other. What it may not do is *capture* a local from the method that encloses it: each
/// capture needs a synthetic constructor parameter the index knows nothing about, so its constructor would
/// come out one parameter short of what a `new` passes.
#[test]
fn a_local_class_is_laid_out_unless_it_captures() {
    let source = r"
public class Host {
    public static int run(int n) {
        class Counter {
            int total;
            Counter(int start) { total = start; }
            int bumped(int by) { return total + by; }
        }
        Counter c = new Counter(n);
        return c.bumped(5);
    }
}
";
    assert_invoke(&[source], "run", &["7"], "12");

    // A capturing class with no declared constructor has no function to fill its capture fields, so the
    // `new` fills them — the same way it fills an inner class's single enclosing instance.
    let capturing = concat!(
        "public class H { public static int run(int n) { ",
        "class C { int read() { return n * 3; } } return new C().read(); } }"
    );
    assert_invoke(&[capturing], "run", &["4"], "12");
}

/// try-with-resources: each resource closed in reverse declaration order, on both paths.
///
/// What this does *not* do is record a suppressed exception. A `close()` that throws while the body is
/// already throwing is swallowed, because `Throwable.addSuppressed` needs a type with no wasm
/// representation. The *primary* exception is still the body's — the one Java propagates and the one a
/// `catch` sees — so the control flow is right and only the suppressed list is missing.
#[test]
fn a_try_with_resources_closes_each_one() {
    let source = r"
public class Handle {
    static int closes;
    int id;
    Handle(int id) { this.id = id; }
    public void close() { closes = closes + id; }
}

public class Boom extends RuntimeException {
    Boom() {}
}

public class Uses {
    public static int normally(int n) {
        try (Handle a = new Handle(n)) {
            Handle.closes = 0;
        }
        return Handle.closes;
    }

    // Two resources: both closed, in reverse declaration order.
    public static int both() {
        try (Handle a = new Handle(1); Handle b = new Handle(10)) {
            Handle.closes = 0;
        }
        return Handle.closes;
    }

    static void raising() {
        try (Handle a = new Handle(4)) {
            Handle.closes = 0;
            throw new Boom();
        }
    }

    // The resource is closed on the way out, and the body's exception carries on past this frame.
    public static int onTheWayOut() {
        try {
            raising();
        } catch (Boom b) {
            return Handle.closes;
        }
        return -1;
    }
}
";
    assert_invoke(&[source], "normally", &["3"], "3");
    assert_invoke(&[source], "both", &[], "11");
    assert_invoke(&[source], "onTheWayOut", &[], "4");
}

/// A local class that *captures* a local, which outlives the frame the local lived in.
///
/// Each capture becomes a struct field and a *trailing* constructor parameter — trailing so a declared one
/// keeps its slot — and every `new` passes the values from wherever they live at that point. Inside the
/// class the name is not a local at all: it reads the field the constructor filled.
#[test]
fn a_local_class_captures_the_locals_it_reads() {
    let source = r"
public class Host {
    public static int run(int n) {
        int seen = n;
        long wide = 40L;
        class Reader {
            int extra;
            Reader(int extra) { this.extra = extra; }
            int read() { return seen + extra; }
            long widened() { return wide + seen; }
        }
        Reader r = new Reader(5);
        return r.read() + (int) r.widened();
    }
}
";
    // 7 + 5, plus 40 + 7.
    assert_invoke(&[source], "run", &["7"], "59");
}

/// An anonymous class — `new I() { … }` — is its own struct type.
///
/// It has no name and no declaration keyword, so it is recognised by shape and its item is found by the
/// `new` keyword's position, which is the only offset the index could key it on. The `new` then builds
/// *that* type rather than the one it named, and the type it named becomes what the dispatch chain tests
/// against.
///
/// One that *captures* is still reported. An anonymous class never declares a constructor, so it lands on
/// the "capturing class with no declared constructor" report — and the `new`-fills-the-fields fix that
/// would lift it runs into a separate `anyref`-versus-struct mismatch, which is not something to leave
/// half-done in the emitter.
#[test]
fn an_anonymous_class_is_its_own_struct_type() {
    let source = r"
public interface Shape { int area(); }

public class Areas {
    static Shape fixed_() {
        return new Shape() {
            public int area() { return 3; }
        };
    }

    // A second one gets its own type, and the dispatch chain has to tell them apart.
    static Shape other() {
        return new Shape() {
            public int area() { return 10; }
        };
    }

    public static int run(int n) { return fixed_().area() + other().area() + n; }
}
";
    assert_invoke(&[source], "run", &["1"], "14");

    // A capturing one: the `new` fills the capture fields itself, since an anonymous class never declares
    // a constructor to fill them in.
    let capturing = concat!(
        "public interface Shape { int area(); } ",
        "public class A { static Shape of(int n) { return new Shape() { public int area() { return n * 2; } }; } ",
        "public static int run(int n) { return of(n).area(); } }"
    );
    assert_invoke(&[capturing], "run", &["6"], "12");
}

/// A lambda, which in a module with no `invokedynamic` is an instance of a one-method class.
///
/// The index gives a lambda its own item, the interface it is converted to as its supertype, and that
/// interface's method as its one member — so the dispatch chain that already finds every implementing class
/// finds this too, and nothing new is needed to *call* it. Building one is then building that object:
/// allocate the struct and write the captures into it, exactly as an anonymous class's `new` does.
///
/// A capture is a *field* here, not a leading parameter as on the JVM: the object outlives the frame either
/// way, and a struct is what this backend has to keep it in.
#[test]
fn a_lambda_is_an_instance_of_a_one_method_class() {
    let source = r"
public interface Doubler { int apply(int n); }

public class Uses {
    public static int plain(int n) {
        Doubler d = x -> x * 2;
        return d.apply(n);
    }

    public static int capturing(int n) {
        int bump = 40;
        Doubler d = x -> x + bump;
        return d.apply(n);
    }

    // A block body returns for itself.
    public static int blocked(int n) {
        Doubler d = x -> { return x * 3; };
        return d.apply(n);
    }

    // Two lambdas on the same interface: the dispatch chain has to tell their types apart.
    public static int both(int n) {
        Doubler a = x -> x + 1;
        Doubler b = x -> x * 10;
        return a.apply(n) + b.apply(n);
    }
}
";
    assert_invoke(&[source], "plain", &["21"], "42");
    assert_invoke(&[source], "capturing", &["2"], "42");
    assert_invoke(&[source], "blocked", &["14"], "42");
    assert_invoke(&[source], "both", &["4"], "45");
}

/// A method reference, which is the same one-method class a lambda is — with a body that delegates.
///
/// The index gives it the same item, supertype, and member a lambda gets, so the dispatch chain finds it the
/// same way. Its body is one call: the interface method's arguments go straight to the method the source named,
/// forwarded by position, so nothing needs binding by name. A delegating reference captures nothing, which is
/// why building the object is a bare allocation.
///
/// A *bound* reference (`x::m`) captures its receiver, so the delegation reads it from the capture field and
/// puts it first. A constructor reference allocates instead of delegating: the object *is* what the interface
/// method returns.
#[test]
fn a_method_reference_delegates_to_the_method_it_names() {
    let source = r"
public interface Doubler { int apply(int n); }

public interface Reader { int read(Box b); }

public class Box {
    // No declared constructor: the `new` runs the initialiser itself.
    int value = 9;
    int get() { return value; }
}

public class Uses {
    static int twice(int n) { return n * 2; }

    public static int statically(int n) {
        Doubler d = Uses::twice;
        return d.apply(n);
    }

    // Unbound: the interface supplies the receiver as the first argument, which the delegation forwards.
    public static int unbound() {
        Reader r = Box::get;
        return r.read(new Box());
    }
}
";
    assert_invoke(&[source], "statically", &["21"], "42");
    assert_invoke(&[source], "unbound", &[], "9");
}

/// The two reference forms that are not a plain delegation.
///
/// `s::scaled` is *bound*: its receiver is captured when the object is built, so the body reads it out of the
/// capture field and puts it first, being the receiver. `Box::new` allocates — the object is what the interface
/// method returns — and a class with no constructor still needs its field initialisers run, which is the same
/// gap a plain `new` of such a class has and gets the same answer.
#[test]
fn a_bound_and_a_constructor_reference_run() {
    let bound = r"
public interface Doubler { int apply(int n); }

public class Scaler {
    int factor;
    Scaler(int factor) { this.factor = factor; }
    int scaled(int n) { return n * factor; }

    public static int run(int n) {
        Scaler s = new Scaler(3);
        Doubler d = s::scaled;
        return d.apply(n);
    }
}
";
    assert_invoke(&[bound], "run", &["14"], "42");

    let constructing = r"
public interface Maker { Box make(); }

public class Box {
    int v = 5;
    int w;
    { w = v * 4; }
}

public class U {
    public static int run() {
        Maker m = Box::new;
        Box b = m.make();
        return b.v + b.w;
    }
}
";
    assert_invoke(&[constructing], "run", &[], "25");
}

/// An unqualified name that reaches an **inherited** field.
///
/// Name resolution is file-local and a superclass's field is not something it can see, so the name is
/// looked up on the enclosing type and then up the superclass chain, nearest first — the order that makes
/// a shadowing field win. A struct holds its supertype's fields first, so the slot the inherited member
/// lands in is the enclosing type's own, and no separate lookup is needed for it.
#[test]
fn an_unqualified_name_reaches_an_inherited_field() {
    let source = r"
public class Base {
    int seed = 4;
    static int shared = 9;
    long wide = 100L;
}

public class Middle extends Base {
    int own = 1;
}

public class Leaf extends Middle {
    // A field of the same name shadows the inherited one, and the nearest declaration wins.
    int seed = 7;

    int shadowed() { return seed; }

    int inherited() { return own; }

    int statics() { return shared; }

    long widened() { return wide; }

    // Assignment takes the same route, and an inherited name as an *operand* needs the type inference
    // recorded for it.
    int bumped() {
        wide += 5L;
        own++;
        shared = 20;
        own = own + 1;
        return own + (int) wide + shared;
    }
}

public class Reader {
    public static int shadowed() { return new Leaf().shadowed(); }
    public static int inherited() { return new Leaf().inherited(); }
    public static int statics() { return new Leaf().statics(); }
    public static long widened() { return new Leaf().widened(); }
    public static int bumped() { return new Leaf().bumped(); }
}
";
    assert_invoke(&[source], "shadowed", &[], "7");
    assert_invoke(&[source], "inherited", &[], "1");
    assert_invoke(&[source], "statics", &[], "9");
    assert_invoke(&[source], "widened", &[], "100");
    assert_invoke(&[source], "bumped", &[], "128");
}

/// A class is initialised on its first *use*, not at a fixed point in a module-wide sequence.
///
/// JLS §12.4.1 initialises a class when something first reaches it, and one start function cannot
/// express that: a class declared later may be read by one declared earlier. So each class's
/// initialisation is a guarded function of its own, called from the start function in source order and
/// again from every `static` access — the guard makes all but the first call a load and a branch.
///
/// Without it an `enum` constant built from another class's `static` field read that field as zero,
/// which is a wrong value in a module that validates. Both orders are tested, because getting it right
/// only when the dependency happens to be declared first is not getting it right.
#[test]
fn a_class_is_initialised_before_its_statics_are_read() {
    let forward = r"
public class Holder { static int scale = compute(); static int compute() { return 3; } }
public enum Coin {
    ONE(1), TWO(2);
    final int v;
    Coin(int n) { v = n * Holder.scale; }
}
public class Reader { public static int ordered() { return Coin.TWO.v; } }
";
    assert_invoke(&[forward], "ordered", &[], "6");

    let reversed = r"
public enum Coin {
    ONE(1), TWO(2);
    final int v;
    Coin(int n) { v = n * Holder.scale; }
}
public class Holder { static int scale = compute(); static int compute() { return 3; } }
public class Reader { public static int ordered() { return Coin.TWO.v; } }
";
    assert_invoke(&[reversed], "ordered", &[], "6");

    // A field initialiser and a `static { … }` block are one sequence in *source* order (§12.4.2), not
    // two: running every field before every block left `b` as 1.
    let interleaved = r"
public class Seq {
    static int a = one();
    static { a = a * 2; }
    static int b = a;
    static int one() { return 1; }
}
public class Reader { public static int seq() { return Seq.a * 10 + Seq.b; } }
";
    assert_invoke(&[interleaved], "seq", &[], "22");
}

/// A constructor runs what the source does not write: the superclass's construction, then the
/// initialisers.
///
/// Four shapes, because each reaches the chain differently. `this(…)` runs it through the constructor
/// it delegates to and must not run it again; an explicit `super(args)` *is* the call; a class three
/// deep with a middle constructor that delegates nowhere needs the implicit one at every level; and a
/// class with no initialisers of its own is a link in the chain rather than its end.
#[test]
fn every_constructor_reaches_the_superclass_chain_once() {
    let source = r"
public class Base { int b = 1; }

public class Leaf extends Base {
    int seen = 0;
    Leaf() { seen = seen + 10; }
    Leaf(int n) { this(); seen = seen + n; }
}

public class Taking {
    int b = 1;
    int got;
    Taking(int n) { got = n; }
}

public class Passing extends Taking {
    int own = 2;
    Passing() { super(7); }
}

public class A0 { int a = 1; }
public class A1 extends A0 { int b = 2; A1() { } }
public class A2 extends A1 { int c = 4; }

public class Chains {
    public static int delegating() { Leaf l = new Leaf(5); return l.b * 1000 + l.seen; }
    public static int explicitSuper() { Passing p = new Passing(); return p.b * 100 + p.got * 10 + p.own; }
    public static int deep() { A2 x = new A2(); return x.a + x.b + x.c; }
}
";
    assert_invoke(&[source], "delegating", &[], "1015");
    assert_invoke(&[source], "explicitSuper", &[], "172");
    assert_invoke(&[source], "deep", &[], "7");
}

/// `x instanceof T t` on wasm: `ref.test`, then `ref.cast` into the binding on the matching path.
///
/// A wasm local starts at its type's default, so unlike the JVM there is nothing to arrange for the
/// other path: the store goes inside the `if` and that is all. `ref.test`'s non-nullable form is used
/// because Java's `instanceof` is false for a `null` and the nullable form is true for one.
#[test]
fn an_instanceof_pattern_binds_the_narrowed_value() {
    let source = r"
public class Animal { int legs() { return 4; } }
public class Bird extends Animal { int legs() { return 2; } int wings() { return 2; } }
public class Fish extends Animal { int fins() { return 5; } }

public class Count {
    static int parts(Animal a) {
        if (a instanceof Bird b) {
            return b.wings() * 100 + b.legs();
        }
        if (a instanceof Fish f) {
            return f.fins();
        }
        return a.legs();
    }

    public static int bird() { return parts(new Bird()); }
    public static int fish() { return parts(new Fish()); }
    public static int plain() { return parts(new Animal()); }

    // The negated form binds on the branch the test did not take.
    public static int negated() {
        Animal a = new Bird();
        if (!(a instanceof Bird b)) {
            return 0;
        }
        return b.wings();
    }
}
";
    assert_invoke(&[source], "bird", &[], "202");
    assert_invoke(&[source], "fish", &[], "5");
    assert_invoke(&[source], "plain", &[], "4");
    assert_invoke(&[source], "negated", &[], "2");
}

/// A pattern `switch` on wasm, both syntaxes, with a guard.
///
/// A pattern is not a constant, so there is nothing for a `br_table` to index on: the arms' types are
/// tested in source order with `ref.test` and the first match wins (§14.11.1). The guard runs after its
/// pattern bound, because it is written in terms of the binding. A wasm local starts at its type's
/// default, so unlike the JVM's there is nothing to arrange for the arms that did not match.
#[test]
fn a_case_pattern_dispatches_on_the_selector_type() {
    let source = r"
public class Shape { int area() { return 0; } }
public class Square extends Shape { int side = 3; int area() { return side * side; } }
public class Circle extends Shape { int r = 2; int area() { return r * r * 3; } }

public class Which {
    static int classify(Shape s) {
        return switch (s) {
            case Square q when q.side > 5 -> 999;
            case Square q -> q.side;
            case Circle c -> c.r * 100;
            default -> -1;
        };
    }

    // The colon form dispatches the same way; only what happens after the arm is entered differs.
    static int colon(Shape s) {
        switch (s) {
            case Circle c:
                return c.r;
            case Square q:
                return q.side * 10;
            default:
                return 0;
        }
    }

    public static int square() { return classify(new Square()); }

    // The guard is taken here and skipped above, on the same arm.
    public static int guarded() {
        Square q = new Square();
        q.side = 9;
        return classify(q);
    }

    public static int circle() { return classify(new Circle()); }
    public static int plain() { return classify(new Shape()); }
    public static int colonSquare() { return colon(new Square()); }
    public static int colonCircle() { return colon(new Circle()); }
}
";
    assert_invoke(&[source], "square", &[], "3");
    assert_invoke(&[source], "guarded", &[], "999");
    assert_invoke(&[source], "circle", &[], "200");
    assert_invoke(&[source], "plain", &[], "-1");
    assert_invoke(&[source], "colonSquare", &[], "30");
    assert_invoke(&[source], "colonCircle", &[], "2");
}

/// try-with-resources beside a `catch` and a `finally`, and a `close` a subclass overrode.
///
/// §14.20.3 makes the resource `try` the *body* of an ordinary one, so the two compose rather than the
/// close sequence being copied into every handler. The `close` that runs is chosen by the receiver's
/// runtime type, the same `ref.test` chain a call site builds — the declared type only named the method.
/// Both were reported before, the second on the ground that there was no call expression to read a
/// receiver out of, which is a signature and not a design.
#[test]
fn a_resource_closes_beside_a_catch_and_on_its_runtime_type() {
    let source = r"
public class Boom extends RuntimeException { Boom() {} }

public class Res implements AutoCloseable {
    static int closed = 0;
    int mark = 1;
    public void close() { closed = closed + mark; }
}

public class Sub extends Res {
    public void close() { closed = closed + 100; }
}

public class Using {
    public static int caught() {
        Res.closed = 0;
        int seen = 0;
        try (Res r = new Res()) {
            throw new Boom();
        } catch (Boom b) {
            seen = 7;
        } finally {
            seen = seen + 30;
        }
        return Res.closed * 1000 + seen;
    }

    public static int overridden() {
        Res.closed = 0;
        try (Res r = new Sub()) {
            Res.closed = Res.closed + 1;
        }
        return Res.closed;
    }

    public static int normal() {
        Res.closed = 0;
        int seen = 0;
        try (Res r = new Res()) {
            seen = 1;
        } finally {
            seen = seen + 4;
        }
        return Res.closed * 100 + seen;
    }
}
";
    // The same three answers a real JVM gives for this source.
    assert_invoke(&[source], "caught", &[], "1037");
    assert_invoke(&[source], "overridden", &[], "101");
    assert_invoke(&[source], "normal", &[], "105");
}

/// An `enum` constant with a body, which is an anonymous subclass of the enum.
///
/// The struct it gets holds the enum's fields first, exactly as any subclass's does, which is what lets
/// the body read `scale` and what makes the existing `ref.test` chain dispatch `apply` to it. The
/// constant's global still has the enum's type; only what is allocated changes.
///
/// The body's *own* field initialisers run after the enum's construction and are reached from the
/// constant site, nothing else knowing about them: the enum's constructor knows nothing of the subclass,
/// and running them from the body's synthesised `super()` would run the enum's twice. `extra` is what
/// checks that — a constant with both arguments and a body used to read it back as zero.
///
/// The JVM test compiles the same enum, so the two backends' answers are compared against each other and
/// against a real JVM's.
#[test]
fn an_enum_constant_with_a_body_is_its_own_subclass() {
    let source = r"
public enum Op {
    ADD { int apply(int a, int b) { return a + b; } },
    MUL(2) { int extra = 7; int apply(int a, int b) { return a * b * scale + extra; } };

    final int scale;

    Op() { this.scale = 1; }

    Op(int scale) { this.scale = scale; }

    int apply(int a, int b) { return 0; }

    // A concrete member the bodies inherit rather than override, whose self-call still dispatches.
    int twice(int n) { return apply(n, n); }
}

public class Reader {
    public static int add() { return Op.ADD.apply(2, 3); }
    public static int mul() { return Op.MUL.apply(2, 3); }
    public static int twiceAdd() { return Op.ADD.twice(4); }
    public static int twiceMul() { return Op.MUL.twice(4); }
    public static int same() { return Op.ADD == Op.ADD ? 1 : 0; }
}
";
    assert_invoke(&[source], "add", &[], "5");
    assert_invoke(&[source], "mul", &[], "19");
    assert_invoke(&[source], "twiceAdd", &[], "8");
    assert_invoke(&[source], "twiceMul", &[], "39");
    assert_invoke(&[source], "same", &[], "1");
}

/// A `record` pattern on wasm, which deconstructs.
///
/// The recursive case: test the type, then read each component through its *accessor* — which is what a
/// deconstruction calls (§14.30.1), a record being free to declare one by hand — and match the component
/// pattern against that. `_` matches anything and binds nothing, so it emits nothing at all. Two forms
/// carry no test: a primitive component, and one of the component's *own* type — the latter matches
/// unconditionally (§14.30.2), including a `null` component that a `ref.test` would reject. `var` is that
/// same case spelled without the type, and its binding takes the component's.
///
/// The selector is an interface rather than `Object`: a wasm host has no `java.base`, and an interface
/// type is held at the top of the reference hierarchy, which is exactly what a pattern narrows from.
#[test]
fn a_record_pattern_deconstructs() {
    let source = r"
public interface Node {}
public record Point(int x, int y) implements Node {}
public record Line(Point a, Point b) implements Node {}
public class Shape implements Node { int tag = 0; }

public class Deconstruct {
    static int total(Node o) {
        return switch (o) {
            case Line(Point(int x, int y), Point a) -> x + y + a.x() + a.y();
            case Point(int x, _) -> x;
            default -> -1;
        };
    }

    // `var` is the ordinary spelling: the component pattern's type *is* the component's.
    static int summed(Node o) {
        return switch (o) {
            case Point(var x, var y) -> x + y;
            case Line(var a, var b) -> (a == null ? 100 : 0) + (b == null ? 20 : 0);
            default -> -1;
        };
    }

    public static int varComponents() { return summed(new Point(3, 4)); }

    // A `null` component still matches a pattern of the component's own type (§14.30.2), which a
    // `ref.test` would have rejected.
    public static int nullComponent() { return summed(new Line(null, new Point(1, 1))); }

    public static int line() { return total(new Line(new Point(1, 2), new Point(3, 4))); }
    public static int point() { return total(new Point(9, 8)); }
    public static int other() { return total(new Shape()); }

    public static int tested() {
        Node o = new Point(5, 6);
        if (o instanceof Point(int x, int y)) {
            return x * 10 + y;
        }
        return 0;
    }
}
";
    assert_invoke(&[source], "line", &[], "10");
    assert_invoke(&[source], "point", &[], "9");
    assert_invoke(&[source], "other", &[], "-1");
    assert_invoke(&[source], "tested", &[], "56");
    assert_invoke(&[source], "varComponents", &[], "7");
    assert_invoke(&[source], "nullComponent", &[], "100");
}

/// Four shapes that a report sweep would not have found, because none of them was reported.
///
/// An interface's members reach a `default` body, a `static` one, and a field — the last is implicitly
/// `static final` (§9.3), and a class with only those used to be given a synthesised *constructor*,
/// which panicked on an interface's missing struct type. A `char` is an unsigned 16-bit integer and is
/// an `i32` here like every other integral type narrower than `long`; its literal was reported. A
/// `continue` naming a loop from inside a `switch` crosses two structures at once.
#[test]
fn an_interface_member_a_char_and_a_labelled_jump_run() {
    let source = r"
public interface Sized {
    int LIMIT = 7;

    static int of(int n) { return n * 2; }

    default int doubled() { return of(3); }
}

public class Thing implements Sized {}

public class Reader {
    public static int fromDefault() { return new Thing().doubled(); }
    public static int fromStatic() { return Sized.of(4); }
    public static int fromField() { return Sized.LIMIT; }

    public static int labelled() {
        int total = 0;
        outer: for (int i = 0; i < 3; i++) {
            switch (i) {
                case 1:
                    continue outer;
                default:
                    total += i;
            }
        }
        return total;
    }

    public static int chars() {
        char c = 'a';
        c += 1;
        return c;
    }
}
";
    assert_invoke(&[source], "fromDefault", &[], "6");
    assert_invoke(&[source], "fromStatic", &[], "8");
    assert_invoke(&[source], "fromField", &[], "7");
    assert_invoke(&[source], "labelled", &[], "2");
    assert_invoke(&[source], "chars", &[], "98");
}

/// A jump inside a `finally` runs only the cleanups *outside* that `finally`.
///
/// The cleanup a `return` or a `break` leaves behind used to be lowered against the whole open set,
/// including itself — so a jump inside one re-entered it, and the compiler recursed until its own
/// stack ran out. An abort is not a report, which is the one property this backend does claim.
///
/// The answers are Java's: a `return` in a `finally` discards the one the `try` had (§14.20.2), a
/// `break` in a `finally` discards the pending `return`, and the enclosing cleanup still runs once.
#[test]
fn a_jump_inside_a_finally_runs_only_the_outer_cleanups() {
    let source = r"
public class Unwind {
    public static int returnsFromFinally() {
        try {
            return 1;
        } finally {
            return 2;
        }
    }

    public static int breaksFromFinally() {
        int n = 0;
        for (int i = 0; i < 3; i++) {
            try {
                n += 1;
                break;
            } finally {
                break;
            }
        }
        return n;
    }

    public static int breakDiscardsAReturn() {
        for (int i = 0; i < 3; i++) {
            try {
                return 1;
            } finally {
                break;
            }
        }
        return 0;
    }

    public static int outerCleanupStillRuns() {
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
}
";
    assert_invoke(&[source], "returnsFromFinally", &[], "2");
    assert_invoke(&[source], "breaksFromFinally", &[], "1");
    assert_invoke(&[source], "breakDiscardsAReturn", &[], "0");
    assert_invoke(&[source], "outerCleanupStillRuns", &[], "11");
}
