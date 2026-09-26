//! A project compiled against a precompiled library, linked and run.
//!
//! The compiler twice: once for the library — [`CompileWasm::library`] — and once for the project
//! that links it, over an index that holds the library's published Java. The two modules then meet
//! in one `Store`, and the project's code calls into the library the way it would call into any
//! other class. This is the arrangement `jals.library` and [`LinkedLibrary`] exist for, at the
//! level a user reaches it: Java in, a linked program out.

#![cfg(feature = "wasm-run")]

use jals_hir::{FileAnalysis, FileId, FileSemantics, ProjectIndex, TypedFile};
use jals_javac::wasm::{CompileWasm, LinkedLibrary, Source, WasmOptions};
use std::io::Write as _;
use std::process::{Command, Stdio};

use tinywasm::{Imports, ModuleInstance, Store, WasmValue, parse_bytes};

/// Hand the module to `wasm-tools`, which is the specification's own answer to whether it is
/// well-formed; a host without the tool checks less, loudly.
fn validate(bytes: &[u8]) {
    let present = Command::new("wasm-tools")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !present {
        eprintln!(
            "note: `wasm-tools` is not installed; this test is checking less than it looks like"
        );
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

const LIBRARY: &str = r"
package demo;

public class Counter {
    public static int TOTAL = 7;

    private int count;

    public Counter(int start) {
        this.count = start;
    }

    public int next() {
        this.count = this.count + 1;
        return this.count;
    }

    public static int twice(int n) {
        return n + n;
    }
}
";

const PROJECT: &str = r"
package app;

import demo.Counter;

public class Main {
    public static int run() {
        Counter.TOTAL = 9;
        Counter c = new Counter(40);
        return c.next() + Counter.twice(1) + Counter.TOTAL;
    }
}
";

/// Compile the library, then the project against it, and return `(library, project)` bytes.
fn compiled_pair(project_source: &str, library_sources: &[(&str, &str)]) -> (Vec<u8>, Vec<u8>) {
    let texts: Vec<&str> = std::iter::once(project_source)
        .chain(library_sources.iter().map(|(_, text)| *text))
        .collect();
    let roots: Vec<(FileId, jals_syntax::SyntaxNode)> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            (
                FileId(u32::try_from(index).expect("a source count that fits")),
                jals_exec::block_on_inline(jals_syntax::Parse::parse(text)).syntax(),
            )
        })
        .collect();
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&roots).with_stdlib().build());
    let analyses: Vec<FileAnalysis> = roots
        .iter()
        .map(|(_, root)| jals_exec::block_on_inline(FileAnalysis::of(root)))
        .collect();
    let semantics: Vec<FileSemantics<'_>> = roots
        .iter()
        .zip(&analyses)
        .map(|((file, _), analysis)| analysis.in_project(&index, *file))
        .collect();
    let typed: Vec<TypedFile<'_>> = semantics
        .iter()
        .map(|binding| jals_exec::block_on_inline(binding.typed()))
        .collect();
    // The project wrote its source first, the library second; the split is the only thing the two
    // lists decide, and both compiles read the whole index for resolution.
    let (project, library) = typed.split_at(1);

    let (library_module, abi) = CompileWasm::library(
        library,
        &index,
        WasmOptions::default(),
        "lib",
        1,
        library_sources
            .iter()
            .map(|(path, text)| Source {
                path: (*path).to_owned(),
                text: (*text).to_owned(),
            })
            .collect(),
    )
    .expect("the library compiles");
    let linked = [LinkedLibrary {
        name: "lib",
        abi: &abi,
    }];
    let project_bytes =
        CompileWasm::project_linked(project, &[], &linked, &index, WasmOptions::default())
            .expect("the project compiles");

    let library_bytes = library_module.finish().expect("the library encodes");
    validate(&library_bytes);
    validate(&project_bytes);
    (library_bytes, project_bytes)
}

/// Compile the pair, link the two with the engine directly, and return what `run` answered.
fn linked_run(project_source: &str, library_sources: &[(&str, &str)]) -> i32 {
    let (library_bytes, project_bytes) = compiled_pair(project_source, library_sources);
    let library = parse_bytes(&library_bytes).expect("the library parses");
    let project = parse_bytes(&project_bytes).expect("the project parses");

    let mut store = Store::default();
    let library_instance =
        ModuleInstance::instantiate(&mut store, &library, None).expect("the library instantiates");
    let mut imports = Imports::new();
    imports
        .link_module("lib", library_instance)
        .expect("the library registers");
    let project_instance = ModuleInstance::instantiate(&mut store, &project, Some(&imports))
        .expect("the project links");

    let func = project_instance
        .func_untyped(&store, "run")
        .expect("the project exports `run`");
    let mut results = [WasmValue::I32(0)];
    func.call(&mut store, &[], &mut results)
        .expect("`run` executes");
    match results {
        [WasmValue::I32(value)] => value,
        other => panic!("expected one i32, got {other:?}"),
    }
}

/// A constructor the library exports is callable, a `static` field crosses in both directions,
/// and an instance method runs against an object the library allocated.
#[test]
fn a_project_calls_into_a_precompiled_library() {
    assert_eq!(linked_run(PROJECT, &[("demo/Counter.java", LIBRARY)]), 52);
}

/// A linked class the library gives an implicit constructor, and a constructor *reference* to it.
///
/// Both shapes missed the factory: the class declares no constructor, so the only construction
/// code is the synthesized initialiser, and `Made::new` reaches the same constructor through the
/// method-reference path rather than an explicit `new`. The value read back says whether the
/// initialiser ran — a factory that allocated without calling it leaves the default zero.
const IMPLICIT_LIBRARY: &str = r"
package demo;

public class Made {
    private int value = 41;

    public int read() {
        return this.value;
    }
}
";

const IMPLICIT_PROJECT: &str = r"
package app;

import demo.Made;

interface Maker {
    Made make();
}

public class Main {
    public static int run() {
        Maker maker = demo.Made::new;
        return maker.make().read();
    }
}
";

#[test]
fn a_constructor_reference_to_a_linked_class_runs_its_implicit_constructor() {
    assert_eq!(
        linked_run(IMPLICIT_PROJECT, &[("demo/Made.java", IMPLICIT_LIBRARY)]),
        41
    );
}

/// An inner class is reached through the outer instance the factory leads with.
///
/// The library's factory takes the enclosing instance as its first parameter — its in-module
/// constructor shape's parameter after `this` — and writes it into the synthetic field, because
/// the synthesized initialiser's own signature is the object alone. A consumer that emitted only
/// the declared arguments would call one short, and one that passed an enclosing instance to a
/// factory that did not want it left a value on the stack; both are modules no validator accepts.
const INNER_LIBRARY: &str = r"
package demo;

public class Outer {
    private int base;

    public Outer(int base) {
        this.base = base;
    }

    public class Inner {
        public int next() {
            return base + 1;
        }
    }
}
";

const INNER_PROJECT: &str = r"
package app;

import demo.Outer;

public class Main {
    public static int run() {
        Outer outer = new Outer(40);
        Outer.Inner inner = outer.new Inner();
        return inner.next();
    }
}
";

#[test]
fn a_project_constructs_a_linked_inner_class_through_its_outer_instance() {
    assert_eq!(
        linked_run(INNER_PROJECT, &[("demo/Outer.java", INNER_LIBRARY)]),
        41
    );
}

/// A `catch` catches the library's tag because the project *imports* it.
///
/// The project declares no tag of its own when a library exports one: two tags with the same
/// payload type are two different tags, and a `try_table` naming the local one would never see
/// the library's `throw`. Without the import the exception escapes the `try` and the call fails.
const SURPRISE_LIBRARY: &str = r"
package demo;

public class Surprise extends RuntimeException {
}
";

const THROWING_LIBRARY: &str = r"
package demo;

public class Thrower {
    public static void boom() {
        throw new Surprise();
    }
}
";

const CATCHING_PROJECT: &str = r"
package app;

import demo.Thrower;

public class Main {
    public static int run() {
        try {
            Thrower.boom();
            return 1;
        } catch (demo.Surprise e) {
            return 2;
        }
    }
}
";

#[test]
fn a_project_catches_an_exception_a_linked_library_throws() {
    assert_eq!(
        linked_run(
            CATCHING_PROJECT,
            &[
                ("demo/Surprise.java", SURPRISE_LIBRARY),
                ("demo/Thrower.java", THROWING_LIBRARY),
            ]
        ),
        2
    );
}

/// Legal Java this backend cannot compile yet reports what is missing.
///
/// The struct is shared, so the data is reachable — but the slots were laid out in the library's
/// own module, and deriving them from the replayed struct type would be inventing the declaration
/// order. The point of the test is the *diagnostic*: it used to say the field "did not resolve",
/// sending a reader looking for a typo in a program that is legal Java.
const FIELD_LIBRARY: &str = r"
package demo;

public class Point {
    public int x = 7;
}
";

const FIELD_PROJECT: &str = r"
package app;

import demo.Point;

public class Main {
    public static int run() {
        Point p = new Point();
        return p.x;
    }
}
";

#[test]
#[should_panic(expected = "an instance field of a linked library class")]
fn reading_a_linked_instance_field_names_the_missing_capability() {
    linked_run(FIELD_PROJECT, &[("demo/Point.java", FIELD_LIBRARY)]);
}

/// The same program through the runner a `jals run` uses: the bytes arrive as a `wasm` dependency
/// does, the library is instantiated first, and the project links against it by name.
#[test]
fn the_runner_links_a_wasm_dependency() {
    let (library_bytes, project_bytes) = compiled_pair(PROJECT, &[("demo/Counter.java", LIBRARY)]);
    let outcome = jals_build::WasmRunner::run(&jals_build::WasmRunRequest {
        module: &project_bytes,
        invoke: Some("run"),
        args: &[],
        natives: &jals_native::NativeBindings::new(),
        libraries: &[jals_build::WasmLibrary {
            name: "lib",
            bytes: &library_bytes,
        }],
        progress: &jals_progress::Progress::SILENT,
    })
    .expect("the run links and executes");
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(52)])
    );
}

/// A library's **own** host imports are linked too, not only the project's.
///
/// A `native` method the library's Java declares is an import of the *library's* module, and the
/// project's import section says nothing about it — the project never has to call the method for
/// the declaration to be there. Before the sweep, the library could not be instantiated at all
/// (`linking error: unknown import: demo/Caller.answer()I`) even with the exact owner and
/// signature selected in `natives`.
const HOST_LIBRARY: &str = r"
package demo;

public class Caller {
    public static native int answer();

    public static int call() {
        return answer();
    }
}
";

const HOST_PROJECT: &str = r"
package app;

import demo.Caller;

public class Main {
    public static int run() {
        return Caller.call();
    }
}
";

#[test]
fn the_runner_links_a_librarys_host_imports() {
    let (library_bytes, project_bytes) =
        compiled_pair(HOST_PROJECT, &[("demo/Caller.java", HOST_LIBRARY)]);
    let mut registry = jals_native::NativeRegistry::new();
    let mut package = jals_native::NativePackage::new("demo.host", 1);
    package.bind(
        "demo/Caller",
        "answer()I",
        |_host: &mut dyn jals_native::NativeHost,
         _args: jals_native::Args<'_>,
         mut results: jals_native::Results<'_>| {
            results.set(0, jals_native::NativeValue::I32(42));
            Ok::<(), jals_native::NativeError>(())
        },
    );
    registry.add(package);
    let selected = registry
        .select(&["demo.host".to_owned()])
        .expect("the package selects");
    let outcome = jals_build::WasmRunner::run(&jals_build::WasmRunRequest {
        module: &project_bytes,
        invoke: Some("run"),
        args: &[],
        natives: &selected.bindings(),
        libraries: &[jals_build::WasmLibrary {
            name: "lib",
            bytes: &library_bytes,
        }],
        progress: &jals_progress::Progress::SILENT,
    })
    .expect("the run links and executes");
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(42)])
    );
}
