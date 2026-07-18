use std::io::{Cursor, Write};

use jals_classpath::{CachedJar, ClasspathEntry, ClasspathLoad, JarExtraction};
use jals_exec::{Exec, block_on_inline};
use jals_storage::{
    ArtifactCache, CacheKey, CacheNamespace, CodeTree, ContentDigest, MemoryCache, MemoryStorage,
};

const BOX_CLASS: &[u8] = include_bytes!("fixtures/Box.class");

fn jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut bytes);
    for (name, content) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(content).unwrap();
    }
    zip.finish().unwrap();
    bytes.into_inner()
}

async fn publish(cache: &mut ArtifactCache<MemoryCache>, bytes: &[u8]) -> CacheKey {
    let key = CacheKey::new(
        CacheNamespace::DependencyJar,
        ContentDigest::of(b"fat"),
        ContentDigest::of(bytes),
    );
    cache.publish(&key, bytes).await.unwrap();
    key
}

#[test]
fn recursively_extracts_and_loads_nested_jars() {
    block_on_inline(async {
        let exec = Exec::inline();
        let leaf = jar(&[("pkg/Box.class", BOX_CLASS)]);
        let middle = jar(&[("lib/leaf.jar", &leaf)]);
        let fat = jar(&[("BOOT-INF/lib/middle.jar", &middle)]);
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let root = publish(&mut cache, &fat).await;
        let extraction = JarExtraction::<CachedJar>::nested(&exec, &mut cache, &root).await;
        assert_eq!(extraction.artifacts.len(), 2);
        assert!(extraction.warnings.is_empty(), "{:?}", extraction.warnings);

        let leaf = extraction
            .artifacts
            .iter()
            .find(|jar| jar.member.to_string().ends_with("leaf.jar"))
            .unwrap();
        let storage = MemoryStorage::memory(CodeTree::default());
        let load = ClasspathLoad::load(
            &exec,
            &storage.view(),
            &cache,
            &[ClasspathEntry::Artifact(leaf.key.clone())],
        )
        .await;
        assert_eq!(load.classes.len(), 1);
    });
}

#[test]
fn corrupt_nested_jar_is_published_but_diagnosed_on_recursion() {
    block_on_inline(async {
        let exec = Exec::inline();
        let fat = jar(&[("lib/bad.jar", b"not a zip")]);
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let root = publish(&mut cache, &fat).await;
        let extraction = JarExtraction::<CachedJar>::nested(&exec, &mut cache, &root).await;
        assert_eq!(extraction.artifacts.len(), 1);
        assert_eq!(extraction.warnings.len(), 1);
    });
}
