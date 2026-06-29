//! Synthesizing signature-only `.java` skeletons from classpath `.class` files
//! ([`synthesize_classpath_sources`]): a dependency class with no real source still gets a navigable
//! declaration on disk. Covers the cache layout, the rendered signatures, idempotent re-runs, and the
//! grouping of nested types into their enclosing type's file.

use std::path::Path;

use jals_classfile::ClassFile;
use jals_classpath::synthesize_classpath_sources;

fn box_class() -> ClassFile {
    jals_classfile::read(include_bytes!("fixtures/Box.class")).expect("parse Box.class")
}

/// `Outer` plus its nested `Outer$Inner` (a static class) and `Outer$Color` (an enum), in package
/// `demo` — the fixture for nested-type grouping.
fn outer_classes() -> Vec<ClassFile> {
    [
        include_bytes!("fixtures/Outer.class").as_slice(),
        include_bytes!("fixtures/Outer$Inner.class").as_slice(),
        include_bytes!("fixtures/Outer$Color.class").as_slice(),
    ]
    .into_iter()
    .map(|bytes| jals_classfile::read(bytes).expect("parse fixture"))
    .collect()
}

#[test]
fn synthesizes_a_skeleton_for_a_class_without_sources() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut warnings = Vec::new();
    let files = synthesize_classpath_sources(&[box_class()], root, |m| warnings.push(m));
    assert!(warnings.is_empty(), "{warnings:?}");

    // One file, written under the decompiled-sources cache, named for the (default-package) type.
    assert_eq!(files.len(), 1, "{files:?}");
    let box_java = &files[0];
    assert!(box_java.ends_with("Box.java"), "{}", box_java.display());
    assert!(box_java.starts_with(root.join("target/jals/deps/decompiled")));

    // The generic type and its members, signatures only (no bodies).
    let text = std::fs::read_to_string(box_java).unwrap();
    assert!(text.contains("public class Box<T> {"), "{text}");
    assert!(text.contains("private T value;"), "{text}");
    assert!(text.contains("public T get();"), "{text}");
    assert!(text.contains("public void set(T arg0);"), "{text}");
    assert!(!text.contains("return"), "no method bodies: {text}");
}

#[test]
fn synthesis_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut warnings = Vec::new();
    let first = synthesize_classpath_sources(&[box_class()], root, |m| warnings.push(m));
    // A second run reuses the file already on disk (skip-if-exists), yielding the same path.
    let second = synthesize_classpath_sources(&[box_class()], root, |m| warnings.push(m));
    assert_eq!(first, second);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn groups_nested_types_into_their_enclosing_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut warnings = Vec::new();
    let files = synthesize_classpath_sources(&outer_classes(), root, |m| warnings.push(m));
    assert!(warnings.is_empty(), "{warnings:?}");

    // One file per top-level type: `Inner` and `Color` fold into `demo/Outer.java`, not their own.
    assert_eq!(files.len(), 1, "{files:?}");
    let outer = &files[0];
    assert!(
        outer.ends_with(Path::new("demo").join("Outer.java")),
        "{}",
        outer.display()
    );

    let text = std::fs::read_to_string(outer).unwrap();
    assert!(text.contains("package demo;"), "{text}");
    assert!(text.contains("public class Outer {"), "{text}");
    assert!(
        text.contains("public java.lang.String greet(java.lang.String arg0);"),
        "{text}"
    );
    // The nested static class and enum are inlined into the enclosing body, with the enum constants.
    assert!(text.contains("public static class Inner {"), "{text}");
    assert!(text.contains("public enum Color {"), "{text}");
    assert!(text.contains("RED, GREEN, BLUE;"), "{text}");
}
