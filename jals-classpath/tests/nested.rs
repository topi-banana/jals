//! Recursively unpacking a jar's **bundled jars** (`recursive = true`): only `*.jar` members are
//! extracted (at any depth, into a per-jar subdir), the extracted jars are loadable end-to-end,
//! extraction is idempotent, and a corrupt bundled jar is a warning, not a failure. Also covers the
//! `resolve_project_dependencies` wiring that adds the unpacked jars to a project's classpath.

use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::Path;

use jals_classpath::{extract_nested_jars, load_classpath, resolve_project_dependencies};
use jals_config::{Dependency, JarDependency, Manifest};

/// `Box.class` (the same fixture `load.rs` and `jals-hir`'s classpath-bridge tests use).
const BOX_CLASS: &[u8] = include_bytes!("fixtures/Box.class");

/// Build a tiny but real (deflated) jar in memory whose members are the given `(name, bytes)` pairs,
/// so a jar can be embedded as a member of another jar (a bundled jar).
fn jar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(content).unwrap();
        }
        zip.finish().unwrap();
    }
    buf.into_inner()
}

/// Write a jar at `path` whose members are the given `(name, bytes)` pairs.
fn write_jar(path: &Path, entries: &[(&str, &[u8])]) {
    std::fs::write(path, jar_bytes(entries)).unwrap();
}

#[test]
fn extracts_and_loads_a_bundled_jar() {
    let dir = tempfile::tempdir().unwrap();
    let inner = jar_bytes(&[("com/example/Box.class", BOX_CLASS)]);
    let fat = dir.path().join("fat.jar");
    write_jar(
        &fat,
        &[
            // A top-level `.class` (a Spring-Boot loader class) and the bundled jar.
            ("org/springframework/boot/loader/Launcher.class", b"loader"),
            ("BOOT-INF/lib/inner.jar", &inner),
        ],
    );

    let dest = dir.path().join("nested");
    let extraction = extract_nested_jars(&fat, &dest);
    assert!(extraction.warnings.is_empty(), "{:?}", extraction.warnings);

    // Only the `*.jar` member is unpacked; the top-level `.class` is left to `load_classpath`.
    assert_eq!(extraction.jars.len(), 1);
    let nested_jar = &extraction.jars[0];
    assert!(
        nested_jar.ends_with("BOOT-INF/lib/inner.jar"),
        "unexpected path {}",
        nested_jar.display()
    );
    assert!(nested_jar.starts_with(&dest));

    // And it is loadable: the bundled library's class parses through the normal classpath path.
    let load = load_classpath(std::slice::from_ref(nested_jar));
    assert_eq!(load.classes.len(), 1, "{:?}", load.warnings);
    assert!(load.warnings.is_empty(), "{:?}", load.warnings);
}

#[test]
fn recurses_into_doubly_nested_jars() {
    let dir = tempfile::tempdir().unwrap();
    let inner = jar_bytes(&[("com/example/Box.class", BOX_CLASS)]);
    let mid = jar_bytes(&[("lib/inner.jar", &inner)]);
    let fat = dir.path().join("fat.jar");
    write_jar(&fat, &[("lib/mid.jar", &mid)]);

    let dest = dir.path().join("nested");
    let extraction = extract_nested_jars(&fat, &dest);
    assert!(extraction.warnings.is_empty(), "{:?}", extraction.warnings);

    // Both the middle jar and the jar it itself bundles are extracted (jar-in-jar-in-jar).
    assert_eq!(extraction.jars.len(), 2);
    assert!(extraction.jars.iter().any(|p| p.ends_with("lib/mid.jar")));
    let leaf = extraction
        .jars
        .iter()
        .find(|p| p.ends_with("lib/inner.jar"))
        .expect("the doubly-nested jar is extracted");

    // The deepest jar carries the class.
    let load = load_classpath(std::slice::from_ref(leaf));
    assert_eq!(load.classes.len(), 1, "{:?}", load.warnings);
}

#[test]
fn extraction_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let inner = jar_bytes(&[("com/example/Box.class", BOX_CLASS)]);
    let fat = dir.path().join("fat.jar");
    write_jar(&fat, &[("BOOT-INF/lib/inner.jar", &inner)]);
    let dest = dir.path().join("nested");

    let first = extract_nested_jars(&fat, &dest);
    assert_eq!(first.jars.len(), 1);
    // A second run reuses the file already on disk (skip-if-exists), yielding the same path.
    let second = extract_nested_jars(&fat, &dest);
    assert_eq!(first.jars, second.jars);
    assert!(second.warnings.is_empty(), "{:?}", second.warnings);
}

#[test]
fn corrupt_bundled_jar_is_a_warning_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let fat = dir.path().join("fat.jar");
    // A bundled "jar" that is not a real archive.
    write_jar(&fat, &[("lib/bad.jar", b"not a zip archive")]);

    let extraction = extract_nested_jars(&fat, &dir.path().join("nested"));
    // The bytes are still written to disk and returned (the host will warn again if it can't load it)...
    assert_eq!(extraction.jars.len(), 1);
    assert!(extraction.jars[0].ends_with("lib/bad.jar"));
    // ...but recursing into it (to look for deeper jars) warns rather than aborting.
    assert_eq!(extraction.warnings.len(), 1, "{:?}", extraction.warnings);
    assert!(
        extraction.warnings[0]
            .message
            .contains("failed to read jar"),
        "{:?}",
        extraction.warnings
    );
}

#[test]
fn resolve_project_dependencies_unpacks_a_recursive_jar() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let inner = jar_bytes(&[("com/example/Box.class", BOX_CLASS)]);
    let fat = root.join("fat.jar");
    write_jar(&fat, &[("BOOT-INF/lib/inner.jar", &inner)]);

    // A `recursive = true` jar dependency pointing at the local fat jar (a bare absolute path).
    let manifest = Manifest {
        dependencies: BTreeMap::from([(
            "fat".to_string(),
            Dependency::Jar(JarDependency {
                jar: fat.to_string_lossy().into_owned(),
                sources: None,
                recursive: Some(true),
            }),
        )]),
        ..Default::default()
    };

    let mut warnings = Vec::new();
    let jars = resolve_project_dependencies(&manifest, root, |m| warnings.push(m));
    assert!(warnings.is_empty(), "{warnings:?}");

    // The fat jar itself (resolved as-is) plus its one bundled jar (unpacked under target/jals/deps).
    assert_eq!(jars.len(), 2, "{jars:?}");
    assert!(jars.iter().any(|p| p == &fat));
    let nested = jars
        .iter()
        .find(|p| p.ends_with("BOOT-INF/lib/inner.jar"))
        .expect("the bundled jar is on the classpath");
    assert!(nested.starts_with(root.join("target/jals/deps/nested")));

    // Without `recursive`, the bundled jar stays sealed: only the fat jar resolves.
    let plain = Manifest {
        dependencies: BTreeMap::from([(
            "fat".to_string(),
            Dependency::Jar(JarDependency {
                jar: fat.to_string_lossy().into_owned(),
                sources: None,
                recursive: None,
            }),
        )]),
        ..Default::default()
    };
    let jars = resolve_project_dependencies(&plain, root, |m| warnings.push(m));
    assert_eq!(jars, vec![fat]);
}
