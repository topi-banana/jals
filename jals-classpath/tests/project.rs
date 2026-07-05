//! The `assemble_project_inputs` seam: one call turns a manifest + capabilities into the analysis /
//! build inputs, and [`ProjectInputOptions`] decides which inputs get assembled.

use std::io::Write;
use std::path::Path;

use jals_classpath::{ProjectInputOptions, assemble_project_inputs};
use jals_config::Manifest;

/// `Box.class` (the same fixture the load tests / `jals-hir`'s classpath bridge use).
const BOX_CLASS: &[u8] = include_bytes!("fixtures/Box.class");

/// Write a one-class jar (`com/example/Box.class`) to `path`.
fn write_jar(path: &Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("com/example/Box.class", options).unwrap();
    zip.write_all(BOX_CLASS).unwrap();
    zip.finish().unwrap();
}

/// A manifest with a single local-jar `[dependencies]` entry pointing at `jar`, optionally declaring
/// a `[package] edition`.
fn manifest_with_jar(jar: &Path, edition: Option<&str>) -> Manifest {
    let edition_line = edition
        .map(|e| format!("edition = \"{e}\"\n"))
        .unwrap_or_default();
    let text = format!(
        "[package]\nname = \"demo\"\n{edition_line}\n[dependencies]\nbox = {{ jar = \"{}\" }}\n",
        jar.display()
    );
    text.parse::<Manifest>().expect("parse manifest")
}

#[test]
fn analysis_loads_the_classpath_and_reads_edition() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("box.jar");
    write_jar(&jar);
    let manifest = manifest_with_jar(&jar, Some("java25"));

    let inputs =
        assemble_project_inputs(&manifest, dir.path(), ProjectInputOptions::Analysis, |m| {
            panic!("unexpected warning: {m}")
        });

    // The jar is resolved and its `.class` loaded for the index.
    assert_eq!(inputs.dependency_jars, vec![jar.clone()]);
    assert_eq!(inputs.classpath_classes.len(), 1);
    // Analysis pulls no navigation source and no git/path source deps.
    assert!(inputs.library_sources.is_empty());
    assert!(inputs.source_dep_sources.is_empty());
    // `[package] edition = "java25"` threads through for the edition-gated lint rules.
    assert_eq!(inputs.target_java_version, Some(25));
}

#[test]
fn compile_resolves_jars_without_loading_classes() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("box.jar");
    write_jar(&jar);
    let manifest = manifest_with_jar(&jar, None);

    let inputs =
        assemble_project_inputs(&manifest, dir.path(), ProjectInputOptions::Compile, |m| {
            panic!("unexpected warning: {m}")
        });

    // The jar path is resolved for `javac -classpath`, but the `.class` is not loaded (the compiler
    // reads the jar itself).
    assert_eq!(inputs.dependency_jars, vec![jar]);
    assert!(inputs.classpath_classes.is_empty());
    // No `git`/`path` source dependencies in this manifest.
    assert!(inputs.source_dep_sources.is_empty());
    assert_eq!(inputs.target_java_version, None);
}
