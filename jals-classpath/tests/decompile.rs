use jals_classfile::ClassFile;
use jals_classpath::SkeletonGroup;
use jals_storage::{ArtifactCache, MemoryCache};

fn class(bytes: &[u8]) -> ClassFile {
    ClassFile::read(bytes).expect("parse fixture")
}

fn synthesize(classes: &[ClassFile]) -> (Vec<(String, String)>, Vec<jals_classpath::Warning>) {
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let result = SkeletonGroup::synthesize(&mut cache, classes);
    let sources = result
        .sources
        .into_iter()
        .map(|source| {
            let bytes = cache.lookup(&source.key).unwrap().unwrap();
            (source.path.to_string(), String::from_utf8(bytes).unwrap())
        })
        .collect();
    (sources, result.warnings)
}

#[test]
fn generates_and_reuses_verified_skeleton_artifacts() {
    let classes = [class(include_bytes!("fixtures/Box.class"))];
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let first = SkeletonGroup::synthesize(&mut cache, &classes);
    let second = SkeletonGroup::synthesize(&mut cache, &classes);
    assert_eq!(first.sources, second.sources);
    assert!(first.warnings.is_empty());
    let text = String::from_utf8(cache.lookup(&first.sources[0].key).unwrap().unwrap()).unwrap();
    assert!(text.contains("public class Box<T> {"), "{text}");
    assert!(text.contains("return this.value;"), "{text}");
}

#[test]
fn groups_nested_types_into_their_top_level_source() {
    let classes = [
        class(include_bytes!("fixtures/Outer.class")),
        class(include_bytes!("fixtures/Outer$Inner.class")),
        class(include_bytes!("fixtures/Outer$Color.class")),
    ];
    let (sources, warnings) = synthesize(&classes);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].0, "demo/Outer.java");
    assert!(sources[0].1.contains("public static class Inner"));
    assert!(sources[0].1.contains("public enum Color"));
}

#[test]
fn preserves_rich_members_and_recovered_control_flow() {
    let classes = [
        class(include_bytes!("fixtures/Consts.class")),
        class(include_bytes!("fixtures/Branchy.class")),
        class(include_bytes!("fixtures/Locals.class")),
        class(include_bytes!("fixtures/Loops.class")),
    ];
    let (sources, warnings) = synthesize(&classes);
    assert!(warnings.is_empty(), "{warnings:?}");
    let joined = sources
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("public static final int MAX = 42;"));
    assert!(joined.contains("throw new java.io.IOException(path);"));
    assert!(joined.contains("} else {"));
    assert!(joined.contains("int doubled;"));
    assert!(joined.contains("do {"));
}

#[test]
fn every_generated_fixture_is_valid_java() {
    let bytes: &[&[u8]] = &[
        include_bytes!("fixtures/Box.class"),
        include_bytes!("fixtures/Consts.class"),
        include_bytes!("fixtures/Branchy.class"),
        include_bytes!("fixtures/Locals.class"),
        include_bytes!("fixtures/Loops.class"),
        include_bytes!("fixtures/Arrays.class"),
        include_bytes!("fixtures/Concat.class"),
        include_bytes!("fixtures/Outer.class"),
        include_bytes!("fixtures/Outer$Inner.class"),
        include_bytes!("fixtures/Outer$Color.class"),
    ];
    let classes: Vec<_> = bytes.iter().map(|bytes| class(bytes)).collect();
    let (sources, warnings) = synthesize(&classes);
    assert!(warnings.is_empty(), "{warnings:?}");
    for (path, text) in sources {
        let parse = jals_syntax::Parse::parse(&text);
        assert!(
            parse.errors().is_empty(),
            "{path}: {:#?}\n{text}",
            parse.errors()
        );
    }
}
