//! The Java a **native package** publishes, indexed through
//! [`ProjectIndexBuilder::with_native_packages`].
//!
//! A native package's classes are compiled into the same artifact the project is, so what they
//! declare the program *has* — which is the whole difference from a stub, and what every assertion
//! below is about: the origin is its own, the declarations are complete rather than lenient, and a
//! package outranks a stub of the same name because the one with a body is the one that will run.

use jals_hir::{FileAnalysis, FileId, ItemOrigin, LibraryFidelity, ProjectIndex, TypeResolution};
use jals_syntax::SyntaxNode;

/// The platform library at **signature** fidelity — what every host but a linking wasm build
/// indexes, and what the embedded stubs used to be.
///
/// One text, read as a record: the real JDK behind a `javac` build is a superset of it, so a
/// member it omits is a gap in the record rather than an absence in the program.
fn platform() -> Vec<jals_hir::LibraryFile> {
    jals_exec::block_on_inline(jals_hir::LibraryFile::parse_tiers(
        &jals_platform::JavaBase::tiers(false),
    ))
}

/// The package's own Java, in the shape a host hands it over: parsed, under ids of the host's
/// choosing.
fn parse(sources: &[&str], base: u32) -> Vec<(FileId, SyntaxNode)> {
    sources
        .iter()
        .enumerate()
        .map(|(index, text)| {
            (
                FileId(base + u32::try_from(index).expect("a small fixture")),
                jals_exec::block_on_inline(jals_syntax::Parse::parse(text)).syntax(),
            )
        })
        .collect()
}

/// One project file indexed against `packages` — the packages at **`Complete`** fidelity, the
/// platform behind them as a record, exactly as a linking wasm build sees them.
///
/// One library slot, two tiers. A caller cannot rank a unit by choosing which argument to pass it
/// in, which is what the two separate slots this replaced allowed.
fn index_of(project: &str, packages: &[&str]) -> (ProjectIndex, SyntaxNode) {
    let project = parse(&[project], 0);
    let mut library = jals_exec::block_on_inline(jals_hir::LibraryFile::parse(
        &packages
            .iter()
            .map(|text| (*text, LibraryFidelity::Complete))
            .collect::<Vec<_>>(),
    ));
    for (offset, unit) in platform().into_iter().enumerate() {
        library.push(jals_hir::LibraryFile {
            file: jals_hir::FileId::library(
                u32::try_from(packages.len() + offset).expect("a small fixture"),
            ),
            ..unit
        });
    }
    let index = jals_exec::block_on_inline(
        ProjectIndex::builder(&project)
            .with_library(&library)
            .build(),
    );
    (index, project[0].1.clone())
}

const OUT: &str = "package jals.io;\n\
                   public final class Out {\n\
                   \x20   public static native void writeChar(int codeUnit);\n\
                   \x20   public static void println(char[] text) {}\n\
                   }\n";

/// A package's type is indexed under its own origin, with its members.
#[test]
fn a_packages_type_is_indexed_under_its_own_origin() {
    let (index, _) = index_of("class C {}", &[OUT]);
    let item = index
        .item_by_fqn("jals.io.Out")
        .expect("the package's class is indexed");
    assert_eq!(
        index.item(item).origin,
        ItemOrigin::Library(LibraryFidelity::Complete)
    );
    // Real source, written by the package's author — so silence about an annotation is a fact.
    assert!(ItemOrigin::Library(LibraryFidelity::Complete).carries_annotations());
    // Never a file the host owns: the text is a constant in the binary that shipped the package.
    assert!(!ItemOrigin::Library(LibraryFidelity::Complete).is_host_editable());

    let members: Vec<&str> = index
        .own_members(item)
        .iter()
        .map(|id| index.member(*id).name.as_str())
        .collect();
    assert!(members.contains(&"writeChar"), "{members:?}");
    assert!(members.contains(&"println"), "{members:?}");
}

/// A project file that imports a package's class resolves it — which is the whole reason the
/// analysis is given the package at all. Without it, every name into the package is unresolved and
/// the analysis reports the absence of code the build compiles.
#[test]
fn a_project_file_resolves_a_name_into_the_package() {
    let source = "import jals.io.Out;\n\
                  class C { void m() { Out.println(new char[]{'a'}); } }\n";
    let (index, root) = index_of(source, &[OUT]);
    let analysis = jals_exec::block_on_inline(FileAnalysis::of(&root));
    let unresolved =
        jals_exec::block_on_inline(analysis.in_project(&index, FileId(0)).unresolved_names());
    assert!(unresolved.is_empty(), "{unresolved:?}");

    let item = index
        .item_by_fqn("jals.io.Out")
        .expect("the package's class is indexed");
    assert_eq!(
        index.resolve_type_name(FileId(0), "Out", None),
        TypeResolution::Project(item),
        "the single-type import reaches the package's class"
    );
}

/// Without the package the same file does *not* resolve. The complement, so the test above cannot
/// pass for a reason that has nothing to do with the package.
#[test]
fn without_the_package_the_same_name_does_not_resolve() {
    let source = "import jals.io.Out;\n\
                  class C { void m() { Out.println(new char[]{'a'}); } }\n";
    let (index, _) = index_of(source, &[]);
    assert!(index.item_by_fqn("jals.io.Out").is_none());
}

/// A package outranks a stub declaring the same name: one describes a JDK nobody here has, the
/// other is compiled into the artifact that will run.
#[test]
fn a_package_outranks_a_stub_of_the_same_name() {
    let package = "package java.io;\n\
                   public class PrintStream {\n\
                   \x20   public void println(char[] text) {}\n\
                   }\n";
    let (index, _) = index_of("class C {}", &[package]);
    let item = index
        .item_by_fqn("java.io.PrintStream")
        .expect("indexed by both");
    assert_eq!(
        index.item(item).origin,
        ItemOrigin::Library(LibraryFidelity::Complete)
    );
}

/// A project type still outranks a package's, exactly as it outranks a library source's: the
/// priority order is project, then source dependencies, then packages, then the classpath, then
/// the stubs.
#[test]
fn a_project_type_still_wins_a_name_clash() {
    let project = "package jals.io;\npublic final class Out { public static void mine() {} }\n";
    let (index, _) = index_of(project, &[OUT]);
    let item = index.item_by_fqn("jals.io.Out").expect("indexed by both");
    assert_eq!(index.item(item).origin, ItemOrigin::Project);
}
