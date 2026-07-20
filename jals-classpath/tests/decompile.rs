use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::process::Command;

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

fn hierarchy_evolution_v1() -> [ClassFile; 6] {
    [
        class(include_bytes!(
            "fixtures/hierarchy-evolution/v1/evolution/HierarchyEvolution.class"
        )),
        class(include_bytes!(
            "fixtures/hierarchy-evolution/v1/evolution/HierarchyBase.class"
        )),
        class(include_bytes!(
            "fixtures/hierarchy-evolution/v1/evolution/HierarchyDirect.class"
        )),
        class(include_bytes!(
            "fixtures/hierarchy-evolution/v1/evolution/HierarchyRoot.class"
        )),
        class(include_bytes!(
            "fixtures/hierarchy-evolution/v1/evolution/HierarchyLeft.class"
        )),
        class(include_bytes!(
            "fixtures/hierarchy-evolution/v1/evolution/HierarchyRight.class"
        )),
    ]
}

fn hierarchy_evolution_mixed() -> [ClassFile; 6] {
    let [client, _, direct, root, left, _] = hierarchy_evolution_v1();
    [
        client,
        class(include_bytes!(
            "fixtures/hierarchy-evolution/v2/evolution/HierarchyBase.class"
        )),
        direct,
        root,
        left,
        class(include_bytes!(
            "fixtures/hierarchy-evolution/v2/evolution/HierarchyRight.class"
        )),
    ]
}

fn generated_source<'a>(sources: &'a [(String, String)], path: &str) -> &'a str {
    &sources
        .iter()
        .find(|(candidate, _)| candidate == path)
        .unwrap_or_else(|| panic!("missing generated source {path}"))
        .1
}

fn assert_javac_accepts(source: &str, support: &[(&str, &[u8])]) {
    let temp = tempfile::tempdir().expect("temp dir");
    let package = temp.path().join("classes/evolution");
    let output = temp.path().join("output");
    let source_dir = temp.path().join("source/evolution");
    fs::create_dir_all(&package).expect("support package dir");
    fs::create_dir_all(&output).expect("javac output dir");
    fs::create_dir_all(&source_dir).expect("source package dir");
    for (name, bytes) in support {
        fs::write(package.join(name), bytes).expect("write support class");
    }
    let source_path = source_dir.join("HierarchyEvolution.java");
    fs::write(&source_path, source).expect("write generated source");

    let javac = std::env::var_os("JAVAC").unwrap_or_else(|| OsString::from("javac"));
    let result = Command::new(javac)
        .arg("-cp")
        .arg(temp.path().join("classes"))
        .arg("-d")
        .arg(&output)
        .arg(&source_path)
        .output();
    let output = match result {
        Ok(output) => output,
        Err(error) if error.kind() == ErrorKind::NotFound => return,
        Err(error) => panic!("failed to run javac: {error}"),
    };
    assert!(
        output.status.success(),
        "javac rejected generated source\nstdout:\n{}\nstderr:\n{}\nsource:\n{source}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
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
    let classes = [
        class(include_bytes!("fixtures/InvokeSpecialCalls.class")),
        class(include_bytes!("fixtures/InvokeSpecialBase.class")),
        class(include_bytes!("fixtures/InvokeSpecialDefault.class")),
    ];
    let (sources, warnings) = synthesize(&classes);
    assert!(warnings.is_empty(), "{warnings:?}");
    let text = &sources
        .iter()
        .find(|(path, _)| path == "demo/InvokeSpecialCalls.java")
        .expect("client skeleton")
        .1;
    assert!(text.contains("return super.classValue(value);"), "{text}");
    assert!(
        text.contains("return demo.InvokeSpecialDefault.super.interfaceValue(value);"),
        "{text}"
    );
}

#[test]
fn hierarchy_evolution_uses_safe_interface_super_fallbacks() {
    let (v1_sources, warnings) = synthesize(&hierarchy_evolution_v1());
    assert!(warnings.is_empty(), "{warnings:?}");
    let v1 = generated_source(&v1_sources, "evolution/HierarchyEvolution.java");
    assert!(
        v1.contains("return evolution.HierarchyDirect.super.directValue(value);"),
        "{v1}"
    );
    assert!(
        v1.contains("return evolution.HierarchyLeft.super.rootValue(value);"),
        "{v1}"
    );
    assert_javac_accepts(
        v1,
        &[
            (
                "HierarchyBase.class",
                include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyBase.class"),
            ),
            (
                "HierarchyDirect.class",
                include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyDirect.class"),
            ),
            (
                "HierarchyRoot.class",
                include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyRoot.class"),
            ),
            (
                "HierarchyLeft.class",
                include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyLeft.class"),
            ),
            (
                "HierarchyRight.class",
                include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyRight.class"),
            ),
        ],
    );

    let (mixed_sources, warnings) = synthesize(&hierarchy_evolution_mixed());
    assert!(warnings.is_empty(), "{warnings:?}");
    let mixed = generated_source(&mixed_sources, "evolution/HierarchyEvolution.java");
    assert!(!mixed.contains("HierarchyDirect.super"), "{mixed}");
    assert!(!mixed.contains("HierarchyLeft.super"), "{mixed}");
    assert!(
        mixed.contains("public int callDirect(int value) { throw new RuntimeException(); }"),
        "{mixed}"
    );
    assert!(
        mixed.contains("public int callLeft(int value) { throw new RuntimeException(); }"),
        "{mixed}"
    );
    assert_javac_accepts(
        mixed,
        &[
            (
                "HierarchyBase.class",
                include_bytes!("fixtures/hierarchy-evolution/v2/evolution/HierarchyBase.class"),
            ),
            (
                "HierarchyDirect.class",
                include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyDirect.class"),
            ),
            (
                "HierarchyRoot.class",
                include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyRoot.class"),
            ),
            (
                "HierarchyLeft.class",
                include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyLeft.class"),
            ),
            (
                "HierarchyRight.class",
                include_bytes!("fixtures/hierarchy-evolution/v2/evolution/HierarchyRight.class"),
            ),
        ],
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
        include_bytes!("fixtures/InvokeSpecialBase.class"),
        include_bytes!("fixtures/InvokeSpecialDefault.class"),
        include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyEvolution.class"),
        include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyBase.class"),
        include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyDirect.class"),
        include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyRoot.class"),
        include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyLeft.class"),
        include_bytes!("fixtures/hierarchy-evolution/v1/evolution/HierarchyRight.class"),
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
