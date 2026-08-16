//! Jar remap / merge / compile-safe decompile smoke tests.

use std::io::{Cursor, Write};

use jals_classfile::ClassFile;
use jals_classpath::{
    JarMerge, JarRemap, MappingFormat, RemapDirection, RemapRequest, SourceTreeExtraction,
    SourceTreeLimits,
};
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

/// A deobfuscating request with no extra hierarchy — what a self-contained library jar needs.
const fn deobfuscate(mappings: &str) -> RemapRequest<'_> {
    RemapRequest {
        mappings,
        format: MappingFormat::Proguard,
        direction: RemapDirection::Deobfuscate,
        hierarchy: &[],
    }
}

/// The same, reading tiny v2 text through one pair of its namespaces.
fn deobfuscate_tiny<'a>(mappings: &'a str, from: &str, to: &str) -> RemapRequest<'a> {
    RemapRequest {
        mappings,
        format: MappingFormat::TinyV2 {
            from: from.to_owned(),
            to: to.to_owned(),
        },
        direction: RemapDirection::Deobfuscate,
        hierarchy: &[],
    }
}

/// The fixture's three namespaces: what the jar ships as, and two renamings of it.
const BOX_TINY: &str = "\
tiny\t2\t0\tofficial\tintermediary\tnamed
c\tBox\tclass_1\tRenamed
\tf\tLjava/lang/Object;\tvalue\tfield_1\tvalue
\tm\t()Ljava/lang/Object;\tget\tmethod_1\tget
\tm\t(Ljava/lang/Object;)V\tset\tmethod_2\tset
";

/// The name `this_class` carries in the single class member of a remapped jar.
async fn remapped_class_name(cache: &ArtifactCache<MemoryCache>, key: &CacheKey) -> String {
    let bytes = cache.lookup(key).await.expect("lookup").expect("present");
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("remapped jar is a zip");
    let name = archive.file_names().next().expect("one member").to_owned();
    let mut member = archive.by_name(&name).expect("member");
    let mut class_bytes = Vec::new();
    std::io::copy(&mut member, &mut class_bytes).unwrap();
    let cf = ClassFile::read(SioCursor::new(class_bytes.as_slice()))
        .await
        .expect("parse remapped class");
    cf.constant_pool
        .class_name(cf.this_class)
        .expect("this_class")
        .into_owned()
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
        let remapped = JarRemap::remap(&exec, &mut cache, &jar, &deobfuscate(mappings))
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
fn tiny_v2_remaps_the_same_jar_the_proguard_text_does() {
    block_on_inline(async {
        let jar_bytes = write_jar(&[("Box.class", box_class())]);
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let jar = publish(&mut cache, b"fixture", &jar_bytes).await;
        let remapped = JarRemap::remap(
            &exec,
            &mut cache,
            &jar,
            &deobfuscate_tiny(BOX_TINY, "official", "named"),
        )
        .await
        .expect("remap succeeds");
        assert_eq!(remapped_class_name(&cache, &remapped).await, "Renamed");
    });
}

#[test]
fn the_namespace_pair_a_tiny_file_is_read_through_is_part_of_the_cache_key() {
    // One jar, one mapping text, two namespace pairs — two different jars. The identity a remap is
    // published under is its *provenance*, and the pair has to reach it: were it folded no further
    // than the format tag, these two derivations would share a provenance, and the cache's locator
    // index (which recovers content from provenance, last-writer-wins) would hand one run the other
    // run's jar. Compared as provenances rather than whole keys on purpose — the content halves
    // differ whether or not the fold is right, so comparing keys would pass either way.
    block_on_inline(async {
        let jar_bytes = write_jar(&[("Box.class", box_class())]);
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let jar = publish(&mut cache, b"fixture", &jar_bytes).await;

        let named = JarRemap::remap(
            &exec,
            &mut cache,
            &jar,
            &deobfuscate_tiny(BOX_TINY, "official", "named"),
        )
        .await
        .expect("remap succeeds");
        let intermediary = JarRemap::remap(
            &exec,
            &mut cache,
            &jar,
            &deobfuscate_tiny(BOX_TINY, "official", "intermediary"),
        )
        .await
        .expect("remap succeeds");

        assert_ne!(
            named.provenance(),
            intermediary.provenance(),
            "two namespace pairs over one mapping text are two derivations"
        );
        assert_eq!(remapped_class_name(&cache, &named).await, "Renamed");
        assert_eq!(remapped_class_name(&cache, &intermediary).await, "class_1");
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

fn hierarchy_class(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hierarchy-evolution/v1/evolution")
        .join(format!("{name}.class"));
    std::fs::read(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

/// Whether `haystack` contains `needle` as a raw byte run — how a `Utf8` constant is stored.
fn contains_bytes(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn member_bytes(jar: &[u8], name: &str) -> Vec<u8> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(jar.to_vec())).expect("remapped jar is a zip");
    let mut member = archive.by_name(name).expect("member is present");
    let mut bytes = Vec::new();
    std::io::copy(&mut member, &mut bytes).unwrap();
    bytes
}

#[test]
fn an_inherited_member_needs_the_jar_that_declares_it() {
    block_on_inline(async {
        // `HierarchyEvolution` calls `HierarchyLeft.rootValue`, but `rootValue` is declared on
        // `HierarchyRoot` — `HierarchyLeft` only extends it. Resolving that rename means walking
        // from the reference's owner up to the declaration, and the types on that path live in a
        // different archive.
        //
        // This is the failure mode the `hierarchy` field exists for, and it is a *silent* one: the
        // remap succeeds either way and produces a jar whose call site still says `rootValue` while
        // every declaration around it says the new name.
        let mappings = "\
evolution.RenamedRoot -> evolution.HierarchyRoot:
    int renamedRootValue(int) -> rootValue
";
        let subject = write_jar(&[(
            "evolution/HierarchyEvolution.class",
            &hierarchy_class("HierarchyEvolution"),
        )]);
        let supertypes = write_jar(&[
            (
                "evolution/HierarchyLeft.class",
                &hierarchy_class("HierarchyLeft"),
            ),
            (
                "evolution/HierarchyRoot.class",
                &hierarchy_class("HierarchyRoot"),
            ),
        ]);

        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let jar = publish(&mut cache, b"subject", &subject).await;
        let supers = publish(&mut cache, b"supertypes", &supertypes).await;

        let alone = JarRemap::remap(&exec, &mut cache, &jar, &deobfuscate(mappings))
            .await
            .expect("remap succeeds");
        let alone = member_bytes(
            &cache.lookup(&alone).await.unwrap().unwrap(),
            "evolution/HierarchyEvolution.class",
        );
        assert!(
            !contains_bytes(&alone, "renamedRootValue"),
            "without the declaring jar the walk cannot reach `HierarchyRoot`"
        );

        let supers = [supers];
        let with_supers = JarRemap::remap(
            &exec,
            &mut cache,
            &jar,
            &RemapRequest {
                hierarchy: &supers,
                ..deobfuscate(mappings)
            },
        )
        .await
        .expect("remap succeeds");
        let with_supers = member_bytes(
            &cache.lookup(&with_supers).await.unwrap().unwrap(),
            "evolution/HierarchyEvolution.class",
        );
        assert!(
            contains_bytes(&with_supers, "renamedRootValue"),
            "the inherited member resolves once its declaring type is in the index"
        );
    });
}

#[test]
fn reobfuscating_a_remapped_jar_restores_its_original_names() {
    block_on_inline(async {
        // What makes a `[build] remap` trustworthy: the jar a runtime loads carries the names it
        // expects. Checked as a round trip because that is the property, and because a one-way
        // assertion passes just as well when both directions are wrong the same way.
        let mappings = "\
Renamed -> Box:
    java.lang.Object value -> value
    java.lang.Object get() -> get
    void set(java.lang.Object) -> set
";
        let exec = Exec::inline();
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let jar = publish(
            &mut cache,
            b"fixture",
            &write_jar(&[("Box.class", box_class())]),
        )
        .await;

        let deobf = JarRemap::remap(&exec, &mut cache, &jar, &deobfuscate(mappings))
            .await
            .expect("deobfuscate succeeds");
        let reobf = JarRemap::remap(
            &exec,
            &mut cache,
            &deobf,
            &RemapRequest {
                direction: RemapDirection::Reobfuscate,
                ..deobfuscate(mappings)
            },
        )
        .await
        .expect("reobfuscate succeeds");

        let bytes = cache.lookup(&reobf).await.unwrap().unwrap();
        let cf = ClassFile::read(SioCursor::new(member_bytes(&bytes, "Box.class").as_slice()))
            .await
            .expect("parse the reobfuscated class");
        assert_eq!(
            cf.constant_pool
                .class_name(cf.this_class)
                .expect("this_class")
                .into_owned(),
            "Box"
        );
    });
}
