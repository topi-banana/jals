//! Snapshot tests for the formatter.

use expect_test::{Expect, expect};
use jals_fmt::{BraceStyle, Config, LineEnding, format_source};

fn fmt(src: &str) -> String {
    format_source(src, &Config::default()).formatted
}

fn fmt_with(src: &str, config: &Config) -> String {
    format_source(src, config).formatted
}

fn check(src: &str, expected: Expect) {
    expected.assert_eq(&fmt(src));
}

/// Format with comment reflow enabled at a narrow `comment-width` so wrapping is visible.
fn fmt_wrapped(src: &str, comment_width: usize) -> String {
    let cfg = Config {
        wrap_comments: true,
        comment_width,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_wrapped(src: &str, comment_width: usize, expected: Expect) {
    expected.assert_eq(&fmt_wrapped(src, comment_width));
}

#[test]
fn simple_class() {
    check(
        "package a.b;import java.util.List;public class Foo{private int x=1;void m(int a){return;}}",
        expect![[r#"
            package a.b;
            import java.util.List;
            public class Foo {
                private int x = 1;
                void m(int a) {
                    return;
                }
            }
        "#]],
    );
}

#[test]
fn method_with_statements() {
    check(
        "class C{void m(){int x=1;foo(x);if(x>0){bar();}}}",
        expect![[r#"
            class C {
                void m() {
                    int x = 1;
                    foo(x);
                    if (x > 0) {
                        bar();
                    }
                }
            }
        "#]],
    );
}

#[test]
fn nested_generics() {
    check(
        "class C{Map<K,List<Map<K2,V2>>> f(){return null;}}",
        expect![[r#"
            class C {
                Map<K, List<Map<K2, V2>>> f() {
                    return null;
                }
            }
        "#]],
    );
}

#[test]
fn long_param_list_wraps() {
    check(
        "class C{void method(int aaaaaaaaaaaaaaaa,int bbbbbbbbbbbbbbbb,int cccccccccccccccc,int dddddddddddddddd,int eeeeeeeeeeeeeeee){}}",
        expect![[r#"
            class C {
                void method(
                    int aaaaaaaaaaaaaaaa,
                    int bbbbbbbbbbbbbbbb,
                    int cccccccccccccccc,
                    int dddddddddddddddd,
                    int eeeeeeeeeeeeeeee
                ) {}
            }
        "#]],
    );
}

#[test]
fn text_block_preserved() {
    check(
        "class C{String s=\"\"\"\n        hello\n          world\n        \"\"\";}",
        expect![[r#"
            class C {
                String s = """
                    hello
                      world
                    """;
            }
        "#]],
    );
}

#[test]
fn comments_kept() {
    check(
        "class C{\n// leading\nvoid m(){foo();// trailing\nbar();}}",
        expect![[r#"
            class C {
                // leading
                void m() {
                    foo();  // trailing
                    bar();
                }
            }
        "#]],
    );
}

#[test]
fn binary_operators_spaced() {
    check(
        "class C{boolean b=a>>2==c&&d>=e;}",
        expect![[r#"
        class C {
            boolean b = a >> 2 == c && d >= e;
        }
    "#]],
    );
}

#[test]
fn already_formatted_is_stable() {
    let once = fmt("class C{int x=1;}");
    let twice = fmt(&once);
    assert_eq!(once, twice, "format must be idempotent");
}

// --- max-blank-lines -------------------------------------------------------

#[test]
fn blank_lines_default_clamps_to_one() {
    // Default max-blank-lines = 1: a run of blank lines collapses to a single blank line,
    // and no blank line is kept before the first member.
    check(
        "class C{\n\n\nint a=1;\n\n\n\nint b=2;}",
        expect![[r#"
            class C {
                int a = 1;

                int b = 2;
            }
        "#]],
    );
}

#[test]
fn blank_lines_upper_bound_two() {
    let cfg = Config {
        max_blank_lines: 2,
        ..Config::default()
    };
    // The source has three blank lines between the members; the bound keeps two.
    expect![[r#"
        class C {
            int a = 1;


            int b = 2;
        }
    "#]]
    .assert_eq(&fmt_with("class C{int a=1;\n\n\n\nint b=2;}", &cfg));
}

#[test]
fn blank_lines_upper_bound_zero_removes_all() {
    let cfg = Config {
        max_blank_lines: 0,
        ..Config::default()
    };
    expect![[r#"
        class C {
            int a = 1;
            int b = 2;
        }
    "#]]
    .assert_eq(&fmt_with("class C{int a=1;\n\n\nint b=2;}", &cfg));
}

#[test]
fn blank_lines_bound_is_an_upper_bound_only() {
    // A bound of 3 does not pad a single source blank line up to three.
    let cfg = Config {
        max_blank_lines: 3,
        ..Config::default()
    };
    expect![[r#"
        class C {
            int a = 1;

            int b = 2;
        }
    "#]]
    .assert_eq(&fmt_with("class C{int a=1;\n\nint b=2;}", &cfg));
}

#[test]
fn blank_lines_before_leading_comment_clamped() {
    let cfg = Config {
        max_blank_lines: 2,
        ..Config::default()
    };
    // The blank-line run sits before a leading comment; it is clamped just the same.
    expect![[r#"
        class C {
            int a = 1;


            // note
            int b = 2;
        }
    "#]]
    .assert_eq(&fmt_with(
        "class C{int a=1;\n\n\n\n// note\nint b=2;}",
        &cfg,
    ));
}

#[test]
fn blank_lines_custom_bound_is_idempotent() {
    let cfg = Config {
        max_blank_lines: 2,
        ..Config::default()
    };
    let once = fmt_with("class C{int a=1;\n\n\n\nint b=2;}", &cfg);
    let twice = fmt_with(&once, &cfg);
    assert_eq!(
        once, twice,
        "format must be idempotent under a custom bound"
    );
}

// --- wrap-comments (comment reflow) ----------------------------------------

#[test]
fn comments_not_reflowed_by_default() {
    // Without `wrap-comments`, an over-long comment is left exactly as written.
    check(
        "class C{\n// aaaa bbbb cccc dddd eeee ffff gggg hhhh\nvoid m(){}}",
        expect![[r#"
            class C {
                // aaaa bbbb cccc dddd eeee ffff gggg hhhh
                void m() {}
            }
        "#]],
    );
}

#[test]
fn long_line_comment_wraps() {
    // Indented one level (4 cols); avail = 20 - 4 - 3 = 13 columns of prose per line.
    check_wrapped(
        "class C{\n// aaaa bbbb cccc dddd eeee ffff\nvoid m(){}}",
        20,
        expect![[r#"
            class C {
                // aaaa bbbb
                // cccc dddd
                // eeee ffff
                void m() {}
            }
        "#]],
    );
}

#[test]
fn short_comment_unchanged_when_wrapping() {
    check_wrapped(
        "class C{\n// short note\nvoid m(){}}",
        40,
        expect![[r#"
            class C {
                // short note
                void m() {}
            }
        "#]],
    );
}

#[test]
fn single_line_javadoc_expands_when_too_long() {
    check_wrapped(
        "class C{\n/** Summary that is quite long indeed today. */\nvoid m(){}}",
        30,
        expect![[r#"
            class C {
                /**
                 * Summary that is quite
                 * long indeed today.
                 */
                void m() {}
            }
        "#]],
    );
}

#[test]
fn multiline_javadoc_reflows_and_keeps_tags() {
    check_wrapped(
        "class C{\n/**\n * A description long enough to need wrapping here.\n * @param x the x value\n */\nvoid m(int x){}}",
        30,
        expect![[r#"
            class C {
                /**
                 * A description long
                 * enough to need wrapping
                 * here.
                 * @param x the x value
                 */
                void m(int x) {}
            }
        "#]],
    );
}

#[test]
fn trailing_comment_is_never_wrapped() {
    // Same-line trailing comments stay on their line regardless of width.
    check_wrapped(
        "class C{int x=1;// aaaa bbbb cccc dddd eeee ffff gggg\n}",
        20,
        expect![[r#"
            class C {
                int x = 1;  // aaaa bbbb cccc dddd eeee ffff gggg
            }
        "#]],
    );
}

#[test]
fn wrapped_comments_are_idempotent() {
    let src = "class C{\n// aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii\n/** Long javadoc summary that certainly needs to wrap around. */\nvoid m(){}}";
    let once = fmt_wrapped(src, 28);
    let twice = fmt_wrapped(&once, 28);
    assert_eq!(once, twice, "wrapped formatting must be idempotent");
}

// --- brace-style -----------------------------------------------------------

fn fmt_next_line(src: &str) -> String {
    let cfg = Config {
        brace_style: BraceStyle::NextLine,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn brace_style_next_line_moves_declaration_braces() {
    // Type and method bodies open on the next line; the control-flow `if`/`else` braces stay
    // on the header's line (those are governed by the future `control-brace-style`).
    expect![[r#"
        class Foo
        {
            void m(int a)
            {
                if (a > 0) {
                    foo();
                } else {
                    bar();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_next_line(
        "class Foo{void m(int a){if(a>0){foo();}else{bar();}}}",
    ));
}

#[test]
fn brace_style_next_line_keeps_empty_body_inline() {
    // An empty body stays `{}` on the header's line; only the (non-empty) class body breaks.
    expect![[r#"
        class Foo
        {
            void m() {}
        }
    "#]]
    .assert_eq(&fmt_next_line("class Foo{void m(){}}"));
}

#[test]
fn brace_style_next_line_covers_constructor_and_initializer() {
    expect![[r#"
        class C
        {
            static
            {
                init();
            }
            C()
            {
                this.x = 1;
            }
        }
    "#]]
    .assert_eq(&fmt_next_line("class C{static{init();}C(){this.x=1;}}"));
}

#[test]
fn brace_style_next_line_wraps_param_list_then_breaks_brace() {
    // When the signature wraps, the brace still lands on its own line under the header.
    expect![[r#"
        class C
        {
            void method(
                int aaaaaaaaaaaaaaaa,
                int bbbbbbbbbbbbbbbb,
                int cccccccccccccccc,
                int dddddddddddddddd,
                int eeeeeeeeeeeeeeee
            )
            {
                foo();
            }
        }
    "#]]
    .assert_eq(&fmt_next_line(
        "class C{void method(int aaaaaaaaaaaaaaaa,int bbbbbbbbbbbbbbbb,int cccccccccccccccc,int dddddddddddddddd,int eeeeeeeeeeeeeeee){foo();}}",
    ));
}

#[test]
fn brace_style_next_line_is_idempotent() {
    let once = fmt_next_line("class C{void m(){foo();bar();}static{go();}}");
    let twice = fmt_next_line(&once);
    assert_eq!(once, twice, "next-line brace style must be idempotent");
}

// --- line-ending -----------------------------------------------------------

fn fmt_le(src: &str, line_ending: LineEnding) -> String {
    let cfg = Config {
        line_ending,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

/// Every `\n` in `s` is part of a `\r\n` (no bare LF slipped through).
fn all_crlf(s: &str) -> bool {
    !s.replace("\r\n", "").contains('\n')
}

#[test]
fn crlf_line_ending_emitted() {
    let out = fmt_le("class C{int x=1;}", LineEnding::Crlf);
    assert_eq!(out, "class C {\r\n    int x = 1;\r\n}\r\n");
}

#[test]
fn auto_preserves_lf_input() {
    // The input's first break is a bare LF, so the output stays LF.
    let out = fmt_le("class C{\nint x=1;}", LineEnding::Auto);
    assert_eq!(out, "class C {\n    int x = 1;\n}\n");
}

#[test]
fn auto_preserves_crlf_input() {
    // The input's first break is a CRLF, so the whole output is CRLF — even the breaks the
    // renderer introduces around the (originally one-line) body.
    let out = fmt_le("class C{\r\nint x=1;}", LineEnding::Auto);
    assert_eq!(out, "class C {\r\n    int x = 1;\r\n}\r\n");
    assert!(all_crlf(&out));
}

#[test]
fn auto_first_break_wins_on_mixed_input() {
    // Mixed endings: the first break (CRLF) decides for the entire output.
    let out = fmt_le("class C{\r\nint x=1;\nint y=2;}", LineEnding::Auto);
    assert!(
        all_crlf(&out),
        "first break is CRLF, so output is all CRLF: {out:?}"
    );
}

#[test]
fn auto_without_break_uses_native() {
    // A source with no line break formats the same as Native (platform terminator).
    let out_auto = fmt_le("class C{int x=1;}", LineEnding::Auto);
    let out_native = fmt_le("class C{int x=1;}", LineEnding::Native);
    assert_eq!(out_auto, out_native);
}

#[test]
fn native_matches_platform_line_ending() {
    let out = fmt_le("class C{int x=1;}", LineEnding::Native);
    let expected = if cfg!(windows) {
        "class C {\r\n    int x = 1;\r\n}\r\n"
    } else {
        "class C {\n    int x = 1;\n}\n"
    };
    assert_eq!(out, expected);
}

#[test]
fn auto_is_idempotent_on_crlf() {
    let once = fmt_le(
        "class C{\r\nint x=1;\r\n\r\n\r\nint y=2;}",
        LineEnding::Auto,
    );
    let twice = fmt_le(&once, LineEnding::Auto);
    assert_eq!(once, twice, "auto formatting must be idempotent");
    assert!(all_crlf(&once));
}
