//! Decoding assertions: prove the modelled attributes are actually decoded (not silently kept as
//! `Unknown`), which a byte-exact round-trip alone would not catch.

use std::path::PathBuf;

use jals_classfile::{Attribute, AttributeBody, ClassFile, ConstantPool};

fn load(name: &str) -> ClassFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let bytes = std::fs::read(path).expect("read fixture");
    jals_exec::block_on_inline(ClassFile::read(bytes.as_slice())).expect("parse fixture")
}

fn any_body(attrs: &[Attribute], pred: impl Fn(&AttributeBody) -> bool) -> bool {
    attrs.iter().any(|a| pred(&a.body))
}

/// Collect the names of every attribute left as `Unknown`, descending into `Code` and `Record`.
fn unknown_names(cf: &ClassFile) -> Vec<String> {
    fn walk(attrs: &[Attribute], pool: &ConstantPool, out: &mut Vec<String>) {
        for a in attrs {
            match &a.body {
                AttributeBody::Unknown(_) => {
                    out.push(
                        pool.utf8(a.name_index)
                            .map(std::borrow::Cow::into_owned)
                            .unwrap_or_default(),
                    );
                }
                AttributeBody::Code(c) => walk(&c.attributes, pool, out),
                AttributeBody::Record(components) => {
                    for comp in components {
                        walk(&comp.attributes, pool, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    let pool = &cf.constant_pool;
    walk(&cf.attributes, pool, &mut out);
    for f in &cf.fields {
        walk(&f.attributes, pool, &mut out);
    }
    for m in &cf.methods {
        walk(&m.attributes, pool, &mut out);
    }
    out
}

#[test]
fn module_attribute_is_decoded() {
    let cf = load("module-info.class");
    assert!(
        any_body(&cf.attributes, |b| matches!(b, AttributeBody::Module(_))),
        "module-info should decode a Module attribute"
    );
}

#[test]
fn type_annotations_are_decoded() {
    let cf = load("TypeAnno.class");
    let on_field = cf.fields.iter().any(|f| {
        any_body(&f.attributes, |b| {
            matches!(b, AttributeBody::RuntimeVisibleTypeAnnotations(_))
        })
    });
    let on_method = cf.methods.iter().any(|m| {
        any_body(&m.attributes, |b| {
            matches!(b, AttributeBody::RuntimeVisibleTypeAnnotations(_))
        })
    });
    assert!(
        on_field || on_method,
        "expected a decoded RuntimeVisibleTypeAnnotations"
    );
}

#[test]
fn code_and_nested_attributes_are_decoded() {
    let cf = load("Sample.class");
    assert!(
        any_body(&cf.attributes, |b| matches!(
            b,
            AttributeBody::SourceFile { .. }
        )),
        "SourceFile"
    );
    assert!(
        any_body(&cf.attributes, |b| matches!(
            b,
            AttributeBody::InnerClasses(_)
        )),
        "InnerClasses"
    );
    let code = cf
        .methods
        .iter()
        .flat_map(|m| &m.attributes)
        .find_map(|a| match &a.body {
            AttributeBody::Code(c) => Some(c),
            _ => None,
        })
        .expect("a method with a Code attribute");
    assert!(!code.code.is_empty(), "Code should carry instruction bytes");
    assert!(
        any_body(&code.attributes, |b| matches!(
            b,
            AttributeBody::LineNumberTable(_)
        )),
        "nested LineNumberTable"
    );
}

#[test]
fn class_signature_is_decoded() {
    let cf = load("Iface.class");
    assert!(
        any_body(&cf.attributes, |b| matches!(
            b,
            AttributeBody::Signature { .. }
        )),
        "a generic interface should decode a class Signature"
    );
}

#[test]
fn stackmaptable_is_decoded() {
    // Sample.java's loop + branch and Switches.java's switches produce StackMapTable attributes.
    let cf = load("Sample.class");
    let frames: usize = cf
        .methods
        .iter()
        .flat_map(|m| &m.attributes)
        .filter_map(|a| match &a.body {
            AttributeBody::Code(c) => Some(c),
            _ => None,
        })
        .flat_map(|c| &c.attributes)
        .filter_map(|a| match &a.body {
            AttributeBody::StackMapTable(frames) => Some(frames.len()),
            _ => None,
        })
        .sum();
    assert!(
        frames > 0,
        "Sample should decode at least one StackMapTable frame"
    );
}

#[test]
fn every_fixture_attribute_decodes() {
    // With all standard attributes modelled, no attribute in any fixture should remain Unknown.
    for name in [
        "Plain.class",
        "Iface.class",
        "Sample.class",
        "Sample$Kind.class",
        "Sample$Point.class",
        "Switches.class",
        "TypeAnno.class",
        "module-info.class",
    ] {
        let cf = load(name);
        let unknown = unknown_names(&cf);
        assert!(
            unknown.is_empty(),
            "unexpected Unknown attribute(s) in {name}: {unknown:?}"
        );
    }
}
