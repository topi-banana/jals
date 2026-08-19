use jals_config::lint::Config;
use jals_lint::LintOutput;
use proptest::prelude::*;

/// A generator of Java-ish source built from fragments that exercise every rule.
fn javaish() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop_oneof![
            Just("class"),
            Just("interface"),
            Just("enum"),
            Just("void"),
            Just("int"),
            Just("if"),
            Just("else"),
            Just("while"),
            Just("for"),
            Just("do"),
            Just("try"),
            Just("catch"),
            Just("return"),
            Just("static"),
            Just("final"),
            Just("import"),
            Just("public"),
            Just("private"),
            Just("Foo"),
            Just("foo"),
            Just("bar_baz"),
            Just("MAX"),
            Just("x"),
            Just("*"),
            Just("."),
            Just(";"),
            Just(","),
            Just("{"),
            Just("}"),
            Just("("),
            Just(")"),
            Just("="),
            Just("1"),
            Just("// c\n"),
            Just("/* b */"),
            // The suppression vocabulary, shredded like everything else here: the extractor walks
            // annotations before any analysis runs, so an unclosed argument list or an annotation
            // with no declaration after it reaches it as readily as a well-formed one.
            Just("@SuppressWarnings"),
            Just("@java.lang.SuppressWarnings"),
            Just("value"),
            Just("\"unused\""),
            Just("\"all\""),
            Just("\n"),
            Just(" "),
        ],
        0..40,
    )
    .prop_map(|parts| parts.concat())
}

proptest! {
    /// Linting never panics on Java-ish input.
    #[test]
    fn never_panics(src in javaish()) {
        let _ = jals_exec::block_on_inline(LintOutput::lint_source(&src, &Config::default()));
    }

    /// Linting never panics on arbitrary input.
    #[test]
    fn never_panics_on_arbitrary(src in ".*") {
        let _ = jals_exec::block_on_inline(LintOutput::lint_source(&src, &Config::default()));
    }

    /// Every diagnostic range is well-formed and within the source bounds.
    #[test]
    fn ranges_in_bounds(src in javaish()) {
        let out = jals_exec::block_on_inline(LintOutput::lint_source(&src, &Config::default()));
        for d in &out.diagnostics {
            prop_assert!(d.range.start <= d.range.end);
            prop_assert!(d.range.end <= src.len());
        }
    }
}
