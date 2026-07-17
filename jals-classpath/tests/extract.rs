use std::io::{Cursor, Write};

use jals_classpath::{JarExtraction, LibrarySource};
use jals_storage::{ArtifactCache, CacheKey, CacheNamespace, ContentDigest, MemoryCache};

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

fn publish(cache: &mut ArtifactCache<MemoryCache>, bytes: &[u8]) -> CacheKey {
    let key = CacheKey::new(
        CacheNamespace::DependencyJar,
        ContentDigest::of(b"sources"),
        ContentDigest::of(bytes),
    );
    cache.publish(&key, bytes).unwrap();
    key
}

#[test]
fn extracts_only_safe_java_members_as_verified_artifacts() {
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let bytes = sources_jar(&[
        ("java/util/List.java", b"interface List {}"),
        ("META-INF/MANIFEST.MF", b"manifest"),
        ("../Escape.java", b"bad"),
    ]);
    let jar = publish(&mut cache, &bytes);
    let extraction = JarExtraction::<LibrarySource>::sources(&mut cache, &[jar]);
    assert_eq!(extraction.artifacts.len(), 1);
    assert!(
        extraction.artifacts[0]
            .path
            .to_string()
            .ends_with("java/util/List.java")
    );
    assert_eq!(
        cache.lookup(&extraction.artifacts[0].key).unwrap().unwrap(),
        b"interface List {}"
    );
    assert_eq!(extraction.warnings.len(), 1);
}

#[test]
fn extraction_is_idempotent_and_corruption_is_advisory() {
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let bytes = sources_jar(&[("a/B.java", b"class B {}")]);
    let jar = publish(&mut cache, &bytes);
    let first = JarExtraction::<LibrarySource>::sources(&mut cache, std::slice::from_ref(&jar));
    let second = JarExtraction::<LibrarySource>::sources(&mut cache, &[jar]);
    assert_eq!(first.artifacts, second.artifacts);

    let bogus = publish(&mut cache, b"not a zip");
    let corrupt = JarExtraction::<LibrarySource>::sources(&mut cache, &[bogus]);
    assert!(corrupt.artifacts.is_empty());
    assert_eq!(corrupt.warnings.len(), 1);
}
