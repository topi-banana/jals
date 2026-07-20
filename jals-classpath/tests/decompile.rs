use jals_classfile::ClassFile;
use jals_classpath::SkeletonGroup;
use jals_exec::block_on_inline;
use jals_storage::{ArtifactCache, MemoryCache};

fn class(bytes: &[u8]) -> ClassFile {
    block_on_inline(ClassFile::read(bytes)).expect("parse fixture")
}

fn synthesize(classes: &[ClassFile]) -> (Vec<(String, String)>, Vec<jals_classpath::Warning>) {
    block_on_inline(async {
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let result = SkeletonGroup::synthesize(&mut cache, classes).await;
        let mut sources = Vec::new();
        for source in result.sources {
            let bytes = cache.lookup(&source.key).await.unwrap().unwrap();
            sources.push((source.path.to_string(), String::from_utf8(bytes).unwrap()));
        }
        (sources, result.warnings)
    })
}

#[test]
fn generates_and_reuses_verified_skeleton_artifacts() {
    block_on_inline(async {
        let classes = [class(include_bytes!("fixtures/Box.class"))];
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let first = SkeletonGroup::synthesize(&mut cache, &classes).await;
        let second = SkeletonGroup::synthesize(&mut cache, &classes).await;
        assert_eq!(first.sources, second.sources);
        assert!(first.warnings.is_empty());
        let text =
            String::from_utf8(cache.lookup(&first.sources[0].key).await.unwrap().unwrap()).unwrap();
        assert!(text.contains("public class Box<T> {"), "{text}");
        assert!(text.contains("return this.value;"), "{text}");
    });
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
fn preserves_int_carried_boolean_and_char_values() {
    let classes = [class(include_bytes!("fixtures/IntCarried.class"))];
    let (sources, warnings) = synthesize(&classes);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].0, "demo/IntCarried.java");
    let text = &sources[0].1;
    for expected in [
        "public static final char CONSTANT_CHAR = 'G';",
        "private boolean flag;",
        "private char letter;",
        "return true;",
        "value = 'B';",
        "this.flag = true;",
        "return this.passChar('D');",
        "return this.charOrInt((int) value);",
        "return \"\" + (int) value;",
        "return new char[]{'E', (char) 55296};",
        "if (value == 0) {",
        "if (!value) {",
        "return (char) value;",
        "return (char) 55296;",
    ] {
        assert!(text.contains(expected), "{text}");
    }
}

#[test]
fn preserves_non_virtual_invokespecial_dispatch() {
    let classes = [class(include_bytes!("fixtures/InvokeSpecialCalls.class"))];
    let (sources, warnings) = synthesize(&classes);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].0, "demo/InvokeSpecialCalls.java");
    let text = &sources[0].1;
    assert!(text.contains("return super.classValue(value);"), "{text}");
    assert!(
        text.contains("return demo.InvokeSpecialDefault.super.interfaceValue(value);"),
        "{text}"
    );
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
        include_bytes!("fixtures/Sb.class"),
        include_bytes!("fixtures/Cmp.class"),
        include_bytes!("fixtures/IntCarried.class"),
        include_bytes!("fixtures/InvokeSpecialCalls.class"),
        include_bytes!("fixtures/Outer.class"),
        include_bytes!("fixtures/Outer$Inner.class"),
        include_bytes!("fixtures/Outer$Color.class"),
    ];
    let classes: Vec<_> = bytes.iter().map(|bytes| class(bytes)).collect();
    let (sources, warnings) = synthesize(&classes);
    assert!(warnings.is_empty(), "{warnings:?}");
    for (path, text) in sources {
        let parse = block_on_inline(jals_syntax::Parse::parse(&text));
        assert!(
            parse.errors().is_empty(),
            "{path}: {:#?}\n{text}",
            parse.errors()
        );
    }
}
