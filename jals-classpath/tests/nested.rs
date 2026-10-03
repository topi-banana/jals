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
            &jals_progress::Progress::SILENT,
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

/// `member_text` is the value-side sibling of `extract`: what a publisher ships as one text
/// member of a jar — Fabric's tiny file inside its intermediary jar — comes back as a string,
/// and nothing is published to the cache on the way.
#[test]
fn jar_text_reads_one_member_as_utf8() {
    block_on_inline(async {
        let exec = Exec::inline();
        let tiny = "tiny\t2\t0\tofficial\tintermediary\nc\ta\tclass_1\n";
        let fat = jar(&[
            ("mappings/mappings.tiny", tiny.as_bytes()),
            ("data/bad.bin", b"\xff\xfe"),
        ]);
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let root = publish(&mut cache, &fat).await;
        let text =
            jals_classpath::NestedJar::member_text(&exec, &cache, &root, "mappings/mappings.tiny")
                .await
                .expect("the member reads");
        assert_eq!(text, tiny);
        let missing = jals_classpath::NestedJar::member_text(&exec, &cache, &root, "nope.tiny")
            .await
            .unwrap_err();
        assert!(missing.contains("missing"), "{missing}");
        let binary = jals_classpath::NestedJar::member_text(&exec, &cache, &root, "data/bad.bin")
            .await
            .unwrap_err();
        assert!(binary.contains("not UTF-8"), "{binary}");
    });
}
