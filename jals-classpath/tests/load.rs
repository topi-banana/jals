//! End-to-end loading of a classpath: directories of `.class` files and jars both yield parsed
//! [`ClassFile`]s, and unreadable entries become warnings rather than aborting the load.

use std::io::Write;
use std::path::{Path, PathBuf};

use jals_classpath::load_classpath;

/// `Box.class` (the same fixture `jals-hir` uses for its classpath-bridge tests).
const BOX_CLASS: &[u8] = include_bytes!("fixtures/Box.class");

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create dirs");
    }
    std::fs::write(path, bytes).expect("write file");
}

#[test]
fn loads_class_files_from_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    // A nested package layout, to confirm the walk recurses.
    write(&dir.path().join("pkg/Box.class"), BOX_CLASS);
    write(&dir.path().join("Box.class"), BOX_CLASS);
    // A non-class file is ignored.
    write(&dir.path().join("README.txt"), b"not a class");

    let load = load_classpath(&[dir.path().to_path_buf()]);
    assert_eq!(load.classes.len(), 2);
    assert!(load.warnings.is_empty(), "{:?}", load.warnings);
}

#[test]
fn loads_class_files_from_a_jar() {
    let dir = tempfile::tempdir().unwrap();
    let jar_path = dir.path().join("dep.jar");
    let file = std::fs::File::create(&jar_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    // Deflated, the real-world case — exercises the decompression path, not just `Stored`.
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("com/example/Box.class", options).unwrap();
    zip.write_all(BOX_CLASS).unwrap();
    // A directory entry and a non-class member must both be skipped silently.
    zip.add_directory("META-INF/", options).unwrap();
    zip.start_file("META-INF/MANIFEST.MF", options).unwrap();
    zip.write_all(b"Manifest-Version: 1.0\n").unwrap();
    zip.finish().unwrap();

    let load = load_classpath(&[jar_path]);
    assert_eq!(load.classes.len(), 1);
    assert!(load.warnings.is_empty(), "{:?}", load.warnings);
}

#[test]
fn loads_a_bare_class_file_entry() {
    let dir = tempfile::tempdir().unwrap();
    let class = dir.path().join("Box.class");
    write(&class, BOX_CLASS);

    let load = load_classpath(&[class]);
    assert_eq!(load.classes.len(), 1);
    assert!(load.warnings.is_empty());
}

#[test]
fn missing_entry_is_a_warning_not_a_failure() {
    let load = load_classpath(&[PathBuf::from("/no/such/path.jar")]);
    assert!(load.classes.is_empty());
    assert_eq!(load.warnings.len(), 1);
    assert!(load.warnings[0].message.contains("does not exist"));
}

#[test]
fn a_corrupt_class_is_skipped_but_siblings_still_load() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("Good.class"), BOX_CLASS);
    write(
        &dir.path().join("Bad.class"),
        b"\xca\xfe\xba\xbe not really a class",
    );

    let load = load_classpath(&[dir.path().to_path_buf()]);
    // The good one still loads; the bad one is a warning.
    assert_eq!(load.classes.len(), 1);
    assert_eq!(load.warnings.len(), 1);
    assert!(load.warnings[0].path.ends_with("Bad.class"));
}

#[test]
fn unrecognized_file_entry_is_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let txt = dir.path().join("notes.txt");
    write(&txt, b"hello");

    let load = load_classpath(&[txt]);
    assert!(load.classes.is_empty());
    assert_eq!(load.warnings.len(), 1);
    assert!(load.warnings[0].message.contains("unrecognized"));
}
