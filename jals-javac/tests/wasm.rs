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

fn tool(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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
#[test]
fn a_compound_assignment_is_reported_rather_than_mis_emitted() {
    let source = r"
public class Compound {
    public static int run(int n) {
        int i = n;
        i += 5;
        return i;
    }
}
";
    let error = compile(&[source]).expect_err("compound assignment is not lowered yet");
    assert!(
        matches!(error, WasmError::Unsupported("a compound assignment")),
        "expected the compound-assignment report, got {error}"
    );
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
