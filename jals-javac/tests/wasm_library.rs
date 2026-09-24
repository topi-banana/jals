//! What a linked library emits: its canonical surface and the ABI section beside it.
//!
//! The counterpart of `wasm_lower.rs`, for the other surface [`CompileWasm`] can produce. A
//! project's module is a program — bare-name exports, no factories — and a library's is a
//! *link target*: every non-private member under a key derived from its declaration, a factory
//! for each constructor, an accessor pair for each `static` field, and a custom section a
//! consumer reads to replay the types. These tests pin both what is exported and what the
//! section says, without an engine.

use std::io::Write as _;
use std::process::{Command, Stdio};

use jals_hir::{FileAnalysis, FileId, FileSemantics, ProjectIndex, TypedFile};
use jals_javac::wasm::{CompileWasm, LibraryAbi, Module, Source, WasmOptions};
use jals_syntax::SyntaxNode;

/// A library compiled from `sources`, plus the ABI it states.
fn library_of(sources: &[(&str, &str)], package: &str) -> (Module, LibraryAbi) {
    let roots: Vec<(FileId, SyntaxNode)> = sources
        .iter()
        .enumerate()
        .map(|(index, (_, text))| {
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
    let published: Vec<Source> = sources
        .iter()
        .map(|(path, text)| Source {
            path: (*path).to_owned(),
            text: (*text).to_owned(),
        })
        .collect();
    CompileWasm::library(
        &typed,
        &index,
        WasmOptions::default(),
        package,
        1,
        published,
    )
    .unwrap_or_else(|error| panic!("compile: {error}"))
}

/// Whether a tool that understands WebAssembly 3.0 is on this host; a missing one stands the
/// check down loudly, because it is the only authority on whether the module is well-formed.
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

const COUNTER: &str = r"
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

    private static int secret() {
        return 1;
    }
}
";

fn counter() -> (Module, LibraryAbi) {
    library_of(&[("demo/Counter.java", COUNTER)], "demo")
}

/// The exported names, sorted, so an expectation reads as the whole surface.
fn exports(module: &Module) -> Vec<String> {
    let mut names: Vec<String> = module
        .exports
        .iter()
        .map(|(name, _, _)| name.clone())
        .collect();
    names.sort();
    names
}

/// Every member a consumer can name is exported under the key its declaration spells, and the
/// shapes a consumer cannot call — a constructor's `this`, a `static` field's global — are
/// replaced by a factory and an accessor pair.
#[test]
fn a_library_exports_its_members_under_canonical_keys() {
    let (module, _) = counter();
    let names = exports(&module);
    for expected in [
        "demo/Counter#<init>(I)V",
        "demo/Counter#next()I",
        "demo/Counter#twice(I)I",
        "demo/Counter#TOTAL#get",
        "demo/Counter#TOTAL#put",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "expected `{expected}` among {names:?}"
        );
    }
    assert!(
        !names.iter().any(|name| name.contains("secret")),
        "a private member is not part of the surface: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "twice"),
        "a library's surface is keys, not the bare names a program exports: {names:?}"
    );
}

/// The module carries its own description: the type groups a consumer replays, the class map,
/// and the Java the consumer's index resolves against.
#[test]
fn a_library_carries_its_abi_in_the_module() {
    let (module, abi) = counter();
    assert_eq!(abi.package, "demo");
    assert_eq!(abi.version, 1);
    assert!(
        abi.sources
            .iter()
            .any(|source| source.path == "demo/Counter.java"),
        "the API text is the Java that was compiled: {:?}",
        abi.sources
    );
    let class = abi
        .classes
        .iter()
        .find(|class| class.name == "demo/Counter")
        .expect("the class map names the class");
    assert!(
        usize::try_from(class.index).expect("an index that fits") < abi.types.len(),
        "the class's struct is one of the replayed types"
    );

    let bytes = module.finish().expect("a module whose lengths all fit");
    let read = LibraryAbi::of_module(&bytes).expect("the module carries its ABI");
    assert_eq!(read, abi, "the section and the compile agree");
    assert_eq!(LibraryAbi::read(&abi.write()).expect("round trip"), abi);
}

/// The library module is a module: the same validator a project's module has to pass accepts it.
#[test]
fn a_library_module_validates() {
    let (module, _) = counter();
    validate(&module.finish().expect("a module whose lengths all fit"));
}

/// The factory is the constructor plus an allocation, so the engine runs the constructor against
/// an object the consumer never had to lay out.
#[test]
fn a_constructor_is_exported_as_a_factory() {
    let (module, _) = counter();
    let (_, kind, index) = module
        .exports
        .iter()
        .find(|(name, ..)| name == "demo/Counter#<init>(I)V")
        .expect("the factory is exported");
    assert!(matches!(kind, jals_javac::wasm::ExportKind::Func));
    let defined = usize::try_from(*index).expect("an index that fits")
        - usize::try_from(module.func_index(0)).expect("a function import count that fits");
    let func = &module.funcs[defined];
    // `struct.new_default`, then the object into a local, then `this` and the parameter, then the
    // constructor, then the object back out.
    assert!(
        matches!(
            func.body.first(),
            Some(jals_javac::wasm::Instr::StructNewDefault(_))
        ),
        "the factory allocates: {:?}",
        func.body.first()
    );
    assert!(
        matches!(func.body.last(), Some(jals_javac::wasm::Instr::LocalGet(_))),
        "the factory returns the object: {:?}",
        func.body.last()
    );
}
