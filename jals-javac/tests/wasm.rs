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

/// A `static` initialiser a constant expression cannot hold is reported, not dropped.
///
/// wasm's constant expressions are a short list — no calls, no arithmetic, no conversions. Emitting the
/// type's default instead would be the same defect as a missing `<clinit>` on the JVM side: a field
/// holding the wrong value in a module that validates.
#[test]
fn a_computed_static_initialiser_is_reported() {
    for (source, expected) in [
        (
            "public class C { static int n = 1 + 1; }",
            "a `static` field initialiser that is no constant",
        ),
        (
            "public class C { static int n = someMethod(); static int someMethod() { return 1; } }",
            "a `static` field initialiser that is no constant",
        ),
    ] {
        let error = compile(&[source]).expect_err("this initialiser is no constant expression");
        assert!(
            matches!(error, WasmError::Unsupported(what) if what == expected),
            "`{source}` should report {expected:?}, got {error}"
        );
    }
}

/// An array-typed field, in a struct and in a global.
///
/// A field's type needs an array type index and an array's element may be a class, so neither can be
/// laid out before the other. Every type lives in one recursive group, so the fix is index
/// pre-assignment rather than anything to do with wasm's type system: classes reserve their indices,
/// then the arrays get theirs, then the struct bodies are written. Before that, an `int[] cells;`
/// field reported that `int[]` had no wasm representation — which is not true of `int[]`.
#[test]
fn an_array_typed_field_is_laid_out() {
    let source = r"
public class Bag {
    int[] cells;
    Bag[] siblings;
    static int[] shared;

    public static int through_a_field(int n) {
        Bag bag = new Bag();
        bag.cells = new int[3];
        bag.cells[1] = n;
        bag.cells[1] += 4;
        int total = 0;
        for (int cell : bag.cells) {
            total += cell;
        }
        return total;
    }

    // An array *of* the class whose field it is: the recursive group is what makes this legal.
    public static int through_a_self_array(int n) {
        Bag bag = new Bag();
        bag.siblings = new Bag[2];
        bag.siblings[0] = new Bag();
        bag.siblings[0].cells = new int[1];
        bag.siblings[0].cells[0] = n;
        return bag.siblings[0].cells[0] + bag.siblings.length;
    }

    public static int through_a_global(int n) {
        shared = new int[2];
        shared[0] = n;
        shared[1] = n * 2;
        return shared[0] + shared[1];
    }
}
";
    assert_invoke(&[source], "through_a_field", &["5"], "9");
    assert_invoke(&[source], "through_a_self_array", &["7"], "9");
    assert_invoke(&[source], "through_a_global", &["3"], "9");
}

/// Every statement form is either compiled or reports *itself*.
///
/// A catch-all report says only "this statement form", which sends a reader looking. Each of these
/// waits on the same thing — the exception-handling proposal's `tag` section and `try_table`, which the
/// encoder does not write yet — and saying so is the difference between a compiler that is missing a
/// feature and one that looks broken.
///
/// `assert` is the exception: Java evaluates one only when assertions are *enabled*, they are disabled
/// by default, and a wasm host has no `-ea` to turn them on. So it compiles to nothing, which is
/// exactly what a JVM does with one by default. A trap would be stricter than Java.
#[test]
fn each_uncompiled_statement_form_names_itself() {
    let body = |statement: &str| {
        format!(
            "public class S {{ public static int run(int n) {{ {statement} return n; }} }}\n\
             class Boom extends RuntimeException {{}}\n"
        )
    };
    for (statement, expected) in [
        ("throw new Boom();", "a `throw`"),
        ("try { n = 1; } catch (Boom b) { n = 2; }", "a `try`"),
        (
            "synchronized (S.class) { n = 1; }",
            "a `synchronized` block",
        ),
    ] {
        let source = body(statement);
        let error = compile(&[&source]).expect_err("this form is not compiled yet");
        assert!(
            matches!(error, WasmError::Unsupported(what) if what == expected),
            "`{statement}` should report {expected:?}, got {error}"
        );
    }
    // An `assert` compiles, and to nothing: the module runs as if assertions were disabled.
    let source = "public class S { public static int run(int n) { assert n > 0; return n; } }";
    assert_invoke(&[source], "run", &["-5"], "-5");
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

/// A `static { … }` block is not an instance initialiser, and running it in every constructor would be
/// a different program. Reported until the module has a start function to run it once.
#[test]
fn a_static_initialiser_block_is_reported() {
    let source = "public class S { static int n; static { n = 1; } S() {} }";
    let error = compile(&[source]).expect_err("a static initialiser needs a start function");
    assert!(
        matches!(
            error,
            WasmError::Unsupported("a `static` initialiser block")
        ),
        "got {error}"
    );
}

/// A `static` nested class is simply another struct type.
///
/// wasm's type space is flat and has no naming convention to satisfy, so there is nothing for a nested
/// class to be nested *in*. Walking only the root's children dropped every one of them silently — the
/// type never existed, and a call to one of its methods reported an unresolved name pointing nowhere
/// useful. A non-`static` one is reported instead: it holds its enclosing instance in a synthetic field
/// and takes it as an extra constructor parameter, so its constructor would be one parameter short of
/// what a `new` passes.
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

    let inner = "public class O { class I { int f; } }";
    let error = compile(&[inner]).expect_err("an inner class needs a synthetic parameter");
    assert!(
        matches!(error, WasmError::Unsupported("a non-`static` inner class")),
        "got {error}"
    );
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
        ("interface I { int f(); }", "an `interface` declaration"),
        ("enum E { A, B }", "an `enum` declaration"),
        ("record R(int x) {}", "a `record` declaration"),
        ("@interface M {}", "an `@interface` declaration"),
        // Nested, which is where the silent drop used to happen.
        (
            "public class O { interface I {} }",
            "an `interface` declaration",
        ),
    ] {
        let error = compile(&[source]).expect_err("this declaration is not laid out yet");
        assert!(
            matches!(error, WasmError::Unsupported(what) if what == expected),
            "`{source}` should report {expected:?}, got {error}"
        );
    }
}

/// Each uncompiled expression form names itself too.
///
/// An array initialiser is the one array form still missing. A lambda, a method reference, and `.class`
/// each name themselves too, but none is reachable from a compiling program yet: every one of them
/// needs a *target type* the backend reports first — an interface it does not lay out, or a library
/// type it has no representation for.
#[test]
fn each_uncompiled_expression_form_names_itself() {
    let source = "public class E { public static int run(int n) { int[] a = {1, 2}; return n; } }";
    let error = compile(&[source]).expect_err("an array initialiser is not compiled yet");
    assert!(
        matches!(error, WasmError::Unsupported("an array initialiser")),
        "got {error}"
    );
}
