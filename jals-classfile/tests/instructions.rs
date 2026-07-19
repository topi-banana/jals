//! Instruction-decoding assertions, including the alignment-sensitive `tableswitch` /
//! `lookupswitch` (the byte-exact round-trip over `Switches.class` exercises the padding, these
//! assert the structure is actually decoded).

use std::path::PathBuf;

use jals_classfile::{AttributeBody, ClassFile, Instruction, MethodInfo};

fn load(name: &str) -> ClassFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let bytes = std::fs::read(path).expect("read fixture");
    jals_exec::block_on_inline(ClassFile::read(bytes.as_slice())).expect("parse fixture")
}

fn method<'a>(cf: &'a ClassFile, name: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| cf.constant_pool.utf8(m.name_index).as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no method named {name}"))
}

fn code(m: &MethodInfo) -> &[Instruction] {
    m.attributes
        .iter()
        .find_map(|a| match &a.body {
            AttributeBody::Code(c) => Some(c.code.as_slice()),
            _ => None,
        })
        .expect("method has a Code attribute")
}

#[test]
fn decodes_tableswitch_and_lookupswitch() {
    let cf = load("Switches.class");
    let dense = code(method(&cf, "dense"));
    assert!(
        dense
            .iter()
            .any(|i| matches!(i, Instruction::TableSwitch { .. })),
        "dense switch should decode a tableswitch"
    );
    let sparse = code(method(&cf, "sparse"));
    assert!(
        sparse
            .iter()
            .any(|i| matches!(i, Instruction::LookupSwitch { .. })),
        "sparse switch should decode a lookupswitch"
    );
}

#[test]
fn decodes_field_access_instructions() {
    let cf = load("Plain.class");
    let get = code(method(&cf, "get"));
    assert!(
        get.iter().any(|i| matches!(i, Instruction::Aload0)),
        "aload_0"
    );
    assert!(
        get.iter().any(|i| matches!(i, Instruction::GetField(_))),
        "getfield"
    );
    assert!(
        get.iter().any(|i| matches!(i, Instruction::Ireturn)),
        "ireturn"
    );
}
