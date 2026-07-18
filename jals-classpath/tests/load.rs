use std::io::{Cursor, Write};

use jals_classpath::{ClasspathEntry, ClasspathLoad, WarningOrigin};
use jals_storage::{
    CacheKey, CacheNamespace, CodeTree, ContentDigest, DirKey, Entry, FileKey, MemoryStorage,
};

const BOX_CLASS: &[u8] = include_bytes!("fixtures/Box.class");

fn jar_bytes() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut bytes);
    zip.start_file("pkg/Box.class", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(BOX_CLASS).unwrap();
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
        Entry::File(FileKey::parse("dep.jar").unwrap(), jar_bytes()),
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

#[test]
fn loads_verified_cached_jar_and_warns_on_missing_artifact() {
    let mut storage = MemoryStorage::memory(CodeTree::default());
    let bytes = jar_bytes();
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
