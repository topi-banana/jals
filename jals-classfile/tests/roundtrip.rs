//! Byte-exact round-trip over the committed `.class` fixtures: `read(b).write() == b`.
//!
//! This is the crate's primary correctness anchor — every "derive counts/lengths, keep raw indices,
//! preserve unknown attributes verbatim" decision in the codec exists to make it hold.

use std::path::{Path, PathBuf};

use jals_classfile::ClassFile;

/// Every `.class` file under `tests/fixtures/`, as `(file name, bytes)`, sorted by name.
fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read fixtures dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("class") {
            let bytes = std::fs::read(&path).expect("read fixture");
            out.push((file_name(&path), bytes));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn roundtrip_is_byte_exact() {
    let fixtures = fixtures();
    assert!(!fixtures.is_empty(), "no .class fixtures found");
    for (name, bytes) in fixtures {
        let class = ClassFile::read(&bytes).unwrap_or_else(|e| panic!("read {name}: {e}"));
        assert_eq!(class.write(), bytes, "round-trip mismatch for {name}");
    }
}

#[test]
fn serde_json_roundtrip_preserves_the_model() {
    for (name, bytes) in fixtures() {
        let class = ClassFile::read(&bytes).unwrap_or_else(|e| panic!("read {name}: {e}"));
        let json = serde_json::to_string(&class).expect("serialize");
        let back: ClassFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, class, "model changed across serde for {name}");
        assert_eq!(back.write(), bytes, "serde round-trip mismatch for {name}");
    }
}
