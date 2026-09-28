//! Compile `src/java` into the wasm module this crate ships.
//!
//! The pipeline is the same one `jals-build`'s in-process backend runs, at the level the compiler
//! offers it: parse every source, index them together with the standard-library stubs the platform
//! shadows, type each file, and compile the lot as a **library** — no exports of a program's own,
//! every class's surface stated at the ABI boundary and every body lowered inside the module.
//!
//! What the build produces is not only code. The ABI section of the module carries the Java the
//! module was built from, which is what lets a *consumer* resolve `new String(chars)` against the
//! real class while linking the compiled body of its constructor. That is why the sources are
//! handed to `CompileWasm::library` here rather than left where they are: the two are one artifact,
//! and an artifact that states its own source cannot be stale against it.
//!
//! Nothing here reads the clock, the environment, or an unordered directory listing: the bytes are
//! a function of `java/`, which is what lets a binary fold them into the cache key of every program
//! that links them.

use std::path::{Path, PathBuf};

use jals_hir::{FileAnalysis, FileId, FileSemantics, ProjectIndex, TypedFile};
use jals_javac::wasm::{CompileWasm, Source, WasmOptions};

// The version travels with the artifact, so the two places that need it share one file — and a
// bump cannot be half-applied.
include!("src/version.rs");

const NAME: &str = "java.base";

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("java");
    println!("cargo:rerun-if-changed={}", root.display());
    let sources = read_sources(&root);
    let bytes = compile(&sources);
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("java.base.wasm");
    std::fs::write(&out, bytes).expect("the platform module is writable");
}

/// Every `.java` file under `root`, as `(path relative to root, text)`, in path order.
///
/// Path order rather than directory order: `read_dir` yields whatever the filesystem feels like,
/// and an artifact whose bytes depend on that is an artifact two machines disagree about.
fn read_sources(root: &Path) -> Vec<(String, String)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("`{}` is readable: {error}", dir.display()));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "java")
            {
                let relative = path
                    .strip_prefix(root)
                    .expect("a path under the platform root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("`{}` is readable: {error}", path.display()));
                out.push((relative, text));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// The library compile, exactly as a consumer's compile sees it through the ABI.
fn compile(sources: &[(String, String)]) -> Vec<u8> {
    let mut roots: Vec<(FileId, jals_syntax::SyntaxNode)> = Vec::with_capacity(sources.len());
    for (position, (path, text)) in sources.iter().enumerate() {
        let parsed = jals_exec::block_on_inline(jals_syntax::Parse::parse(text));
        assert!(
            parsed.errors().is_empty(),
            "`{path}` does not parse: {:?}",
            parsed.errors()
        );
        let file = FileId(u32::try_from(position).expect("a source count that fits"));
        roots.push((file, parsed.syntax()));
    }
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
    let (module, _) = CompileWasm::library(
        &typed,
        &index,
        WasmOptions::default(),
        NAME,
        VERSION,
        sources
            .iter()
            .map(|(path, text)| Source {
                path: path.clone(),
                text: text.clone(),
            })
            .collect(),
    )
    .unwrap_or_else(|error| panic!("`java.base` does not compile: {error}"));
    module
        .finish()
        .unwrap_or_else(|| panic!("`java.base` does not encode"))
}
