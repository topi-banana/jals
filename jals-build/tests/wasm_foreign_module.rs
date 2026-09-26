//! A foreign core module satisfying a project's `native` declarations.
//!
//! The other side of the seam a linked library is: the project's own Java declares the API (a
//! `native` method is an import), and a module this workspace's backend never emitted — a Rust
//! library compiled through a WIT interface, say — provides the implementation. What the two
//! sides agree on is the canonical key: the declaring class's internal name joined to the
//! method's name-with-descriptor, which is the same string the backend writes into the import
//! section.
//!
//! The boundary is scalars here. A reference to a GC object is not something a core module can
//! hold, so a foreign module cannot satisfy an import whose signature mentions one — the engine
//! refuses the link, and the compiler refuses the declaration the same way a host package would.

#![cfg(feature = "wasm-run")]

use jals_hir::{FileAnalysis, FileId, FileSemantics, ProjectIndex, TypedFile};
use jals_javac::wasm::{
    CompType, CompileWasm, ExportKind, Func, Import, ImportKind, Insn, Module, NumOp, SubType,
    ValType, WasmOptions,
};
use jals_syntax::SyntaxNode;

const PROJECT: &str = r"
package demo;

public class Main {
    public static int run() {
        return Native.twice(21);
    }
}
";

/// The Java side of the interface: a declaration with no body, which is an import.
const NATIVE: &str = r"
package demo;

public class Native {
    public static native int twice(int n);
}
";

/// The project's own module, compiled from `texts` — what every run here starts from.
fn compile(texts: &[&str]) -> Vec<u8> {
    let roots: Vec<(FileId, SyntaxNode)> = texts
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
    CompileWasm::project(&typed, &[], &index, WasmOptions::default()).expect("the project compiles")
}

/// A hand-built foreign module: one export, `demo/Native#twice(I)I`, doubling its argument.
fn foreign_module() -> Module {
    let mut module = Module::new();
    let ty = module.add_type(SubType::plain(CompType::Func {
        params: vec![ValType::I32],
        results: vec![ValType::I32],
    }));
    let mut body = Insn::new();
    body.local_get(0)
        .i32_const(2)
        .numeric(NumOp::Mul, ValType::I32)
        .expect("i32.mul");
    let function = module.func_index(0);
    module.funcs.push(Func {
        type_index: ty,
        locals: Vec::new(),
        body: body.into_body(),
    });
    module.exports.push((
        "demo/Native#twice(I)I".to_owned(),
        ExportKind::Func,
        function,
    ));
    module
}

/// Compile the project and run it against the foreign module.
#[test]
fn a_foreign_module_satisfies_a_native_declaration() {
    let project_bytes = compile(&[PROJECT, NATIVE]);
    let foreign_bytes = foreign_module()
        .finish()
        .expect("the foreign module encodes");

    let outcome = jals_build::WasmRunner::run(&jals_build::WasmRunRequest {
        module: &project_bytes,
        invoke: Some("run"),
        args: &[],
        natives: &jals_native::NativeBindings::new(),
        libraries: &[],
        foreign: &[jals_build::WasmForeignModule {
            name: "core",
            bytes: &foreign_bytes,
        }],
        progress: &jals_progress::Progress::SILENT,
    })
    .expect("the run links and executes");
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(42)])
    );
}

/// Without the module, the import is reported with the key that was missing — the diagnostic a
/// project gets when the foreign half is not linked at all.
#[test]
fn without_the_module_the_import_is_unresolved() {
    let project_bytes = compile(&[PROJECT, NATIVE]);

    let error = jals_build::WasmRunner::run(&jals_build::WasmRunRequest {
        module: &project_bytes,
        invoke: Some("run"),
        args: &[],
        natives: &jals_native::NativeBindings::new(),
        libraries: &[],
        foreign: &[],
        progress: &jals_progress::Progress::SILENT,
    })
    .expect_err("the import nothing supplies");
    assert!(
        matches!(
            &error,
            jals_build::WasmRunError::UnresolvedImport { module, name, .. }
                if module == "demo/Native" && name == "twice(I)I"
        ),
        "the refusal names both halves: {error}"
    );
}

/// A foreign module that imports from one linked before it.
///
/// The shape the WIT case takes when one component's interfaces call another's: the generated
/// core modules are separate, and the host — this runner — is the linker between them. The import
/// is spelled the ordinary wasm way, `<module>.<export>`, and what makes the provider resolvable
/// under `core-one` is that every foreign module is linked under the name it was declared by.
#[test]
fn a_foreign_module_can_import_from_an_earlier_one() {
    // The provider: `double(I)I`, linked under its dependency's name.
    let mut provider = Module::new();
    let ty = provider.add_type(SubType::plain(CompType::Func {
        params: vec![ValType::I32],
        results: vec![ValType::I32],
    }));
    let mut body = Insn::new();
    body.local_get(0)
        .i32_const(2)
        .numeric(NumOp::Mul, ValType::I32)
        .expect("i32.mul");
    let function = provider.func_index(0);
    provider.funcs.push(Func {
        type_index: ty,
        locals: Vec::new(),
        body: body.into_body(),
    });
    provider
        .exports
        .push(("double".to_owned(), ExportKind::Func, function));
    let provider_bytes = provider.finish().expect("the provider encodes");

    // The consumer: it exports the canonical key the project imports, computed by calling the
    // provider. The import is function index 0 — every import precedes every defined function.
    let mut consumer = Module::new();
    let ty = consumer.add_type(SubType::plain(CompType::Func {
        params: vec![ValType::I32],
        results: vec![ValType::I32],
    }));
    consumer.imports.push(Import {
        module: "core-one".to_owned(),
        name: "double".to_owned(),
        kind: ImportKind::Function {
            params: vec![ValType::I32],
            results: vec![ValType::I32],
        },
    });
    let mut body = Insn::new();
    body.local_get(0).call(0);
    let function = consumer.func_index(0);
    consumer.funcs.push(Func {
        type_index: ty,
        locals: Vec::new(),
        body: body.into_body(),
    });
    consumer.exports.push((
        "demo/Native#twice(I)I".to_owned(),
        ExportKind::Func,
        function,
    ));
    let consumer_bytes = consumer.finish().expect("the consumer encodes");

    let project_bytes = compile(&[PROJECT, NATIVE]);
    let outcome = jals_build::WasmRunner::run(&jals_build::WasmRunRequest {
        module: &project_bytes,
        invoke: Some("run"),
        args: &[],
        natives: &jals_native::NativeBindings::new(),
        libraries: &[],
        foreign: &[
            jals_build::WasmForeignModule {
                name: "core-one",
                bytes: &provider_bytes,
            },
            jals_build::WasmForeignModule {
                name: "core-two",
                bytes: &consumer_bytes,
            },
        ],
        progress: &jals_progress::Progress::SILENT,
    })
    .expect("the run links and executes");
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(42)])
    );
}

/// A link that cannot complete is refused before any module code runs.
///
/// The library here traps the moment its start function runs. Reporting the unresolved import
/// after starting it would make a diagnostic about the *link* conditional on library code whose
/// effects are host-visible running first, and the trap would replace the message. The missing
/// import is what the run has to report, and the trap must never be reached.
#[test]
fn an_unresolved_import_is_reported_before_a_library_starts() {
    let mut library = Module::new();
    let ty = library.add_type(SubType::plain(CompType::Func {
        params: Vec::new(),
        results: Vec::new(),
    }));
    let mut body = Insn::new();
    body.unreachable();
    let function = library.func_index(0);
    library.funcs.push(Func {
        type_index: ty,
        locals: Vec::new(),
        body: body.into_body(),
    });
    library.start = Some(function);
    let library_bytes = library.finish().expect("the library encodes");

    let project_bytes = compile(&[PROJECT, NATIVE]);
    let error = jals_build::WasmRunner::run(&jals_build::WasmRunRequest {
        module: &project_bytes,
        invoke: Some("run"),
        args: &[],
        natives: &jals_native::NativeBindings::new(),
        libraries: &[jals_build::WasmLibrary {
            name: "lib",
            bytes: &library_bytes,
        }],
        foreign: &[],
        progress: &jals_progress::Progress::SILENT,
    })
    .expect_err("the import nothing supplies");
    assert!(
        matches!(
            &error,
            jals_build::WasmRunError::UnresolvedImport { module, name, .. }
                if module == "demo/Native" && name == "twice(I)I"
        ),
        "the link is what is reported, not the trap: {error}"
    );
}
