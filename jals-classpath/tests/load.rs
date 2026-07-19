use std::io::{Cursor, Write};

use jals_classpath::{ClasspathEntry, ClasspathLoad, WarningOrigin};
use jals_exec::{Exec, block_on_inline};
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
    let load = block_on_inline(ClasspathLoad::load(
        &Exec::inline(),
        &view,
        storage.artifacts(),
        &[
            ClasspathEntry::ProjectDirectory(DirKey::parse("classes").unwrap()),
            ClasspathEntry::ProjectFile(FileKey::parse("dep.jar").unwrap()),
        ],
    ));
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
    let load = block_on_inline(ClasspathLoad::load(
        &Exec::inline(),
        &storage.view(),
        storage.artifacts(),
        &[ClasspathEntry::ProjectFile(
            FileKey::parse("dep.jar").unwrap(),
        )],
    ));
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
    block_on_inline(async {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        let bytes = jar(&[("pkg/Box.class", BOX_CLASS)]);
        let key = CacheKey::new(
            CacheNamespace::DependencyJar,
            ContentDigest::of(b"fixture"),
            ContentDigest::of(&bytes),
        );
        storage.artifacts_mut().publish(&key, &bytes).await.unwrap();
        let missing = CacheKey::new(
            CacheNamespace::DependencyJar,
            ContentDigest::of(b"missing"),
            ContentDigest::of(b"none"),
        );
        let load = ClasspathLoad::load(
            &Exec::inline(),
            &storage.view(),
            storage.artifacts(),
            &[
                ClasspathEntry::Artifact(key),
                ClasspathEntry::Artifact(missing),
            ],
        )
        .await;
        assert_eq!(load.classes.len(), 1);
        assert_eq!(load.warnings.len(), 1);
    });
}

/// One jar with several chunks' worth of members (some broken), a directory, and a loose file:
/// the multi-worker tokio fan-out must produce byte-identical classes *and* warnings, in the
/// same order, as the inline executor. The chunk split is a fixed constant, so this holds at
/// any parallelism.
#[test]
fn load_is_deterministic_across_inline_and_multi_worker_executors() {
    let members: Vec<(String, Vec<u8>)> = (0..200u16)
        .map(|n| {
            if n % 17 == 0 {
                (format!("pkg/broken{n:03}.class"), b"not a class".to_vec())
            } else {
                (format!("pkg/c{n:03}.class"), class_with_minor(n))
            }
        })
        .collect();
    let entries: Vec<(&str, &[u8])> = members
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let tree = CodeTree::new([
        Entry::File(FileKey::parse("dep.jar").unwrap(), jar(&entries)),
        Entry::File(
            FileKey::parse("classes/Box.class").unwrap(),
            BOX_CLASS.to_vec(),
        ),
        Entry::File(
            FileKey::parse("classes/Bad.class").unwrap(),
            b"bad".to_vec(),
        ),
    ])
    .unwrap();
    let classpath = [
        ClasspathEntry::ProjectFile(FileKey::parse("dep.jar").unwrap()),
        ClasspathEntry::ProjectDirectory(DirKey::parse("classes").unwrap()),
    ];

    let storage = MemoryStorage::memory(tree.clone());
    let inline_load = block_on_inline(ClasspathLoad::load(
        &Exec::inline(),
        &storage.view(),
        storage.artifacts(),
        &classpath,
    ));

    let parallel_load = jals_exec::tokio_rt::run(|exec| async move {
        let storage = MemoryStorage::memory(tree);
        ClasspathLoad::load(&exec, &storage.view(), storage.artifacts(), &classpath).await
    })
    .expect("test runtime bootstraps");

    let minors = |load: &ClasspathLoad| -> Vec<u16> {
        load.classes.iter().map(|c| c.minor_version).collect()
    };
    assert_eq!(minors(&inline_load), minors(&parallel_load));
    assert_eq!(inline_load.warnings, parallel_load.warnings);
    // Sanity: both broke the same 12 jar members plus the one loose bad file.
    assert_eq!(inline_load.warnings.len(), 13);
}

/// The disk-backed cache path streams jars through `open_verified` readers; it must yield the
/// same classes in the same order as the in-memory path (workers clone one reader each).
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

    let (native_load, memory_load) = jals_exec::tokio_rt::run(|exec| async move {
        let dir = tempfile::tempdir().unwrap();
        let mut native = NativeStorage::native(dir.path(), dir.path().join(".cache"), exec.clone())
            .await
            .unwrap();
        native.artifacts_mut().publish(&key, &bytes).await.unwrap();
        let native_load = ClasspathLoad::load(
            &exec,
            &native.view(),
            native.artifacts(),
            &[ClasspathEntry::Artifact(key.clone())],
        )
        .await;

        let mut memory = MemoryStorage::memory(CodeTree::default());
        memory.artifacts_mut().publish(&key, &bytes).await.unwrap();
        let memory_load = ClasspathLoad::load(
            &exec,
            &memory.view(),
            memory.artifacts(),
            &[ClasspathEntry::Artifact(key)],
        )
        .await;
        (native_load, memory_load)
    })
    .expect("test runtime bootstraps");

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
    let tampered = jar(&[("pkg/Other.class", BOX_CLASS)]);
    let key = CacheKey::new(
        CacheNamespace::DependencyJar,
        ContentDigest::of(b"tampered-fixture"),
        ContentDigest::of(&bytes),
    );
    let load = jals_exec::tokio_rt::run(|exec| async move {
        let dir = tempfile::tempdir().unwrap();
        let mut native = NativeStorage::native(dir.path(), dir.path().join(".cache"), exec.clone())
            .await
            .unwrap();
        native.artifacts_mut().publish(&key, &bytes).await.unwrap();

        let artifact = native.artifacts().backend().artifact_path(&key);
        std::fs::write(&artifact, tampered).unwrap();

        ClasspathLoad::load(
            &exec,
            &native.view(),
            native.artifacts(),
            &[ClasspathEntry::Artifact(key)],
        )
        .await
    })
    .expect("test runtime bootstraps");
    assert!(load.classes.is_empty());
    assert_eq!(load.warnings.len(), 1);
    assert!(
        load.warnings[0].message.contains("Corrupt"),
        "{:?}",
        load.warnings[0]
    );
}
