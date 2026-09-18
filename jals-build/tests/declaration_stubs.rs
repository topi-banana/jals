//! A package that ships **no Java**: its API is declarations, and the wasm target still runs it.
//!
//! The compiler has no second path for a declaration — `jals-build` renders the model back into the
//! smallest Java that produces the class's struct and one host import per `native` method, and the
//! existing front end lowers that stub. So the assertion here is the whole B arrangement: a
//! declared class, a Rust binding, a project that calls it, and a module that runs.

#![cfg(feature = "wasm-run")]

use jals_build::{
    Assertions, BackendOptions, BackendRequest, BackendSelection, BackendSource, WasmRunOutcome,
    WasmRunRequest, WasmRunner,
};
use jals_config::BackendKind;
use jals_native::{
    Args, DeclaredType, JavaPackage, Member, MemberKind, Modifiers, NativeValue, Param, Results,
    TypeKind, TypeRef,
};
use jals_progress::Progress;
use jals_storage::{CacheKey, CacheNamespace, ContentDigest, RelativePath};

/// One project source as the backend receives it.
fn source(path: &str, text: &str) -> BackendSource {
    let bytes = text.as_bytes().to_vec();
    BackendSource {
        path: RelativePath::parse(path).expect("a valid path"),
        key: CacheKey::new(
            CacheNamespace::FrontendOutput,
            ContentDigest::of(b"declaration-stubs"),
            ContentDigest::of(&bytes),
        ),
        bytes,
    }
}

/// A package whose API is one declared class and whose implementation is one Rust binding.
///
/// No `implementation` unit anywhere: `answer` is `native`, so the declaration is the whole
/// publication and the stub generated from it is the whole Java the compiler sees.
fn declared_package() -> JavaPackage {
    let mut package = JavaPackage::new("demo", 1);
    package.declare(DeclaredType {
        fqn: "demo.Host".into(),
        package: "demo".into(),
        kind: TypeKind::Class,
        type_params: Vec::new(),
        supertypes: Vec::new(),
        members: vec![Member {
            name: "answer".into(),
            kind: MemberKind::Method,
            ty: TypeRef::Primitive {
                keyword: "int".into(),
                dims: 0,
            },
            params: vec![Param {
                name: Some("seed".into()),
                ty: TypeRef::Primitive {
                    keyword: "int".into(),
                    dims: 0,
                },
                annotations: Vec::new(),
            }],
            varargs: false,
            type_params: Vec::new(),
            throws: Vec::new(),
            annotations: Vec::new(),
            modifiers: Modifiers {
                is_static: true,
                is_public: true,
                is_native: true,
                ..Modifiers::default()
            },
        }],
        annotations: Vec::new(),
    });
    package.bind(
        "demo/Host",
        "answer(I)I",
        |_host, args: Args<'_>, mut results: Results<'_>| {
            results.set(0, NativeValue::I32(args.i32(0)?.saturating_add(1)));
            Ok(())
        },
    );
    package
}

#[test]
fn a_declared_package_compiles_and_runs_with_no_java_anywhere() {
    let packages = jals_native::PackageSelection::of([declared_package()]);
    let tree = [source(
        "Main.java",
        "import demo.Host;\n\
         public class Main { public static int run() { return Host.answer(41); } }\n",
    )];
    let options = BackendOptions::default();
    let request = BackendRequest {
        progress: &Progress::SILENT,
        tree: &tree,
        classpath: &[],
        options: &options,
    };
    let BackendSelection::Available(backend) = BackendSelection::in_process(
        BackendKind::JalsWasm {},
        None,
        Assertions::Disabled,
        packages.clone(),
    ) else {
        panic!("the in-process wasm backend is always available");
    };
    let outcome = jals_exec::block_on_inline(backend.compile(&request)).expect("the backend ran");
    assert!(
        outcome.success(),
        "the declared package compiles: {:?}",
        outcome.messages
    );
    let (path, module) = outcome.artifacts.into_iter().next().expect("one module");
    assert_eq!(path.to_string(), jals_build::JalsBackend::WASM_MODULE);

    let outcome = WasmRunner::run(&WasmRunRequest {
        module: &module,
        invoke: Some("run"),
        args: &[],
        natives: &packages.bindings(),
        progress: &Progress::SILENT,
    })
    .expect("the module links and runs");
    assert!(
        matches!(outcome, WasmRunOutcome::Returned(ref values) if values == &[jals_build::WasmValue::I32(42)]),
        "{outcome:?}"
    );
}
