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

use jals_build::{
    Assertions, BackendOptions, BackendRequest, BackendSelection, BackendSource,
};
use jals_config::BackendKind;
use jals_native::{PackageSelection, ResolverChain, SourceKind, StaticResolver};
use jals_platform::{CapturedHost, JavaBase};
use jals_progress::Progress;
use jals_storage::{CacheKey, CacheNamespace, ContentDigest, RelativePath};
use std::rc::Rc;

/// The platform over a host whose clock reads a fixed instant, so a test can assert an exact number.
fn platform(host: Rc<CapturedHost>) -> PackageSelection {
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
fn compile(sources: &[(&str, &str)], packages: PackageSelection) -> Result<Vec<u8>, String> {
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
    Ok(outcome
        .artifacts
        .into_iter()
        .next()
        .expect("one module")
        .1)
}

/// **It compiles.** Every implementation unit is lowered, not only the reachable ones — a library
/// input has every body compiled — so this is the whole 50-odd files going through the backend.
#[test]
fn the_platform_compiles_into_a_module() {
    let host = Rc::new(CapturedHost::new());
    let module = compile(
        &[("Main.java", "public final class Main { public static int run() { return 1; } }")],
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

    let linked: Vec<&str> = selection.link_sources().map(|(_, s)| s.path.as_ref()).collect();
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
    let indexed: Vec<&str> = selection.analysis_sources().map(|(_, s)| s.path.as_ref()).collect();
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

