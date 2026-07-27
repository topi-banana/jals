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

fn java_available() -> bool {
    Command::new("java")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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
