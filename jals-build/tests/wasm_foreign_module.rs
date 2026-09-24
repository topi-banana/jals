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
    CompType, CompileWasm, ExportKind, Func, Insn, Module, NumOp, SubType, ValType, WasmOptions,
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
    let texts = [PROJECT, NATIVE];
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
    let project_bytes = CompileWasm::project(&typed, &[], &index, WasmOptions::default())
        .expect("the project compiles");
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
    let texts = [PROJECT, NATIVE];
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
    let project_bytes = CompileWasm::project(&typed, &[], &index, WasmOptions::default())
        .expect("the project compiles");

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
