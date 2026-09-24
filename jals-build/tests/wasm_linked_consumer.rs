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

/// Compile the library, compile the project against it, link the two, and return what `run`
/// answered.
fn linked_run() -> i32 {
    let texts = [PROJECT, LIBRARY];
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
        vec![Source {
            path: "demo/Counter.java".to_owned(),
            text: LIBRARY.to_owned(),
        }],
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
    assert_eq!(linked_run(), 52);
}
