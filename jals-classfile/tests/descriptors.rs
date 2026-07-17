//! Validate the descriptor and signature parsers against real `javac` output: every descriptor and
//! `Signature` string in the fixtures must parse and render back to itself.

use std::path::PathBuf;

use jals_classfile::{
    Attribute, AttributeBody, ClassFile, ClassSignature, FieldType, MethodDescriptor,
    MethodSignature, TypeSignature,
};

const FIXTURES: &[&str] = &[
    "Plain.class",
    "Iface.class",
    "Sample.class",
    "Sample$Kind.class",
    "Sample$Point.class",
    "Switches.class",
    "TypeAnno.class",
    "module-info.class",
];

fn load(name: &str) -> ClassFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let bytes = std::fs::read(path).expect("read fixture");
    ClassFile::read(bytes.as_slice()).expect("parse fixture")
}

fn signature(cf: &ClassFile, attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|a| match &a.body {
        AttributeBody::Signature { signature_index } => cf
            .constant_pool
            .utf8(*signature_index)
            .map(std::borrow::Cow::into_owned),
        _ => None,
    })
}

#[test]
fn field_and_method_descriptors_round_trip() {
    let mut checked = 0usize;
    for name in FIXTURES {
        let cf = load(name);
        for field in &cf.fields {
            let desc = cf
                .constant_pool
                .utf8(field.descriptor_index)
                .unwrap()
                .into_owned();
            let parsed = FieldType::parse(&desc)
                .unwrap_or_else(|e| panic!("{name}: field descriptor {desc:?}: {e}"));
            assert_eq!(parsed.to_string(), desc, "{name}: field descriptor");
            checked += 1;
        }
        for method in &cf.methods {
            let desc = cf
                .constant_pool
                .utf8(method.descriptor_index)
                .unwrap()
                .into_owned();
            let parsed = MethodDescriptor::parse(&desc)
                .unwrap_or_else(|e| panic!("{name}: method descriptor {desc:?}: {e}"));
            assert_eq!(parsed.to_string(), desc, "{name}: method descriptor");
            checked += 1;
        }
    }
    assert!(checked > 0, "no descriptors were checked");
}

#[test]
fn signatures_round_trip() {
    let mut checked = 0usize;
    for name in FIXTURES {
        let cf = load(name);

        if let Some(sig) = signature(&cf, &cf.attributes) {
            let parsed = ClassSignature::parse(&sig)
                .unwrap_or_else(|e| panic!("{name}: class signature {sig:?}: {e}"));
            assert_eq!(parsed.to_string(), sig, "{name}: class signature");
            checked += 1;
        }
        for field in &cf.fields {
            if let Some(sig) = signature(&cf, &field.attributes) {
                let parsed = TypeSignature::parse(&sig)
                    .unwrap_or_else(|e| panic!("{name}: field signature {sig:?}: {e}"));
                assert_eq!(parsed.to_string(), sig, "{name}: field signature");
                checked += 1;
            }
        }
        for method in &cf.methods {
            if let Some(sig) = signature(&cf, &method.attributes) {
                let parsed = MethodSignature::parse(&sig)
                    .unwrap_or_else(|e| panic!("{name}: method signature {sig:?}: {e}"));
                assert_eq!(parsed.to_string(), sig, "{name}: method signature");
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "no Signature attributes were checked");
}
