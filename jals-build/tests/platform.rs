//! The platform package, compiled and run.
//!
//! Here rather than in `jals-platform` because this is the one crate holding both halves:
//! `jals-platform` has no compiler and no engine, and `jals-javac` does not know packages exist.
//!
//! # What this replaces
//!
//! A test that built two indexes — one with the package, one with the hand-written stubs it
//! shadowed — and diffed their member sets per fully-qualified name, to catch a member the package
//! forgot to declare. That invariant no longer exists to check: there is one text now, so there is
//! no second member set to lose one against.
//!
//! What is left is the claim that text actually carries its weight: it compiles, the tiers keep
//! `java.lang.Object` out of the module, and a program written against it runs.

use jals_build::{Assertions, BackendOptions, BackendRequest, BackendSelection, BackendSource};
use jals_config::BackendKind;
use jals_native::{PackageSelection, ResolverChain, SourceKind, StaticResolver};
use jals_platform::{CapturedHost, JavaBase};
use jals_progress::Progress;
use jals_storage::{CacheKey, CacheNamespace, ContentDigest, RelativePath};
use std::rc::Rc;

/// The platform over a host whose clock reads a fixed instant, so a test can assert an exact number.
pub(crate) fn platform(host: Rc<CapturedHost>) -> PackageSelection {
    let mut resolver = StaticResolver::new("test");
    resolver.add(JavaBase::package(host));
    ResolverChain::new()
        .push(Box::new(resolver))
        .select(&[JavaBase::NAME.to_owned()])
        .expect("the platform ships with this build")
}

fn source(path: &str, text: &str) -> BackendSource {
    let bytes = text.as_bytes().to_vec();
    BackendSource {
        path: RelativePath::parse(path).expect("a valid path"),
        key: CacheKey::new(
            CacheNamespace::FrontendOutput,
            ContentDigest::of(b"platform-test"),
            ContentDigest::of(&bytes),
        ),
        bytes,
    }
}

/// Compile `sources` as a wasm project with the platform linked.
///
/// One pipeline, returning a `Result`, because the tests that expect a refusal have to be running
/// the same compile as the ones that expect a module.
pub(crate) fn compile(
    sources: &[(&str, &str)],
    packages: PackageSelection,
) -> Result<Vec<u8>, String> {
    let tree: Vec<BackendSource> = sources
        .iter()
        .map(|(path, text)| source(path, text))
        .collect();
    let options = BackendOptions::default();
    let request = BackendRequest {
        progress: &Progress::SILENT,
        tree: &tree,
        classpath: &[],
        options: &options,
    };
    // Through the selection, not by naming the backend: `BackendSelection` is the one place
    // `[build] backend` becomes an implementation, and a test that reached past it would be
    // exercising a construction no host performs.
    let BackendSelection::Available(backend) = BackendSelection::in_process(
        BackendKind::JalsWasm {},
        None,
        Assertions::Disabled,
        packages,
    ) else {
        panic!("the in-process wasm backend is always available");
    };
    let outcome = jals_exec::block_on_inline(backend.compile(&request))
        .map_err(|error| format!("{error:?}"))?;
    if !outcome.success() {
        return Err(outcome.messages.join("\n"));
    }
    Ok(outcome.artifacts.into_iter().next().expect("one module").1)
}

/// **It compiles.** Every implementation unit is lowered, not only the reachable ones — a library
/// input has every body compiled — so this is the whole 50-odd files going through the backend.
#[test]
fn the_platform_compiles_into_a_module() {
    let host = Rc::new(CapturedHost::new());
    let module = compile(
        &[(
            "Main.java",
            "public final class Main { public static int run() { return 1; } }",
        )],
        platform(host),
    )
    .expect("the platform compiles");
    assert!(
        module.starts_with(b"\0asm"),
        "the backend produced something that is not a module"
    );
}

/// **The tier keeps `java.lang.Object` out of the module**, and every other signature unit with it.
///
/// `Object` *is* the backend's `anyref`: it answers for that name before it consults its struct
/// table, so a declared `Object` would be one question with two answers — a field present on some
/// instances and not others. That used to be a rule stated in prose for a reviewer to enforce.
/// Here it is the type: `link_sources` cannot yield a signature unit, so there is no way to hand
/// the compile that file.
#[test]
fn no_signature_unit_reaches_the_compile() {
    let host = Rc::new(CapturedHost::new());
    let selection = platform(host);

    let linked: Vec<&str> = selection
        .link_sources()
        .map(|(_, s)| s.path.as_ref())
        .collect();
    assert!(
        !linked.contains(&"java/lang/Object.java"),
        "`Object` is the backend's own `anyref`; a compiled one would be a second answer"
    );
    assert!(
        !linked.iter().any(|path| path.starts_with("java/util/")),
        "`java.util` is declared and not implemented, so nothing there can be lowered"
    );
    assert!(
        linked.contains(&"java/lang/String.java"),
        "the check above is vacuous unless something *is* linked"
    );

    // And the index still sees them: a program that names `Object` or `List` resolves.
    let indexed: Vec<&str> = selection
        .analysis_sources()
        .map(|(_, s)| s.path.as_ref())
        .collect();
    assert!(indexed.contains(&"java/lang/Object.java"));
    assert!(indexed.contains(&"java/util/List.java"));
    assert_eq!(
        indexed.len() - linked.len(),
        selection
            .analysis_sources()
            .filter(|(_, s)| matches!(s.kind, SourceKind::Signatures))
            .count(),
        "every unit is in exactly one of the two answers"
    );
}

/// **It runs.** The renderings, the parses, the arithmetic, and the two streams staying apart,
/// asserted through a module the engine actually executed.
///
/// Gated on `wasm-run` because that feature is what links an engine. Every assertion here is about
/// Java the platform ships — `Integer.toString`'s negative accumulation, `Math.sqrt`'s Newton
/// passes, `String`'s hash — reached through a program that only calls it.
#[cfg(feature = "wasm-run")]
mod running {
    use super::{compile, platform};
    use jals_build::{WasmRunOutcome, WasmRunRequest, WasmRunner};
    use jals_platform::CapturedHost;
    use jals_progress::Progress;
    use std::rc::Rc;

    /// Compile `source` against the platform, run `export`, and hand back what the host captured.
    fn run(source: &str, export: &str) -> (String, String) {
        let host = Rc::new(CapturedHost::at(1_700_000_000_000));
        let packages = platform(Rc::clone(&host));
        let bindings = packages.bindings();
        let module = compile(&[("Main.java", source)], packages).expect("the platform compiles");
        let outcome = WasmRunner::run(&WasmRunRequest {
            module: &module,
            invoke: Some(export),
            args: &[],
            natives: &bindings,
            progress: &Progress::SILENT,
        })
        .expect("the module links and runs");
        assert!(matches!(outcome, WasmRunOutcome::Returned(_)));
        (host.take_out(), host.take_err())
    }

    /// Every import the module declares is bound.
    ///
    /// Asserted by the run happening at all: an engine refuses a module whose imports are unmet, so
    /// a missing binding fails at instantiation and never reaches the body.
    #[test]
    fn the_clock_reaches_the_host_the_package_was_built_over() {
        let (out, _) = run(
            "public final class Main {\n\
             public static void run() { System.out.println(System.currentTimeMillis()); }\n\
             }",
            "run",
        );
        assert_eq!(out, "1700000000000\n");
    }

    /// The two streams stay apart all the way to the host, which is why `Stream` is an enum.
    #[test]
    fn a_program_prints_through_the_streams_the_host_supplied() {
        let (out, err) = run(
            "public final class Main {\n\
             public static void run() {\n\
             System.out.println(Integer.MIN_VALUE);\n\
             System.out.println(true);\n\
             System.err.println(7);\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "-2147483648\ntrue\n");
        assert_eq!(err, "7\n");
    }

    /// `String` is a `char[]`, and everything on it is the package's own Java.
    #[test]
    fn strings_behave() {
        let (out, _) = run(
            "public final class Main {\n\
             private static final char[] TEXT = {'h', 'e', 'l', 'l', 'o'};\n\
             public static void run() {\n\
             String s = new String(TEXT);\n\
             System.out.println(s.length());\n\
             System.out.println(s.hashCode());\n\
             System.out.println(s.substring(1, 3));\n\
             System.out.println(s.indexOf('l'));\n\
             }\n\
             }",
            "run",
        );
        // `"hello".hashCode()` is 99162322 on any JVM; the loop that produces it is Java here.
        assert_eq!(out, "5\n99162322\nel\n2\n");
    }

    /// The floating-point seam: a rendering the host produced, and a parse it read back.
    #[test]
    fn the_floating_point_seam_round_trips() {
        let (out, _) = run(
            "public final class Main {\n\
             private static final char[] TEXT = {'0', '.', '1'};\n\
             public static void run() {\n\
             System.out.println(Math.sqrt(2.0));\n\
             System.out.println(Double.doubleToRawLongBits(1.0));\n\
             System.out.println(Double.parseDouble(new String(TEXT)));\n\
             System.out.println(Math.max(0.0, -0.0));\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "1.4142135623730951\n4607182418800017408\n0.1\n0.0\n");
    }

    /// An exception the package raises reaches the project's own `catch`.
    ///
    /// The whole hierarchy is the package's Java — `NumberFormatException` through
    /// `IllegalArgumentException` through `RuntimeException` — so this is a throw, a stack unwind
    /// and a type test all inside the module.
    #[test]
    fn an_exception_the_package_raises_reaches_the_projects_catch() {
        let (out, _) = run(
            "public final class Main {\n\
             private static final char[] BAD = {'1', '2', 'x'};\n\
             public static void run() {\n\
             try {\n\
             Integer.parseInt(new String(BAD));\n\
             } catch (NumberFormatException failure) {\n\
             System.out.println(failure.toString());\n\
             }\n\
             }\n\
             }",
            "run",
        );
        assert_eq!(out, "java.lang.NumberFormatException: 12x\n");
    }
}
