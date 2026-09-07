//! The `java.base` native package, driven end to end.
//!
//! Here rather than in `jals-native` because this crate is the one that holds both halves of the
//! claim: `jals-native` publishes the package but has no compiler and no engine (it is
//! dependency-free by design), and `jals-javac` has the compiler but does not know packages exist.
//! This crate selects a package, compiles its Java into a module beside a project's, and links its
//! bindings when that module is instantiated — so it is where "the package compiles" and "the
//! package runs" can both be asserted.
//!
//! Three claims live here, and each is one a review would otherwise have to take on trust.
//!
//! 1. **It compiles.** Every one of the package's Java files is lowered by the wasm backend, not
//!    just the ones a fixture happens to call: a library input has every body lowered, so one
//!    method reaching for a construct this target does not have fails the whole compile.
//! 2. **It supersedes what it shadows.** A native package's Java outranks `jals-hir`'s stubs per
//!    fully-qualified name, so a type this package declares *replaces* the stub for every project
//!    that selects it. A member the stub had and this does not is a member that disappears from
//!    analysis — a regression with no diagnostic — which is why the two member sets are diffed
//!    rather than eyeballed.
//! 3. **It runs.** The renderings, the parses and the arithmetic are asserted through a module the
//!    engine actually executed, because a compile that succeeds says nothing about what the code
//!    answers.

use jals_hir::{FileAnalysis, FileId, FileSemantics, ItemOrigin, ProjectIndex, Ty, TypedFile};
use jals_javac::wasm::{CompileWasm, WasmOptions};
use jals_native::packages::java_base::{CapturedSystem, JavaBase, SystemHost};
use jals_native::{NativePackageSet, NativeRegistry};
use jals_syntax::SyntaxNode;
use std::rc::Rc;

/// The package, over a host that keeps what was written.
fn selection() -> (NativePackageSet, Rc<CapturedSystem>) {
    let host = Rc::new(CapturedSystem::at(1_700_000_000_000));
    let mut registry = NativeRegistry::new();
    registry.add(JavaBase::package(Rc::clone(&host) as Rc<dyn SystemHost>));
    let selected = registry
        .select(&[JavaBase::NAME.to_owned()])
        .expect("the package was just registered");
    (selected, host)
}

/// Compile `sources` as the project and the package's Java as the library beside it.
fn module_of(sources: &[&str], packages: &NativePackageSet) -> Vec<u8> {
    compile(sources, packages)
        .unwrap_or_else(|error| panic!("the wasm backend refused the package: {error}"))
}

/// [`module_of`], handing back the backend's refusal instead of panicking on one.
///
/// One pipeline rather than two, because the tests that expect a refusal have to be running the
/// same compile as the ones that expect a module — a second copy is where the two would start
/// answering about different inputs.
fn compile(sources: &[&str], packages: &NativePackageSet) -> Result<Vec<u8>, String> {
    let library: Vec<&str> = packages.sources().map(|(_, source)| source.text).collect();
    let texts: Vec<&str> = sources.iter().copied().chain(library).collect();
    let roots: Vec<(FileId, SyntaxNode)> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let file = FileId(u32::try_from(index).expect("a small fixture"));
            let parsed = jals_exec::block_on_inline(jals_syntax::Parse::parse(text));
            (file, parsed.syntax())
        })
        .collect();
    let (project_roots, native_roots) = roots.split_at(sources.len());
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(project_roots)
            .with_native_packages(native_roots)
            .with_stdlib()
            .build(),
    );
    let analyses: Vec<FileAnalysis> = roots
        .iter()
        .map(|(_, root)| jals_exec::block_on_inline(FileAnalysis::of(root)))
        .collect();
    // The bindings own the inference memo the witnesses borrow, so both live to the end.
    let semantics: Vec<FileSemantics<'_>> = roots
        .iter()
        .zip(&analyses)
        .map(|((file, _), analysis)| analysis.in_project(&index, *file))
        .collect();
    let typed: Vec<TypedFile<'_>> = semantics
        .iter()
        .map(|binding| jals_exec::block_on_inline(binding.typed()))
        .collect();
    let (project, library) = typed.split_at(sources.len());
    CompileWasm::project(project, library, &index, WasmOptions::default())
        .map_err(|error| error.to_string())
}

/// A member's identity for the purpose of "did this one survive": its name and its erased
/// parameter types.
///
/// Erased, because the package is generic where the stub is raw — `Comparable<T>.compareTo(T)`
/// against `Comparable.compareTo(Object)` is the same method, and comparing the spellings would
/// report it as a loss. Everything else is compared as written: a stub `abs(int)` answered only by
/// an `abs(long)` really is a member that disappeared.
fn signature(index: &ProjectIndex, member: jals_hir::MemberId) -> String {
    let rendered: Vec<String> = index
        .resolved_param_tys(member)
        .iter()
        .map(|ty| erased(index, ty))
        .collect();
    format!("{}({})", index.member(member).name, rendered.join(", "))
}

/// `ty` with type variables erased and class names taken to their last segment.
///
/// The last segment because one index resolves `String` against a stub and the other against this
/// package, and the two render the same type under different spellings; the segment is what both
/// agree on.
fn erased(index: &ProjectIndex, ty: &Ty) -> String {
    match ty {
        Ty::TypeVar { .. } => index
            .type_var_erasure(ty)
            .map_or_else(|| String::from("Object"), |bound| erased(index, &bound)),
        Ty::Array(element) => format!("{}[]", erased(index, element)),
        Ty::Class(_) => ty
            .to_string()
            .split('<')
            .next()
            .unwrap_or_default()
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_owned(),
        other => other.to_string(),
    }
}

/// An index over the package's Java, with the stubs behind it exactly as a project's would be.
fn indexed(packages: &NativePackageSet) -> (Vec<(FileId, SyntaxNode)>, ProjectIndex) {
    let roots: Vec<(FileId, SyntaxNode)> = packages
        .sources()
        .enumerate()
        .map(|(index, (_, source))| {
            let file = FileId(u32::try_from(index).expect("a small package"));
            let parsed = jals_exec::block_on_inline(jals_syntax::Parse::parse(source.text));
            (file, parsed.syntax())
        })
        .collect();
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&[])
            .with_native_packages(&roots)
            .with_stdlib()
            .build(),
    );
    (roots, index)
}

/// Every body in the package is lowered, which is what a library input means.
#[test]
fn the_package_compiles_into_a_module() {
    let (packages, _host) = selection();
    let module = module_of(
        &["public final class Main { public static int run() { return 0; } }"],
        &packages,
    );
    assert!(
        module.starts_with(b"\0asm"),
        "the backend produced something that is not a module"
    );
}

/// A type the package declares outranks the stub of the same name, so it must not declare less.
#[test]
fn the_package_supersedes_every_stub_member_it_shadows() {
    let (packages, _host) = selection();
    let (_roots, index) = indexed(&packages);
    let stubs = jals_exec::block_on_inline(ProjectIndex::builder(&[]).with_stdlib().build());

    let mut lost: Vec<String> = Vec::new();
    let mut shadowed = 0_usize;
    for (stub_id, stub_item) in stubs.items() {
        let fqn = stub_item.fqn.as_str();
        let Some(id) = index.item_by_fqn(fqn) else {
            continue;
        };
        if index.item(id).origin != ItemOrigin::Native {
            continue;
        }
        shadowed += 1;
        let declared: Vec<String> = index
            .own_members(id)
            .iter()
            .map(|&member| signature(&index, member))
            .collect();
        for &member in stubs.own_members(stub_id) {
            let wanted = signature(&stubs, member);
            if !declared.contains(&wanted) {
                lost.push(format!("{fqn}.{wanted}"));
            }
        }
    }

    assert!(
        shadowed >= 40,
        "the package shadowed only {shadowed} stub types, which is fewer than it declares"
    );
    assert!(
        lost.is_empty(),
        "these stub members disappear for a project that selects `java.base`:\n  {}",
        lost.join("\n  ")
    );
}

/// The gaps this package cannot close, pinned so closing one is a deliberate edit here.
///
/// Every entry is a *backend* refusal rather than a missing declaration: the type is present and
/// the lowering is what is absent. They are asserted as failures so that the day the wasm backend
/// grows a string-literal or boxing conversion, this test fails and says which line to delete.
#[test]
fn the_gaps_are_the_backends_and_not_the_packages() {
    let (packages, _host) = selection();
    for (source, refusal) in [
        (
            "public final class Main { public static int run() { String s = \"x\"; return s.length(); } }",
            "this literal kind",
        ),
        (
            "public final class Main { public static int run() { Integer n = 1; return n.intValue(); } }",
            "java.lang.Integer",
        ),
    ] {
        let error = compile(&[source], &packages)
            .expect_err("this is a gap, and it is expected to still be one");
        assert!(
            error.contains(refusal),
            "expected a refusal naming `{refusal}`, got `{error}`"
        );
    }
}

#[cfg(feature = "wasm-run")]
mod running {
    use super::{module_of, selection};
    use jals_build::{WasmRunOutcome, WasmRunRequest, WasmRunner, WasmValue};
    use jals_native::NativeBindings;
    use jals_progress::Progress;

    /// Call `export` in a module compiled from `source` beside the package.
    fn returns(source: &str, export: &str) -> (WasmValue, String) {
        let (packages, host) = selection();
        let module = module_of(&[source], &packages);
        let outcome = run(&module, export, &packages.bindings());
        let WasmRunOutcome::Returned(values) = outcome else {
            panic!("`{export}` returned nothing");
        };
        let value = *values.first().expect("one result");
        (value, host.take_out())
    }

    /// Call a `void` export and hand back what the host was written.
    fn prints(source: &str, export: &str) -> (String, String) {
        let (packages, host) = selection();
        let module = module_of(&[source], &packages);
        run(&module, export, &packages.bindings());
        (host.take_out(), host.take_err())
    }

    fn run(module: &[u8], export: &str, natives: &NativeBindings) -> WasmRunOutcome {
        WasmRunner::run(&WasmRunRequest {
            module,
            invoke: Some(export),
            args: &[],
            natives,
            progress: &Progress::SILENT,
        })
        .unwrap_or_else(|error| panic!("running `{export}`: {error}"))
    }

    /// The module imports ten host functions and the package binds exactly those ten.
    ///
    /// Asserted by instantiating: an engine refuses a module whose imports are unmet, so a run
    /// that reached the export at all is the link having succeeded.
    #[test]
    fn every_import_the_module_declares_is_bound() {
        let (value, _) = returns(
            "public final class Main { public static long run() { return System.currentTimeMillis(); } }",
            "run",
        );
        assert_eq!(value, WasmValue::I64(1_700_000_000_000));
    }

    /// `System.out.println` reaches the host's sink, and `System.err` reaches the other one.
    #[test]
    fn a_program_prints_through_the_streams_the_host_supplied() {
        let (out, err) = prints(
            "public final class Main {\n\
             \x20   public static void run() {\n\
             \x20       System.out.println(Integer.toString(-2147483648));\n\
             \x20       System.out.println(Long.toHexString(255L));\n\
             \x20       System.out.println(Double.toString(0.0001));\n\
             \x20       System.out.println(Boolean.toString(true));\n\
             \x20       System.err.println(Integer.toString(7));\n\
             \x20   }\n\
             }",
            "run",
        );
        assert_eq!(out, "-2147483648\nff\n1.0E-4\ntrue\n");
        assert_eq!(err, "7\n");
    }

    /// `String` is a real object: built, compared, sliced, hashed.
    #[test]
    fn strings_behave() {
        let (value, _) = returns(
            "public final class Main {\n\
             \x20   public static int run() {\n\
             \x20       char[] units = {'h', 'e', 'l', 'l', 'o'};\n\
             \x20       String text = new String(units);\n\
             \x20       StringBuilder builder = new StringBuilder();\n\
             \x20       builder.append(text).append(' ').append(42).append(true);\n\
             \x20       String joined = builder.toString();\n\
             \x20       if (!joined.startsWith(text)) { return -1; }\n\
             \x20       if (joined.indexOf('4') != 6) { return -2; }\n\
             \x20       if (!text.equals(text.substring(0, 5))) { return -3; }\n\
             \x20       if (text.hashCode() != 99162322) { return -4; }\n\
             \x20       return joined.length();\n\
             \x20   }\n\
             }",
            "run",
        );
        assert_eq!(
            value,
            WasmValue::I32("hello 42true".len().try_into().unwrap())
        );
    }

    /// A `throw` from inside the package is caught by the project, and carries its message.
    #[test]
    fn an_exception_the_package_raises_reaches_the_projects_catch() {
        let (out, _) = prints(
            "public final class Main {\n\
             \x20   public static void run() {\n\
             \x20       char[] units = {'1', '2', 'x'};\n\
             \x20       try {\n\
             \x20           Integer.parseInt(new String(units));\n\
             \x20           System.out.println(Boolean.toString(false));\n\
             \x20       } catch (NumberFormatException failure) {\n\
             \x20           System.out.println(failure.toString());\n\
             \x20       }\n\
             \x20   }\n\
             }",
            "run",
        );
        assert_eq!(out, "java.lang.NumberFormatException: 12x\n");
    }

    /// The floating-point half: the host renders and parses, and Java's `Math` computes.
    #[test]
    fn the_floating_point_seam_round_trips() {
        let (out, _) = prints(
            "public final class Main {\n\
             \x20   public static void run() {\n\
             \x20       System.out.println(Double.toString(Math.sqrt(2.0)));\n\
             \x20       System.out.println(Long.toString(Double.doubleToLongBits(1.0)));\n\
             \x20       System.out.println(Double.toString(Double.parseDouble(Double.toString(0.1))));\n\
             \x20       System.out.println(Float.toString(0.1f));\n\
             \x20       System.out.println(Integer.toString(Math.floorMod(-7, 3)));\n\
             \x20       System.out.println(Float.toString(Math.max(-0.0f, 0.0f)));\n\
             \x20       System.out.println(Float.toString(Math.min(-0.0f, 0.0f)));\n\
             \x20   }\n\
             }",
            "run",
        );
        // The last two lines are the one thing widening could have broken: `Math.max(float, float)`
        // delegates to the `double` overload, which tells `0.0` from `-0.0` by reading the sign
        // bit. A widening conversion preserves it, and these say so rather than assuming it.
        assert_eq!(
            out,
            "1.4142135623730951\n4607182418800017408\n0.1\n0.1\n2\n0.0\n-0.0\n"
        );
    }
}
