use std::io::{Cursor, Write};

use jals_classpath::{JarExtraction, LibrarySource, SourceTreeExtraction, SourceTreeLimits};
use jals_exec::{Exec, block_on_inline};
use jals_storage::{
    ArtifactCache, CacheKey, CacheNamespace, ContentDigest, MemoryCache, RelativePath,
};

fn sources_jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
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
        ContentDigest::of(b"sources"),
        ContentDigest::of(bytes),
    );
    cache.publish(&key, bytes).await.unwrap();
    key
}

#[test]
fn extracts_only_safe_java_members_as_verified_artifacts() {
    block_on_inline(async {
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let bytes = sources_jar(&[
            ("java/util/List.java", b"interface List {}"),
            ("META-INF/MANIFEST.MF", b"manifest"),
            ("../Escape.java", b"bad"),
        ]);
        let jar = publish(&mut cache, &bytes).await;
        let extraction = JarExtraction::<LibrarySource>::sources(&exec, &mut cache, &[jar]).await;
        assert_eq!(extraction.artifacts.len(), 1);
        assert!(
            extraction.artifacts[0]
                .path
                .to_string()
                .ends_with("java/util/List.java")
        );
        assert_eq!(
            cache
                .lookup(&extraction.artifacts[0].key)
                .await
                .unwrap()
                .unwrap(),
            b"interface List {}"
        );
        assert_eq!(extraction.warnings.len(), 1);
    });
}

#[test]
fn extraction_is_idempotent_and_corruption_is_advisory() {
    block_on_inline(async {
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let bytes = sources_jar(&[("a/B.java", b"class B {}")]);
        let jar = publish(&mut cache, &bytes).await;
        let first =
            JarExtraction::<LibrarySource>::sources(&exec, &mut cache, std::slice::from_ref(&jar))
                .await;
        let second = JarExtraction::<LibrarySource>::sources(&exec, &mut cache, &[jar]).await;
        assert_eq!(first.artifacts, second.artifacts);

        let bogus = publish(&mut cache, b"not a zip").await;
        let corrupt = JarExtraction::<LibrarySource>::sources(&exec, &mut cache, &[bogus]).await;
        assert!(corrupt.artifacts.is_empty());
        assert_eq!(corrupt.warnings.len(), 1);
    });
}

#[test]
fn task_source_tree_strips_the_prefix_and_rejects_the_whole_unsafe_archive() {
    block_on_inline(async {
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let bytes = sources_jar(&[
            ("net/example/A.java", b"class A {}"),
            ("net/example/nested/B.java", b"class B {}"),
            ("other/Ignored.java", b"class Ignored {}"),
        ]);
        let jar = publish(&mut cache, &bytes).await;
        let tree = SourceTreeExtraction::java(
            &exec,
            &mut cache,
            &jar,
            &RelativePath::parse("net/example").unwrap(),
            SourceTreeLimits {
                max_files: 10,
                max_file_bytes: 1024,
                max_total_bytes: 4096,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            tree.files
                .iter()
                .map(|file| file.path.to_string())
                .collect::<Vec<_>>(),
            ["A.java", "nested/B.java"]
        );
        let error = SourceTreeExtraction::java(
            &exec,
            &mut cache,
            &jar,
            &RelativePath::parse("net/example").unwrap(),
            SourceTreeLimits {
                max_files: 1,
                max_file_bytes: 1024,
                max_total_bytes: 4096,
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("matching members"), "{error}");

        let unsafe_bytes = sources_jar(&[("../Escape.java", b"bad")]);
        let unsafe_jar = publish(&mut cache, &unsafe_bytes).await;
        let error = SourceTreeExtraction::java(
            &exec,
            &mut cache,
            &unsafe_jar,
            &RelativePath::ROOT,
            SourceTreeLimits {
                max_files: 10,
                max_file_bytes: 1024,
                max_total_bytes: 4096,
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("unsafe Java archive member"), "{error}");
    });
}
