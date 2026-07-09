//! Property tests: resolution never panics and produces internally consistent, in-bounds results.

use jals_hir::{
    FileId, ProjectIndex, Resolution, infer, infer_node, resolve, resolve_node, type_mismatches,
};
use jals_syntax::SyntaxNode;
use proptest::prelude::*;

/// A generator of Java-ish source built from fragments that exercise scopes and references.
fn javaish() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("class"),
            Just("interface"),
            Just("enum"),
            Just("record"),
            Just("void"),
            Just("int"),
            Just("var"),
            Just("if"),
            Just("for"),
            Just("while"),
            Just("try"),
            Just("catch"),
            Just("switch"),
            Just("case"),
            Just("return"),
            Just("static"),
            Just("final"),
            Just("new"),
            Just("Foo"),
            Just("foo"),
            Just("x"),
            Just("_"),
            Just("->"),
            Just("."),
            Just(";"),
            Just(","),
            Just(":"),
            Just("{"),
            Just("}"),
            Just("("),
            Just(")"),
            Just("="),
            Just("1"),
            Just("// c\n"),
            Just("\n"),
            Just(" "),
        ],
        0..40,
    )
    .prop_map(|parts| parts.concat())
}

proptest! {
    /// Resolution never panics on Java-ish input.
    #[test]
    fn never_panics(src in javaish()) {
        let _ = resolve(&src);
    }

    /// Resolution never panics on arbitrary input (including arbitrary Unicode).
    #[test]
    fn never_panics_on_arbitrary(src in ".*") {
        let _ = resolve(&src);
    }

    /// Every definition, reference, and scope range is well-formed and within the source bounds.
    #[test]
    fn ranges_in_bounds(src in javaish()) {
        let resolved = resolve(&src);
        let n = src.len();
        for d in &resolved.defs {
            prop_assert!(d.name_range.start <= d.name_range.end);
            prop_assert!(d.name_range.end <= n);
        }
        for r in &resolved.references {
            prop_assert!(r.range.start <= r.range.end);
            prop_assert!(r.range.end <= n);
        }
        for s in &resolved.scopes {
            prop_assert!(s.range.start <= s.range.end);
            prop_assert!(s.range.end <= n);
        }
    }

    /// A resolved reference points at a real definition whose name and name-space match it.
    #[test]
    fn resolutions_are_consistent(src in javaish()) {
        let resolved = resolve(&src);
        for r in &resolved.references {
            if let Resolution::Def(id) = r.resolution {
                let d = resolved.def(id);
                prop_assert_eq!(&d.name, &r.name);
                prop_assert_eq!(d.kind.namespace(), r.namespace);
            }
        }
    }

    /// Building a project index over several Java-ish files and querying it never panics, and every
    /// indexed item's name range is well-formed and within its file's bounds.
    #[test]
    fn project_index_never_panics(srcs in proptest::collection::vec(javaish(), 0..4)) {
        let nodes: Vec<(FileId, SyntaxNode)> = srcs
            .iter()
            .enumerate()
            .map(|(i, s)| (FileId(u32::try_from(i).unwrap()), jals_syntax::parse(s).syntax()))
            .collect();
        let index = ProjectIndex::builder(&nodes).build();
        for (file, root) in &nodes {
            let resolved = resolve_node(root);
            let _ = index.unresolved_types(*file, &resolved);
            for r in &resolved.references {
                let _ = index.definition_at(*file, &resolved, r.range.start);
            }
        }
        for item in index.items() {
            prop_assert!(item.name_range.start <= item.name_range.end);
            prop_assert!(item.name_range.end <= srcs[item.file.0 as usize].len());
        }
    }

    /// Type inference never panics on Java-ish input, with or without a project index, and every
    /// definition and source offset can be queried.
    #[test]
    fn infer_never_panics(src in javaish()) {
        let node = jals_syntax::parse(&src).syntax();
        let resolved = resolve_node(&node);
        let index = ProjectIndex::builder(&[(FileId(0), node.clone())]).build();

        let ti = infer(&node, &resolved, &index, FileId(0));
        for d in &resolved.defs {
            let _ = ti.type_of_def(d.id);
        }
        for offset in 0..=src.len() {
            let _ = ti.type_at(offset);
        }

        // The project-free path must hold up too.
        let ti_local = infer_node(&node, &resolved);
        for d in &resolved.defs {
            let _ = ti_local.type_of_def(d.id);
        }
    }

    /// Type-mismatch collection never panics, with or without an index, and every reported range is
    /// well-formed and within the source bounds.
    #[test]
    fn type_mismatches_never_panic(src in javaish()) {
        let node = jals_syntax::parse(&src).syntax();
        let resolved = resolve_node(&node);
        let index = ProjectIndex::builder(&[(FileId(0), node.clone())]).build();
        let n = src.len();

        for mismatches in [
            type_mismatches(&node, &resolved, Some((&index, FileId(0)))),
            type_mismatches(&node, &resolved, None),
        ] {
            for m in &mismatches {
                prop_assert!(m.range.start <= m.range.end);
                prop_assert!(m.range.end <= n);
                let _ = m.message();
            }
        }
    }
}
