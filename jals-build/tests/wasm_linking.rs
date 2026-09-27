//! Linking a project module against a precompiled library module.
//!
//! The engine's side of the library ABI, exercised without a Java source: two modules built by
//! [`jals_javac::wasm::Module`], linked by [`tinywasm::Imports::link_module`] in one `Store`. What
//! this pins is the one property the whole arrangement rests on and that nothing else reports:
//! canonicalisation is **per recursive group**, so two modules meet at the same types only when
//! they declare the *same* group. The positive case shares a group and passes references, globals,
//! and an exception across the boundary; the negative case declares the same types inside a group
//! with one extra member and the link is refused.
//!
//! # Why the fixture is hand-built
//!
//! `CompileWasm` does not emit a library-import mode yet — it compiles library inputs into the
//! project's own module. This test builds the ABI's *shape* directly, which is what makes it a
//! statement about the engine and the encoder rather than about the lowering: when the lowering
//! learns the mode, the shape asserted here is the contract it has to hit.

#![cfg(feature = "wasm-run")]

use jals_javac::wasm::{
    CompType, ExportKind, FieldType, Func, Global, HeapType, Insn, Module, RefType, StorageType,
    SubType, ValType,
};
use tinywasm::{Imports, ModuleInstance, Store, WasmValue, parse_bytes};

/// The type indices both modules declare identically, in the group that must match.
struct Shared {
    point: u32,
    payload: u32,
    make: u32,
    get: u32,
    raise: u32,
}

/// Declare the library's types, then close the group.
///
/// Every type a library and its consumer share lives here, and the boundary after them is the
/// point: the consumer's own types go in a later group, where they may reference the library's and
/// not the reverse.
fn shared_types(module: &mut Module) -> Shared {
    let point = module.add_type(SubType::plain(CompType::Struct(vec![FieldType {
        storage: StorageType::Val(ValType::I32),
        mutable: true,
    }])));
    let payload = module.add_type(SubType::plain(CompType::Func {
        params: vec![ValType::I32],
        results: Vec::new(),
    }));
    let reference = ValType::Ref(RefType::nullable(HeapType::Concrete(point)));
    let make = module.add_type(SubType::plain(CompType::Func {
        params: vec![ValType::I32],
        results: vec![reference],
    }));
    let get = module.add_type(SubType::plain(CompType::Func {
        params: vec![reference],
        results: vec![ValType::I32],
    }));
    let raise = module.add_type(SubType::plain(CompType::Func {
        params: vec![ValType::I32],
        results: Vec::new(),
    }));
    module.begin_group();
    Shared {
        point,
        payload,
        make,
        get,
        raise,
    }
}

/// One instruction stream holding a single constant, for a global's initialiser.
fn constant(value: i32) -> Vec<jals_javac::wasm::Instr> {
    let mut insn = Insn::new();
    insn.i32_const(value);
    insn.into_body()
}

/// The library: allocates a `Point`, reads it, throws a tagged exception, and owns a global.
fn library_module() -> Module {
    let mut module = Module::new();
    let shared = shared_types(&mut module);
    let point_ref = ValType::Ref(RefType::nullable(HeapType::Concrete(shared.point)));

    let mut make = Insn::new();
    make.struct_new_default(shared.point)
        .local_tee(1)
        .local_get(0)
        .struct_set(shared.point, 0)
        .local_get(1);
    module.funcs.push(Func {
        type_index: shared.make,
        locals: vec![point_ref],
        body: make.into_body(),
    });

    let mut get = Insn::new();
    get.local_get(0).struct_get(shared.point, 0);
    module.funcs.push(Func {
        type_index: shared.get,
        locals: Vec::new(),
        body: get.into_body(),
    });

    module.tags.push(shared.payload);
    let mut raise = Insn::new();
    raise.local_get(0).throw(module.tag_index(0));
    module.funcs.push(Func {
        type_index: shared.raise,
        locals: Vec::new(),
        body: raise.into_body(),
    });

    module.globals.push(Global {
        ty: ValType::I32,
        init: constant(41),
    });

    module
        .exports
        .push(("make".to_owned(), ExportKind::Func, module.func_index(0)));
    module
        .exports
        .push(("get".to_owned(), ExportKind::Func, module.func_index(1)));
    module
        .exports
        .push(("raise".to_owned(), ExportKind::Func, module.func_index(2)));
    module.exports.push((
        "counter".to_owned(),
        ExportKind::Global,
        module.global_index(0),
    ));
    module
        .exports
        .push(("boom".to_owned(), ExportKind::Tag, module.tag_index(0)));
    module
}

/// The project: imports the library's three functions, its global, and its tag, and allocates a
/// `Point` of its own that the library reads.
fn project_module() -> Module {
    let mut module = Module::new();
    let shared = shared_types(&mut module);

    let make = module.add_shared_import("library".to_owned(), "make".to_owned(), shared.make);
    let get = module.add_shared_import("library".to_owned(), "get".to_owned(), shared.get);
    let raise = module.add_shared_import("library".to_owned(), "raise".to_owned(), shared.raise);
    module.add_global_import(
        "library".to_owned(),
        "counter".to_owned(),
        ValType::I32,
        true,
    );
    let boom = module.add_tag_import("library".to_owned(), "boom".to_owned(), shared.payload);

    // The project's own types, after the shared group's boundary.
    let gives_i32 = |module: &mut Module| {
        module.add_type(SubType::plain(CompType::Func {
            params: Vec::new(),
            results: vec![ValType::I32],
        }))
    };
    let use_ty = module.add_type(SubType::plain(CompType::Func {
        params: vec![ValType::I32],
        results: vec![ValType::I32],
    }));
    let own_ty = gives_i32(&mut module);
    let counter_ty = gives_i32(&mut module);
    let caught_ty = gives_i32(&mut module);

    let point_ref = ValType::Ref(RefType::nullable(HeapType::Concrete(shared.point)));

    let mut use_ = Insn::new();
    use_.local_get(0).call(make).call(get);
    module.funcs.push(Func {
        type_index: use_ty,
        locals: Vec::new(),
        body: use_.into_body(),
    });

    let mut own = Insn::new();
    own.struct_new_default(shared.point)
        .local_tee(0)
        .i32_const(7)
        .struct_set(shared.point, 0)
        .local_get(0)
        .call(get);
    module.funcs.push(Func {
        type_index: own_ty,
        locals: vec![point_ref],
        body: own.into_body(),
    });

    let mut counter = Insn::new();
    counter.global_get(0);
    module.funcs.push(Func {
        type_index: counter_ty,
        locals: Vec::new(),
        body: counter.into_body(),
    });

    // A catch of the library's tag, with the payload the typed block receives. `raise` always
    // throws, so the try body's fallthrough is unreachable and the block's value is either the
    // payload or the `-1` the normal path would leave.
    let mut caught = Insn::new();
    caught.block_typed(ValType::I32);
    caught.try_table(&[(boom, 0)]);
    caught.i32_const(99).call(raise);
    caught.end();
    caught.i32_const(-1);
    caught.end();
    module.funcs.push(Func {
        type_index: caught_ty,
        locals: Vec::new(),
        body: caught.into_body(),
    });

    module
        .exports
        .push(("use".to_owned(), ExportKind::Func, module.func_index(0)));
    module
        .exports
        .push(("own".to_owned(), ExportKind::Func, module.func_index(1)));
    module.exports.push((
        "read_counter".to_owned(),
        ExportKind::Func,
        module.func_index(2),
    ));
    module
        .exports
        .push(("caught".to_owned(), ExportKind::Func, module.func_index(3)));
    module
}

/// Instantiate `library` first, then link `project` against it.
fn link(library: &[u8], project: &[u8]) -> Result<ModuleInstance, String> {
    let library = parse_bytes(library).map_err(|error| error.to_string())?;
    let project = parse_bytes(project).map_err(|error| error.to_string())?;
    let mut store = Store::default();
    let instance = ModuleInstance::instantiate(&mut store, &library, None)
        .map_err(|error| error.to_string())?;
    let mut imports = Imports::new();
    imports
        .link_module("library", instance)
        .map_err(|error| error.to_string())?;
    ModuleInstance::instantiate(&mut store, &project, Some(&imports))
        .map_err(|error| error.to_string())
}

/// Call `name` with `args` and hand back its results.
fn call(
    instance: &ModuleInstance,
    store: &mut Store,
    name: &str,
    args: &[WasmValue],
) -> Vec<WasmValue> {
    let func = instance
        .func_untyped(store, name)
        .unwrap_or_else(|error| panic!("export `{name}`: {error}"));
    let ty = func.ty(store).expect("the export's type");
    let mut results = vec![WasmValue::I32(0); ty.results().len()];
    func.call(store, args, &mut results)
        .unwrap_or_else(|error| panic!("call `{name}`: {error}"));
    results
}

/// The one result an `i32`-returning export leaves, as the number it is.
fn i32_of(results: &[WasmValue]) -> i32 {
    match results {
        [WasmValue::I32(value)] => *value,
        other => panic!("expected one i32, got {other:?}"),
    }
}

/// A reference allocated in the library crosses into the project, and one allocated in the project
/// crosses back — which is the whole reason the two modules must share a recursive group.
#[test]
fn a_project_module_links_against_a_library_module() {
    let library = library_module().finish().expect("the library encodes");
    let project = project_module().finish().expect("the project encodes");
    let library = parse_bytes(&library).expect("the library parses");
    let project = parse_bytes(&project).expect("the project parses");

    let mut store = Store::default();
    let library_instance =
        ModuleInstance::instantiate(&mut store, &library, None).expect("the library instantiates");
    let mut imports = Imports::new();
    imports
        .link_module("library", library_instance)
        .expect("the library registers");
    let project_instance = ModuleInstance::instantiate(&mut store, &project, Some(&imports))
        .expect("the project links");

    // The library allocated the `Point` and the project read it back through the library.
    assert_eq!(
        i32_of(&call(
            &project_instance,
            &mut store,
            "use",
            &[WasmValue::I32(5)]
        )),
        5
    );
    // The project allocated the `Point` and the library read it: the reverse direction.
    assert_eq!(i32_of(&call(&project_instance, &mut store, "own", &[])), 7);
    // The library's global.
    assert_eq!(
        i32_of(&call(&project_instance, &mut store, "read_counter", &[])),
        41
    );
    // The library threw its tag and the project caught it.
    assert_eq!(
        i32_of(&call(&project_instance, &mut store, "caught", &[])),
        99
    );
}

/// The negative half: the *same* types declared in a group with one extra member do not
/// canonicalise to the library's, and the link is refused with the import named.
#[test]
fn a_group_that_differs_by_one_type_does_not_link() {
    let library = library_module().finish().expect("the library encodes");
    // Rebuild the project so that the shared declarations sit in the same group as an extra type:
    // no `begin_group` between them.
    let mut module = Module::new();
    let point = module.add_type(SubType::plain(CompType::Struct(vec![FieldType {
        storage: StorageType::Val(ValType::I32),
        mutable: true,
    }])));
    let reference = ValType::Ref(RefType::nullable(HeapType::Concrete(point)));
    let make = module.add_type(SubType::plain(CompType::Func {
        params: vec![ValType::I32],
        results: vec![reference],
    }));
    // The extra member that changes the group's canonical identity.
    module.add_type(SubType::plain(CompType::Struct(Vec::new())));
    module.add_shared_import("library".to_owned(), "make".to_owned(), make);
    let project = module.finish().expect("the project encodes");

    let Err(error) = link(&library, &project) else {
        panic!("the groups differ and the link must fail");
    };
    assert!(
        error.contains("incompatible import type"),
        "the refusal names the import: {error}"
    );
}
