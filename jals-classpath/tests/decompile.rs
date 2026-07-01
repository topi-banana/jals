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

/// `Consts` (package `demo`), compiled with `-parameters -g` — the fixture for the M0 enrichments:
/// `ConstantValue` initializers, real parameter names, a declared checked exception, and the body
/// shapes (value-returning / `void` / constructor).
fn consts_class() -> ClassFile {
    jals_classfile::read(include_bytes!("fixtures/Consts.class")).expect("parse Consts.class")
}

/// `Branchy` (package `demo`), compiled with `-parameters -g` — the fixture for M2 `if` / `if-else`
/// control-flow structuring.
fn branchy_class() -> ClassFile {
    jals_classfile::read(include_bytes!("fixtures/Branchy.class")).expect("parse Branchy.class")
}

/// Every committed fixture class, including `jals-classfile`'s round-trip fixtures, so the skeleton
/// renderer is exercised across generics, enums, records, switches, and annotations.
fn all_fixture_classes() -> Vec<ClassFile> {
    const BYTES: &[&[u8]] = &[
        include_bytes!("fixtures/Box.class"),
        include_bytes!("fixtures/Consts.class"),
        include_bytes!("fixtures/Branchy.class"),
        include_bytes!("fixtures/Outer.class"),
        include_bytes!("fixtures/Outer$Inner.class"),
        include_bytes!("fixtures/Outer$Color.class"),
        include_bytes!("../../jals-classfile/tests/fixtures/Plain.class"),
        include_bytes!("../../jals-classfile/tests/fixtures/Iface.class"),
        include_bytes!("../../jals-classfile/tests/fixtures/Sample.class"),
        include_bytes!("../../jals-classfile/tests/fixtures/Sample$Kind.class"),
        include_bytes!("../../jals-classfile/tests/fixtures/Sample$Visitor.class"),
        include_bytes!("../../jals-classfile/tests/fixtures/Sample$Point.class"),
        include_bytes!("../../jals-classfile/tests/fixtures/Switches.class"),
        include_bytes!("../../jals-classfile/tests/fixtures/TypeAnno.class"),
        include_bytes!("../../jals-classfile/tests/fixtures/TypeAnno$RNonNull.class"),
        include_bytes!("../../jals-classfile/tests/fixtures/TypeAnno$CNonNull.class"),
    ];
    BYTES
        .iter()
        .map(|bytes| jals_classfile::read(bytes).expect("parse fixture"))
        .collect()
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

    // The generic type and its members, each with its decompiled body.
    let text = std::fs::read_to_string(box_java).unwrap();
    assert!(text.contains("public class Box<T> {"), "{text}");
    assert!(text.contains("private T value;"), "{text}");
    assert!(text.contains("return this.value;"), "{text}");
    assert!(text.contains("this.value = arg0;"), "{text}");
    assert!(text.contains("public Box() {}"), "{text}");
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
        text.contains("public java.lang.String greet(java.lang.String arg0) {"),
        "{text}"
    );
    // `greet` returns its parameter — decompiled straight through.
    assert!(text.contains("return arg0;"), "{text}");
    // The nested static class and enum are inlined into the enclosing body, with the enum constants.
    assert!(text.contains("public static class Inner {"), "{text}");
    assert!(text.contains("public enum Color {"), "{text}");
    assert!(text.contains("RED, GREEN, BLUE;"), "{text}");
}

#[test]
fn renders_constants_parameter_names_throws_and_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let mut warnings = Vec::new();
    let files = synthesize_classpath_sources(&[consts_class()], dir.path(), |m| warnings.push(m));
    assert!(warnings.is_empty(), "{warnings:?}");
    let text = std::fs::read_to_string(&files[0]).unwrap();

    // `ConstantValue` initializers across every constant kind (a boolean's `1` shows as `true`).
    assert!(text.contains("public static final int MAX = 42;"), "{text}");
    assert!(
        text.contains("public static final long BIG = 9000000000L;"),
        "{text}"
    );
    assert!(
        text.contains("public static final double RATE = 1.5"),
        "{text}"
    );
    assert!(
        text.contains("public static final float RATIO = 0.25f;"),
        "{text}"
    );
    assert!(
        text.contains("public static final boolean ENABLED = true;"),
        "{text}"
    );
    assert!(
        text.contains("public static final java.lang.String NAME = \"jals\";"),
        "{text}"
    );

    // Real parameter names (from `-parameters` / `-g`), a declared checked exception, and decompiled
    // bodies: a field-storing constructor, an arithmetic return, an empty `void`, and a `throw`.
    assert!(text.contains("this.count = start;"), "{text}");
    assert!(text.contains("return this.count + delta;"), "{text}");
    assert!(text.contains("public void reset() {}"), "{text}");
    assert!(
        text.contains("throw new java.io.IOException(path);"),
        "{text}"
    );
    assert!(
        text.contains("public void risky(java.lang.String path) throws java.io.IOException {"),
        "{text}"
    );
}

#[test]
fn renders_recovered_control_flow() {
    let dir = tempfile::tempdir().unwrap();
    let mut warnings = Vec::new();
    let files = synthesize_classpath_sources(&[branchy_class()], dir.path(), |m| warnings.push(m));
    assert!(warnings.is_empty(), "{warnings:?}");
    let text = std::fs::read_to_string(&files[0]).unwrap();

    // A guard-clause `if` (the `then` returns, so there is no `else`).
    assert!(text.contains("if (a > b) {"), "{text}");
    // A null guard whose body stores a call result to a field.
    assert!(text.contains("if (s != null) {"), "{text}");
    assert!(text.contains("this.value = s.length();"), "{text}");
    // A real `if`-`else` with a join afterwards.
    assert!(text.contains("} else {"), "{text}");
    assert!(text.contains("this.value = this.value + 1;"), "{text}");

    // The recovered bodies must parse cleanly.
    assert!(
        jals_syntax::parse(&text).errors().is_empty(),
        "{text}\n{:#?}",
        jals_syntax::parse(&text).errors()
    );
}

#[test]
fn synthesized_skeletons_are_valid_java() {
    // Whatever the renderer emits for any fixture, it must parse without syntax errors — the
    // invariant that keeps go-to-definition landing in a well-formed file.
    let dir = tempfile::tempdir().unwrap();
    let mut warnings = Vec::new();
    let files =
        synthesize_classpath_sources(&all_fixture_classes(), dir.path(), |m| warnings.push(m));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(!files.is_empty(), "expected some skeletons");
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let parse = jals_syntax::parse(&text);
        assert!(
            parse.errors().is_empty(),
            "{}: {:#?}\n{text}",
            file.display(),
            parse.errors()
        );
    }
}
