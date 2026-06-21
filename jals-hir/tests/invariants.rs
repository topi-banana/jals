//! Property tests: resolution never panics and produces internally consistent, in-bounds results.

use jals_hir::{Resolution, resolve};
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
}
