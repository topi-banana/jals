//! Snapshot tests for project-wide (cross-file) type-name resolution.
//!
//! Each file's type-name references are rendered one per line as
//! `fileN: name -> <target>`, where the target is the file-local definition, the project type
//! (with its FQN and file), `external`, or `unresolved`.

use core::fmt::Write;

use expect_test::{Expect, expect};
use jals_hir::{FileId, Namespace, ProjectIndex, Resolution, Resolved, TypeResolution};
use jals_syntax::SyntaxNode;

/// Parses each source and keeps its `SOURCE_FILE` node alive (rowan nodes are ref-counted, so the
/// parse may be dropped). Returns `(FileId, node)` pairs in input order.
fn nodes(sources: &[&str]) -> Vec<(FileId, SyntaxNode)> {
    sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                FileId(u32::try_from(i).unwrap()),
                jals_syntax::Parse::parse(s).syntax(),
            )
        })
        .collect()
}

fn render(sources: &[&str]) -> String {
    let nodes = nodes(sources);
    let index = ProjectIndex::builder(&nodes).build();
    let mut out = String::new();
    for (file, root) in &nodes {
        let resolved = Resolved::resolve_node(root);
        for r in &resolved.references {
            if r.namespace != Namespace::Type {
                continue;
            }
            let target = match r.resolution {
                Resolution::Def(id) => {
                    let d = resolved.def(id);
                    format!("local {:?} `{}`", d.kind, d.name)
                }
                Resolution::Unresolved => {
                    let tr = index.resolve_reference(*file, r);
                    match tr {
                        TypeResolution::Project(id) => {
                            let it = index.item(id);
                            format!("project `{}` @file{}", it.fqn, it.file.0)
                        }
                        TypeResolution::External => "external".to_owned(),
                        TypeResolution::Unresolved => "unresolved".to_owned(),
                    }
                }
            };
            writeln!(out, "file{}: {} -> {}", file.0, r.name, target).unwrap();
        }
    }
    out
}

#[allow(clippy::needless_pass_by_value)]
fn check(sources: &[&str], expected: Expect) {
    expected.assert_eq(&render(sources));
}

#[test]
fn same_package_type_resolves_across_files() {
    check(
        &[
            "package a; class Foo { }",
            "package a; class Bar { Foo f; }",
        ],
        expect![[r"
            file1: Foo -> project `a.Foo` @file0
        "]],
    );
}

#[test]
fn single_type_import_resolves_to_project() {
    check(
        &[
            "package a.b; class Foo { }",
            "package x; import a.b.Foo; class Bar { Foo f; }",
        ],
        expect![[r"
            file1: Foo -> project `a.b.Foo` @file0
        "]],
    );
}

#[test]
fn on_demand_import_resolves_to_project() {
    check(
        &[
            "package a.b; class Foo { }",
            "package x; import a.b.*; class Bar { Foo f; }",
        ],
        expect![[r"
            file1: Foo -> project `a.b.Foo` @file0
        "]],
    );
}

#[test]
fn nested_type_is_indexed_by_dotted_fqn() {
    check(
        &[
            "package a; class Outer { class Inner { } }",
            "package x; import a.Outer.Inner; class Bar { Inner i; }",
        ],
        expect![[r"
            file1: Inner -> project `a.Outer.Inner` @file0
        "]],
    );
}

#[test]
fn qualified_reference_resolves_to_project() {
    check(
        &[
            "package a.b; class Foo { }",
            "package x; class Bar { a.b.Foo f; }",
        ],
        expect![[r"
            file1: Foo -> project `a.b.Foo` @file0
        "]],
    );
}

#[test]
fn java_lang_name_is_external_not_unresolved() {
    check(
        &["package a; class Bar { String s; Object o; }"],
        expect![[r"
            file0: String -> external
            file0: Object -> external
        "]],
    );
}

#[test]
fn import_of_unindexed_type_is_external() {
    check(
        &["package a; import java.util.List; class Bar { List xs; }"],
        expect![[r"
            file0: List -> external
        "]],
    );
}

#[test]
fn on_demand_import_shields_unknown_names() {
    // With an on-demand import present, an unknown bare name might come from it — external.
    check(
        &["package a; import java.util.*; class Bar { Whatever w; }"],
        expect![[r"
            file0: Whatever -> external
        "]],
    );
}

#[test]
fn bare_unknown_type_is_unresolved() {
    // No import, not same-package, not java.lang: genuinely unresolvable.
    check(
        &["package a; class Bar { Nope n; }"],
        expect![[r"
            file0: Nope -> unresolved
        "]],
    );
}

#[test]
fn file_local_type_takes_precedence() {
    // A sibling class in the same file resolves file-locally, before the project layer.
    check(
        &["package a; class Bar { Helper h; } class Helper { }"],
        expect![[r"
            file0: Helper -> local Class `Helper`
        "]],
    );
}

#[test]
fn definition_at_jumps_across_files() {
    let srcs = [
        "package a; class Foo { }",
        "package a; class Bar { Foo f; }",
    ];
    let nodes = nodes(&srcs);
    let index = ProjectIndex::builder(&nodes).build();
    let resolved = Resolved::resolve_node(&nodes[1].1);
    let offset = srcs[1].find("Foo").unwrap();

    let (file, range) = index
        .definition_at(FileId(1), &resolved, offset)
        .expect("type reference resolves cross-file");
    assert_eq!(file, FileId(0));
    assert_eq!(&srcs[0][range], "Foo");
}

#[test]
fn unresolved_types_reports_only_genuine_unknowns() {
    // `Nope` is nameable from nowhere; `String` is java.lang (external); `Helper` resolves
    // file-locally. Only `Nope` is a diagnostic span.
    let srcs = ["package a; class Bar { Nope n; String s; Helper h; } class Helper { }"];
    let nodes = nodes(&srcs);
    let index = ProjectIndex::builder(&nodes).build();
    let resolved = Resolved::resolve_node(&nodes[0].1);

    let spans = index.unresolved_types(FileId(0), &resolved);
    assert_eq!(spans.len(), 1);
    assert_eq!(&srcs[0][spans[0].clone()], "Nope");
}
