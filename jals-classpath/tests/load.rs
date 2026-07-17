use std::io::{Cursor, Write};

use jals_classpath::{ClasspathEntry, ClasspathLoad, WarningOrigin};
use jals_storage::{
    CacheKey, CacheNamespace, CodeTree, ContentDigest, DirKey, Entry, FileKey, MemoryStorage,
};

const BOX_CLASS: &[u8] = include_bytes!("fixtures/Box.class");

fn jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut bytes);
    let options = zip::write::SimpleFileOptions::default();
    for (name, contents) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(contents).unwrap();
    }
    zip.finish().unwrap();
    bytes.into_inner()
}

#[test]
fn loads_typed_project_directory_and_file_entries() {
    let tree = CodeTree::new([
        Entry::File(
            FileKey::parse("classes/Box.class").unwrap(),
            BOX_CLASS.to_vec(),
        ),
        Entry::File(
            FileKey::parse("classes/Bad.class").unwrap(),
            b"bad".to_vec(),
        ),
        Entry::File(
            FileKey::parse("dep.jar").unwrap(),
            jar(&[("pkg/Box.class", BOX_CLASS)]),
        ),
    ])
    .unwrap();
    let storage = MemoryStorage::memory(tree);
    let view = storage.view();
    let load = ClasspathLoad::load(
        &view,
        storage.artifacts(),
        &[
            ClasspathEntry::ProjectDirectory(DirKey::parse("classes").unwrap()),
            ClasspathEntry::ProjectFile(FileKey::parse("dep.jar").unwrap()),
        ],
    );
    assert_eq!(load.classes.len(), 2);
    assert_eq!(load.warnings.len(), 1);
    assert!(matches!(
        load.warnings[0].origin,
        WarningOrigin::ProjectFile(_)
    ));
}

/// `BOX_CLASS` with `minor_version` patched to `minor` — parseable, but distinguishable, so member
/// order stays observable.
fn class_with_minor(minor: u16) -> Vec<u8> {
    let mut bytes = BOX_CLASS.to_vec();
    bytes[4..6].copy_from_slice(&minor.to_be_bytes());
    bytes
}

#[test]
fn jar_members_load_in_archive_order() {
    let (a, b, c) = (
        class_with_minor(1),
        class_with_minor(2),
        class_with_minor(3),
    );
    let bytes = jar(&[
        ("pkg/A.class", &a),
        ("pkg/Broken.class", b"bad"),
        ("pkg/B.class", &b),
        ("notes.txt", b"not a class"),
        ("pkg/C.class", &c),
    ]);

    let tree = CodeTree::new([Entry::File(FileKey::parse("dep.jar").unwrap(), bytes)]).unwrap();
    let storage = MemoryStorage::memory(tree);
    let load = ClasspathLoad::load(
        &storage.view(),
        storage.artifacts(),
        &[ClasspathEntry::ProjectFile(
            FileKey::parse("dep.jar").unwrap(),
        )],
    );
    let minors: Vec<_> = load
        .classes
        .iter()
        .map(|class| class.minor_version)
        .collect();
    assert_eq!(minors, [1, 2, 3]);
    assert_eq!(load.warnings.len(), 1);
}

#[test]
fn loads_verified_cached_jar_and_warns_on_missing_artifact() {
    let mut storage = MemoryStorage::memory(CodeTree::default());
    let bytes = jar(&[("pkg/Box.class", BOX_CLASS)]);
    let key = CacheKey::new(
        CacheNamespace::DependencyJar,
        ContentDigest::of(b"fixture"),
        ContentDigest::of(&bytes),
    );
    storage.artifacts_mut().publish(&key, &bytes).unwrap();
    let missing = CacheKey::new(
        CacheNamespace::DependencyJar,
        ContentDigest::of(b"missing"),
        ContentDigest::of(b"none"),
    );
    let load = ClasspathLoad::load(
        &storage.view(),
        storage.artifacts(),
        &[
            ClasspathEntry::Artifact(key),
            ClasspathEntry::Artifact(missing),
        ],
    );
    assert_eq!(load.classes.len(), 1);
    assert_eq!(load.warnings.len(), 1);
}

/// The disk-backed cache path streams jars through `open_verified` readers; it must yield the
/// same classes in the same order as the in-memory path (with `parallel`, one reader clone per
/// worker).
#[cfg(feature = "native")]
#[test]
fn native_cached_jar_streams_and_matches_the_memory_path() {
    use jals_storage::NativeStorage;

    let (a, b, c) = (
        class_with_minor(4),
        class_with_minor(5),
        class_with_minor(6),
    );
    let bytes = jar(&[
        ("pkg/A.class", &a),
        ("pkg/B.class", &b),
        ("sub/C.class", &c),
    ]);
    let key = CacheKey::new(
        CacheNamespace::DependencyJar,
        ContentDigest::of(b"native-fixture"),
        ContentDigest::of(&bytes),
    );

    let dir = tempfile::tempdir().unwrap();
    let mut native = NativeStorage::native(dir.path(), dir.path().join(".cache")).unwrap();
    native.artifacts_mut().publish(&key, &bytes).unwrap();
    let native_load = ClasspathLoad::load(
        &native.view(),
        native.artifacts(),
        &[ClasspathEntry::Artifact(key.clone())],
    );

    let mut memory = MemoryStorage::memory(CodeTree::default());
    memory.artifacts_mut().publish(&key, &bytes).unwrap();
    let memory_load = ClasspathLoad::load(
        &memory.view(),
        memory.artifacts(),
        &[ClasspathEntry::Artifact(key)],
    );

    let minors = |load: &ClasspathLoad| {
        load.classes
            .iter()
            .map(|class| class.minor_version)
            .collect::<Vec<_>>()
    };
    assert_eq!(minors(&native_load), [4, 5, 6]);
    assert_eq!(minors(&native_load), minors(&memory_load));
    assert!(
        native_load.warnings.is_empty(),
        "{:?}",
        native_load.warnings
    );
}

/// On-disk tampering after publish must surface as a per-entry `Corrupt` warning through the
/// verified reader — never as parsed classes, never as a panic.
#[cfg(feature = "native")]
#[test]
fn native_cached_jar_tampered_on_disk_is_a_warning() {
    use jals_storage::NativeStorage;

    let bytes = jar(&[("pkg/Box.class", BOX_CLASS)]);
    let key = CacheKey::new(
        CacheNamespace::DependencyJar,
        ContentDigest::of(b"tampered-fixture"),
        ContentDigest::of(&bytes),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut native = NativeStorage::native(dir.path(), dir.path().join(".cache")).unwrap();
    native.artifacts_mut().publish(&key, &bytes).unwrap();

    let artifact = native.artifacts().backend().artifact_path(&key);
    std::fs::write(&artifact, jar(&[("pkg/Other.class", BOX_CLASS)])).unwrap();

    let load = ClasspathLoad::load(
        &native.view(),
        native.artifacts(),
        &[ClasspathEntry::Artifact(key)],
    );
    assert!(load.classes.is_empty());
    assert_eq!(load.warnings.len(), 1);
    assert!(
        load.warnings[0].message.contains("Corrupt"),
        "{:?}",
        load.warnings[0]
    );
}
