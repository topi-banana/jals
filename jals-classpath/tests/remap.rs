//! Jar remap / merge / compile-safe decompile smoke tests.

use std::io::{Cursor, Write};

use jals_classfile::ClassFile;
use jals_classpath::{JarMerge, JarRemap, SourceTreeExtraction, SourceTreeLimits};
use jals_exec::{Exec, block_on_inline};
use jals_storage::io::Cursor as SioCursor;
use jals_storage::{
    ArtifactCache, CacheKey, CacheNamespace, ContentDigest, MemoryCache, RelativePath,
};

const fn box_class() -> &'static [u8] {
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/Box.class"
    ))
}

fn write_jar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(&mut cursor);
    for (name, bytes) in entries {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
    cursor.into_inner()
}

async fn publish(cache: &mut ArtifactCache<MemoryCache>, tag: &[u8], bytes: &[u8]) -> CacheKey {
    let key = CacheKey::new(
        CacheNamespace::BuildTaskArtifact,
        ContentDigest::of(tag),
        ContentDigest::of(bytes),
    );
    cache.publish(&key, bytes).await.unwrap();
    key
}

#[test]
fn remap_renames_top_level_class() {
    block_on_inline(async {
        let jar_bytes = write_jar(&[("Box.class", box_class())]);
        let mappings = "\
Renamed -> Box:
    java.lang.Object value -> value
    java.lang.Object get() -> get
    void set(java.lang.Object) -> set
";
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let jar = publish(&mut cache, b"fixture", &jar_bytes).await;
        let remapped = JarRemap::remap(&exec, &mut cache, &jar, mappings)
            .await
            .expect("remap succeeds");
        let bytes = cache
            .lookup(&remapped)
            .await
            .expect("lookup")
            .expect("present");
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("remapped jar is a zip");
        let mut class_member = archive
            .by_name("Renamed.class")
            .expect("class member renamed to official path");
        let mut class_bytes = Vec::new();
        std::io::copy(&mut class_member, &mut class_bytes).unwrap();
        let cf = ClassFile::read(SioCursor::new(class_bytes.as_slice()))
            .await
            .expect("parse remapped class");
        let name = cf
            .constant_pool
            .class_name(cf.this_class)
            .expect("this_class")
            .into_owned();
        assert_eq!(name, "Renamed");
    });
}

#[test]
fn merge_overlay_wins_on_conflict() {
    block_on_inline(async {
        let base = write_jar(&[("a.txt", b"base-a"), ("shared.txt", b"base-shared")]);
        let overlay = write_jar(&[("shared.txt", b"overlay-shared"), ("b.txt", b"overlay-b")]);
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let base_key = publish(&mut cache, b"base", &base).await;
        let overlay_key = publish(&mut cache, b"overlay", &overlay).await;
        let merged = JarMerge::merge(&exec, &mut cache, &base_key, &overlay_key)
            .await
            .expect("merge");
        let bytes = cache
            .lookup(&merged)
            .await
            .expect("lookup")
            .expect("present");
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut names = Vec::new();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            names.push(file.name().to_owned());
            let mut body = Vec::new();
            std::io::copy(&mut file, &mut body).unwrap();
            match file.name() {
                "a.txt" => assert_eq!(body, b"base-a"),
                "shared.txt" => assert_eq!(body, b"overlay-shared"),
                "b.txt" => assert_eq!(body, b"overlay-b"),
                other => panic!("unexpected member {other}"),
            }
        }
        assert_eq!(names, ["a.txt", "shared.txt", "b.txt"]);
    });
}

#[test]
fn decompile_strips_prefix_and_drops_field_final() {
    block_on_inline(async {
        let jar_bytes = write_jar(&[("Box.class", box_class())]);
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let jar = publish(&mut cache, b"fixture", &jar_bytes).await;
        let tree = SourceTreeExtraction::decompile(
            &exec,
            &mut cache,
            &jar,
            &RelativePath::new([]),
            SourceTreeLimits {
                max_files: 100,
                max_file_bytes: 1_048_576,
                max_total_bytes: 4 * 1_048_576,
            },
        )
        .await
        .expect("decompile");
        assert_eq!(tree.files.len(), 1);
        assert_eq!(tree.files[0].path.to_string(), "Box.java");
        let bytes = cache
            .lookup(&tree.files[0].key)
            .await
            .expect("lookup")
            .expect("present");
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(
            text.contains("private T value;") || text.contains("T value;"),
            "{text}"
        );
        assert!(!text.contains("final T value"), "{text}");
        let parsed = jals_syntax::Parse::parse(&text).await;
        assert!(
            parsed.errors().is_empty(),
            "syntax errors: {:?}",
            parsed.errors()
        );
    });
}
