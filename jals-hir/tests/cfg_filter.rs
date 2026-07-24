//! `cfg`-aware analysis: a `#[cfg(...)]`-disabled host is invisible to name resolution and to
//! the project index, exactly as it will be after the compile frontend blanks it.

use std::collections::BTreeSet;

use jals_hir::{FileId, Namespace, ProjectIndex, Resolution, Resolved, TypeResolution};
use jals_syntax::cfg::CfgMap;
use jals_syntax::{Parse, SyntaxNode};

fn parse(src: &str) -> Parse {
    jals_exec::block_on_inline(Parse::parse(src))
}

/// The file's `CfgMap` under the given enabled build features.
fn cfg_of(parse: &Parse, features: &[&str]) -> CfgMap {
    let features: BTreeSet<String> = features.iter().map(ToString::to_string).collect();
    let map = CfgMap::compute(parse, &features);
    assert!(map.errors().is_empty(), "unexpected: {:?}", map.errors());
    map
}

#[test]
fn disabled_defs_and_their_contents_are_not_resolved() {
    let src = "class C {\n    #[cfg(feature = \"x\")]\n    void gone() { helper(); }\n    void kept() { helper(); }\n    void helper() {}\n}";
    let p = parse(src);
    let cfg = cfg_of(&p, &[]);
    let resolved = jals_exec::block_on_inline(Resolved::resolve_node_with_cfg(&p.syntax(), &cfg));

    // `gone` contributes no definition; `kept` and `helper` survive.
    let names: Vec<&str> = resolved.defs.iter().map(|d| d.name.as_str()).collect();
    assert!(!names.contains(&"gone"), "defs: {names:?}");
    assert!(names.contains(&"kept") && names.contains(&"helper"));

    // Only the live `helper()` call is recorded — the one inside the disabled body is not.
    let helper_refs = resolved
        .references
        .iter()
        .filter(|r| r.name == "helper")
        .count();
    assert_eq!(helper_refs, 1);

    // The empty map resolves identically to the plain entry point.
    let plain = jals_exec::block_on_inline(Resolved::resolve_node(&p.syntax()));
    let with_empty = jals_exec::block_on_inline(Resolved::resolve_node_with_cfg(
        &p.syntax(),
        &CfgMap::default(),
    ));
    assert_eq!(plain, with_empty);
}

#[test]
fn enabled_attribute_changes_nothing_in_resolution() {
    // With the feature on, only the attribute text differs from plain Java — resolution must not
    // see it (the attribute holds no NAME_REF/TYPE).
    let src = "class C {\n    #[cfg(feature = \"x\")]\n    void m() { f(); }\n    void f() {}\n}";
    let p = parse(src);
    let cfg = cfg_of(&p, &["x"]);
    let resolved = jals_exec::block_on_inline(Resolved::resolve_node_with_cfg(&p.syntax(), &cfg));
    let names: Vec<&str> = resolved.defs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"m") && names.contains(&"f"));
    assert!(
        resolved
            .references
            .iter()
            .filter(|r| r.namespace == Namespace::Method)
            .any(|r| r.name == "f" && matches!(r.resolution, Resolution::Def(_)))
    );
}

#[test]
fn disabled_types_and_members_leave_the_index() {
    let src = "class C {\n    #[cfg(feature = \"x\")]\n    static class Inner {}\n    #[cfg(feature = \"x\")]\n    void gone() {}\n    void kept() {}\n}";
    let p = parse(src);
    let nodes = [(FileId(0), p.syntax())];

    let index_with = |cfgs: &[(FileId, CfgMap)]| {
        jals_exec::block_on_inline(ProjectIndex::builder(&nodes).with_disabled(cfgs).build())
    };
    let fqns = |index: &ProjectIndex| -> Vec<String> {
        index.items().map(|it| it.fqn.to_string()).collect()
    };
    let member_names = |index: &ProjectIndex| -> Vec<String> {
        let TypeResolution::Project(c) = index.resolve_type_name(FileId(0), "C", None) else {
            panic!("`C` is indexed");
        };
        index
            .members_of(c)
            .into_iter()
            .map(|m| index.member(m).name.clone())
            .collect()
    };

    // Feature off: the nested type and the method vanish.
    let off = index_with(&[(FileId(0), cfg_of(&p, &[]))]);
    assert_eq!(fqns(&off), ["C"]);
    let members = member_names(&off);
    assert!(
        members.contains(&"kept".to_owned()) && !members.contains(&"gone".to_owned()),
        "members: {members:?}"
    );

    // Feature on: everything is back, identical to the unfiltered index.
    let on = index_with(&[(FileId(0), cfg_of(&p, &["x"]))]);
    assert_eq!(fqns(&on), ["C", "C.Inner"]);
    let unfiltered = index_with(&[]);
    assert_eq!(fqns(&on), fqns(&unfiltered));
    assert_eq!(member_names(&on), member_names(&unfiltered));
}

#[test]
fn disabled_import_no_longer_resolves_a_simple_name() {
    // `List` resolves through the import when it is live, and stops resolving as that project
    // type when the import is `cfg`-disabled.
    let lib = "package a;\npublic class List {}";
    let using = "#[cfg(feature = \"x\")] import a.List;\nclass Use { List l; }";
    let p = parse(using);
    let nodes = [(FileId(0), parse(lib).syntax()), (FileId(1), p.syntax())];

    let resolve_list = |cfgs: &[(FileId, CfgMap)]| {
        let index =
            jals_exec::block_on_inline(ProjectIndex::builder(&nodes).with_disabled(cfgs).build());
        let resolved = jals_exec::block_on_inline(Resolved::resolve_node(&nodes[1].1));
        let reference = resolved
            .references
            .iter()
            .find(|r| r.name == "List" && r.namespace == Namespace::Type)
            .expect("the `List l;` type reference");
        index.resolve_reference(FileId(1), reference)
    };

    let on = resolve_list(&[(FileId(1), cfg_of(&p, &["x"]))]);
    assert!(matches!(on, TypeResolution::Project(_)), "{on:?}");
    let off = resolve_list(&[(FileId(1), cfg_of(&p, &[]))]);
    assert!(!matches!(off, TypeResolution::Project(_)), "{off:?}");
}

#[test]
fn cross_file_reference_to_a_disabled_type_unresolves() {
    let defining = "#[cfg(feature = \"x\")]\nclass Gone {}";
    let using = "class Use { Gone g; }";
    let nodes: Vec<(FileId, SyntaxNode)> = [defining, using]
        .iter()
        .enumerate()
        .map(|(i, s)| (FileId(u32::try_from(i).unwrap()), parse(s).syntax()))
        .collect();

    let resolve_gone = |cfgs: &[(FileId, CfgMap)]| {
        let index =
            jals_exec::block_on_inline(ProjectIndex::builder(&nodes).with_disabled(cfgs).build());
        let resolved = jals_exec::block_on_inline(Resolved::resolve_node(&nodes[1].1));
        let reference = resolved
            .references
            .iter()
            .find(|r| r.name == "Gone" && r.namespace == Namespace::Type)
            .expect("the `Gone g;` type reference");
        index.resolve_reference(FileId(1), reference)
    };

    // Feature off: `Gone` is not indexed, so the reference cannot resolve to a project type.
    let p = parse(defining);
    let off = resolve_gone(&[(FileId(0), cfg_of(&p, &[]))]);
    assert!(
        !matches!(off, TypeResolution::Project(_)),
        "disabled type must not resolve: {off:?}"
    );

    // Feature on (and equally: no map at all): it resolves as a project type again.
    let on = resolve_gone(&[(FileId(0), cfg_of(&p, &["x"]))]);
    assert!(matches!(on, TypeResolution::Project(_)));
    let unfiltered = resolve_gone(&[]);
    assert!(matches!(unfiltered, TypeResolution::Project(_)));
}
