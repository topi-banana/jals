//! Snapshot tests for the formatter.

use expect_test::{Expect, expect};
use jals_fmt::{
    BinopSeparator, BraceStyle, Config, ControlBraceStyle, LineEnding, TrailingComma, format_source,
};

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

/// Format with `reorder-imports` enabled.
fn fmt_reorder(src: &str) -> String {
    let cfg = Config {
        reorder_imports: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_reorder(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_reorder(src));
}

/// Format with `group-imports` enabled (default `import-groups`).
fn fmt_group(src: &str) -> String {
    let cfg = Config {
        group_imports: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_group(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_group(src));
}

/// Format with `group-imports` enabled and a custom `import-groups` list.
fn fmt_group_with(src: &str, import_groups: &[&str]) -> String {
    let cfg = Config {
        group_imports: true,
        import_groups: import_groups.iter().map(|s| s.to_string()).collect(),
        ..Config::default()
    };
    format_source(src, &cfg).formatted
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
fn class_literals() {
    check(
        "class A{void m(){f(int.class);f(void.class);f(int[].class);f(String[].class);f(java.lang.String[][].class);}}",
        expect![[r#"
            class A {
                void m() {
                    f(int.class);
                    f(void.class);
                    f(int [].class);
                    f(String [].class);
                    f(java.lang.String [] [].class);
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
fn explicit_type_witness_hugs_method_name() {
    check(
        "class C{void m(){var a=List.<String>of();var b=Collections.<File>emptyList();var c=Foo::<String>bar;}}",
        expect![[r#"
            class C {
                void m() {
                    var a = List.<String>of();
                    var b = Collections.<File>emptyList();
                    var c = Foo::<String>bar;
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
fn comments_after_final_brace_kept() {
    // Own-line comments after the file's last significant token survive even when that token
    // is a closing brace (emitted by `lower_braced`, not the generic token path).
    check(
        "class A{} // same\n// below\n/* block */\n",
        expect![[r#"
            class A {}  // same
            // below
            /* block */
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

// --- control-brace-style ---------------------------------------------------

/// Format with `control-brace-style = next-line` (declaration braces left at the default K&R).
fn fmt_ctrl_next_line(src: &str) -> String {
    let cfg = Config {
        control_brace_style: ControlBraceStyle::NextLine,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

/// Format in full Allman: both `brace-style` and `control-brace-style` break onto their lines.
fn fmt_full_allman(src: &str) -> String {
    let cfg = Config {
        brace_style: BraceStyle::NextLine,
        control_brace_style: ControlBraceStyle::NextLine,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn control_brace_style_next_line_moves_control_braces_only() {
    // The mirror image of `brace_style_next_line_moves_declaration_braces`: the type and
    // method braces stay K&R, while the `if`/`else` control-flow braces and the `else`
    // continuation break onto their own lines.
    expect![[r#"
        class Foo {
            void m(int a) {
                if (a > 0)
                {
                    foo();
                }
                else
                {
                    bar();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_ctrl_next_line(
        "class Foo{void m(int a){if(a>0){foo();}else{bar();}}}",
    ));
}

#[test]
fn control_brace_style_next_line_try_catch_finally() {
    expect![[r#"
        class C {
            void m() {
                try
                {
                    a();
                }
                catch (E e)
                {
                    b();
                }
                finally
                {
                    c();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_ctrl_next_line(
        "class C{void m(){try{a();}catch(E e){b();}finally{c();}}}",
    ));
}

#[test]
fn control_brace_style_next_line_loops() {
    // A `while` loop's opening brace breaks (via the block), and a `do`-`while`'s trailing
    // `while` breaks (via the continuation).
    expect![[r#"
        class C {
            void m() {
                while (x)
                {
                    a();
                }
                do
                {
                    b();
                }
                while (y);
            }
        }
    "#]]
    .assert_eq(&fmt_ctrl_next_line(
        "class C{void m(){while(x){a();}do{b();}while(y);}}",
    ));
}

#[test]
fn control_brace_style_does_not_touch_declaration_braces() {
    // With only `control-brace-style` set, declaration bodies stay K&R; an empty method body
    // still collapses to `{}`.
    expect![[r#"
        class C {
            void m() {}
        }
    "#]]
    .assert_eq(&fmt_ctrl_next_line("class C{void m(){}}"));
}

#[test]
fn full_allman_breaks_every_brace() {
    expect![[r#"
        class Foo
        {
            void m(int a)
            {
                if (a > 0)
                {
                    foo();
                }
                else
                {
                    bar();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_full_allman(
        "class Foo{void m(int a){if(a>0){foo();}else{bar();}}}",
    ));
}

#[test]
fn control_brace_style_next_line_is_idempotent() {
    let once = fmt_ctrl_next_line("class C{void m(){try{a();}catch(E e){b();}finally{c();}}}");
    let twice = fmt_ctrl_next_line(&once);
    assert_eq!(
        once, twice,
        "next-line control brace style must be idempotent"
    );
}

#[test]
fn full_allman_is_idempotent() {
    let once = fmt_full_allman("class C{void m(){if(a){x();}else{y();}do{z();}while(p);}}");
    let twice = fmt_full_allman(&once);
    assert_eq!(once, twice, "full Allman must be idempotent");
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

// ---------------------------------------------------------------------------
// Method chains (`chain-width`)
// ---------------------------------------------------------------------------

fn fmt_chain(src: &str, chain_width: usize) -> String {
    let cfg = Config {
        chain_width,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn short_chain_stays_inline() {
    check(
        "class A{void m(){foo.bar().baz();}}",
        expect![[r#"
            class A {
                void m() {
                    foo.bar().baz();
                }
            }
        "#]],
    );
}

#[test]
fn long_chain_breaks_one_call_per_line() {
    check(
        "class A{void m(){result=source.stream().filter(x->x.isActive()).map(Item::getName).sorted().collect(Collectors.toList());}}",
        expect![[r#"
            class A {
                void m() {
                    result = source.stream()
                        .filter(x -> x.isActive())
                        .map(Item::getName)
                        .sorted()
                        .collect(Collectors.toList());
                }
            }
        "#]],
    );
}

#[test]
fn leading_field_path_hugs_head() {
    check(
        "class A{void m(){this.config.getServiceRegistry().lookupByName(\"primary\").resolveAndConnect();}}",
        expect![[r#"
            class A {
                void m() {
                    this.config.getServiceRegistry()
                        .lookupByName("primary")
                        .resolveAndConnect();
                }
            }
        "#]],
    );
}

#[test]
fn field_only_path_stays_inline() {
    // No calls: a pure field path is never broken, even past max-width.
    check(
        "class A{void m(){x=a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p.q.r.s.t.u.v.w.x.y.z.aa.bb.cc.dd.ee.ff.gg.hh;}}",
        expect![[r#"
            class A {
                void m() {
                    x = a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p.q.r.s.t.u.v.w.x.y.z.aa.bb.cc.dd.ee.ff.gg.hh;
                }
            }
        "#]],
    );
}

#[test]
fn type_witness_preserved_in_broken_chain() {
    check(
        "class A{void m(){result=obj.<String>alpha().<Integer>beta().gamma().delta().epsilon();}}",
        expect![[r#"
            class A {
                void m() {
                    result = obj.<String>alpha()
                        .<Integer>beta()
                        .gamma()
                        .delta()
                        .epsilon();
                }
            }
        "#]],
    );
}

#[test]
fn chain_width_forces_break_below_max_width() {
    // Fits max-width(100) but exceeds the narrow chain-width, so it still breaks.
    expect![[r#"
        class A {
            void m() {
                v = alpha.beta()
                    .gamma()
                    .delta();
            }
        }
    "#]]
    .assert_eq(&fmt_chain(
        "class A{void m(){v=alpha.beta().gamma().delta();}}",
        20,
    ));
}

#[test]
fn chain_width_generous_keeps_inline() {
    // A generous chain-width keeps the same chain on one line.
    expect![[r#"
        class A {
            void m() {
                v = alpha.beta().gamma().delta();
            }
        }
    "#]]
    .assert_eq(&fmt_chain(
        "class A{void m(){v=alpha.beta().gamma().delta();}}",
        200,
    ));
}

// ---------------------------------------------------------------------------
// Function calls (`fn-call-width`)
// ---------------------------------------------------------------------------

fn fmt_fn_call(src: &str, fn_call_width: usize) -> String {
    let cfg = Config {
        fn_call_width,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn fn_call_width_forces_break_below_max_width() {
    // Fits max-width(100) but exceeds the narrow fn-call-width, so the args break.
    expect![[r#"
        class A {
            void m() {
                foo(
                    alpha,
                    beta,
                    gamma
                );
            }
        }
    "#]]
    .assert_eq(&fmt_fn_call(
        "class A{void m(){foo(alpha,beta,gamma);}}",
        10,
    ));
}

#[test]
fn fn_call_width_generous_keeps_inline() {
    // A generous fn-call-width keeps the call on one line.
    expect![[r#"
        class A {
            void m() {
                foo(alpha, beta, gamma);
            }
        }
    "#]]
    .assert_eq(&fmt_fn_call(
        "class A{void m(){foo(alpha,beta,gamma);}}",
        200,
    ));
}

#[test]
fn fn_call_width_leaves_param_list_inline() {
    // fn-call-width targets call argument lists (ARG_LIST), not method-definition
    // parameter lists (PARAM_LIST), which stay inline under a narrow fn-call-width.
    expect![[r#"
        class A {
            void method(int alpha, int beta, int gamma) {}
        }
    "#]]
    .assert_eq(&fmt_fn_call(
        "class A{void method(int alpha,int beta,int gamma){}}",
        5,
    ));
}

// ---------------------------------------------------------------------------
// Array initializers (`array-width`)
// ---------------------------------------------------------------------------

fn fmt_array_init(src: &str, array_width: usize) -> String {
    let cfg = Config {
        array_width,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn array_width_forces_break_below_max_width() {
    // Fits max-width(100) but exceeds the narrow array-width, so the elements break.
    expect![[r#"
        class A {
            int [] x = {
                alpha,
                beta,
                gamma
            };
        }
    "#]]
    .assert_eq(&fmt_array_init("class A{int[] x={alpha,beta,gamma};}", 10));
}

#[test]
fn array_width_generous_keeps_inline() {
    // A generous array-width keeps the initializer on one line.
    expect![[r#"
        class A {
            int [] x = {alpha, beta, gamma};
        }
    "#]]
    .assert_eq(&fmt_array_init("class A{int[] x={alpha,beta,gamma};}", 200));
}

#[test]
fn array_width_breaks_new_array_creation() {
    // `new T[]{…}` carries the same ARRAY_INIT node, so it honors array-width too.
    expect![[r#"
        class A {
            int [] x = new int [] {
                alpha,
                beta,
                gamma
            };
        }
    "#]]
    .assert_eq(&fmt_array_init(
        "class A{int[] x=new int[]{alpha,beta,gamma};}",
        10,
    ));
}

// ---------------------------------------------------------------------------
// Trailing comma (`trailing-comma`)
// ---------------------------------------------------------------------------

fn fmt_trailing(src: &str, trailing_comma: TrailingComma) -> String {
    let cfg = Config {
        trailing_comma,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

/// Format with a `trailing-comma` policy and a narrow `array-width`, so the initializer breaks
/// one element per line — exercising the layout-sensitive `vertical` mode.
fn fmt_trailing_narrow(src: &str, trailing_comma: TrailingComma) -> String {
    let cfg = Config {
        trailing_comma,
        array_width: 10,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn trailing_comma_preserve_keeps_source_absent() {
    // Preserve (the default): an absent trailing comma stays absent.
    expect![[r#"
        class A {
            int [] x = {a, b, c};
        }
    "#]]
    .assert_eq(&fmt_trailing(
        "class A{int[] x={a,b,c};}",
        TrailingComma::Preserve,
    ));
}

#[test]
fn trailing_comma_preserve_keeps_source_present() {
    // Preserve: a present trailing comma stays present.
    expect![[r#"
        class A {
            int [] x = {a, b, c,};
        }
    "#]]
    .assert_eq(&fmt_trailing(
        "class A{int[] x={a,b,c,};}",
        TrailingComma::Preserve,
    ));
}

#[test]
fn trailing_comma_always_adds_when_absent() {
    expect![[r#"
        class A {
            int [] x = {a, b, c,};
        }
    "#]]
    .assert_eq(&fmt_trailing(
        "class A{int[] x={a,b,c};}",
        TrailingComma::Always,
    ));
}

#[test]
fn trailing_comma_never_drops_when_present() {
    expect![[r#"
        class A {
            int [] x = {a, b, c};
        }
    "#]]
    .assert_eq(&fmt_trailing(
        "class A{int[] x={a,b,c,};}",
        TrailingComma::Never,
    ));
}

#[test]
fn trailing_comma_vertical_omits_when_flat() {
    // Fits on one line, so the comma is omitted — even though the source had one.
    expect![[r#"
        class A {
            int [] x = {a, b, c};
        }
    "#]]
    .assert_eq(&fmt_trailing(
        "class A{int[] x={a,b,c,};}",
        TrailingComma::Vertical,
    ));
}

#[test]
fn trailing_comma_vertical_adds_when_broken() {
    // Broken one element per line, so the comma is added.
    expect![[r#"
        class A {
            int [] x = {
                alpha,
                beta,
                gamma,
            };
        }
    "#]]
    .assert_eq(&fmt_trailing_narrow(
        "class A{int[] x={alpha,beta,gamma};}",
        TrailingComma::Vertical,
    ));
}

#[test]
fn trailing_comma_never_omits_even_when_broken() {
    // `never` keeps no trailing comma regardless of layout.
    expect![[r#"
        class A {
            int [] x = {
                alpha,
                beta,
                gamma
            };
        }
    "#]]
    .assert_eq(&fmt_trailing_narrow(
        "class A{int[] x={alpha,beta,gamma,};}",
        TrailingComma::Never,
    ));
}

#[test]
fn trailing_comma_always_when_broken() {
    expect![[r#"
        class A {
            int [] x = {
                alpha,
                beta,
                gamma,
            };
        }
    "#]]
    .assert_eq(&fmt_trailing_narrow(
        "class A{int[] x={alpha,beta,gamma};}",
        TrailingComma::Always,
    ));
}

#[test]
fn trailing_comma_only_touches_array_initializers() {
    // `trailing-comma` governs array initializers only; a call's argument list is never given a
    // trailing comma (it would be invalid Java).
    expect![[r#"
        class A {
            void m() {
                foo(a, b, c);
            }
        }
    "#]]
    .assert_eq(&fmt_trailing(
        "class A{void m(){foo(a,b,c);}}",
        TrailingComma::Always,
    ));
}

#[test]
fn trailing_comma_never_keeps_commented_comma() {
    // A comment glued to the trailing comma is never dropped: `never` keeps the comma so the
    // comment survives.
    expect![[r#"
        class A {
            int [] x = {a, b, c,};  /* keep */
        }
    "#]]
    .assert_eq(&fmt_trailing(
        "class A{int[] x={a,b,c, /* keep */};}",
        TrailingComma::Never,
    ));
}

#[test]
fn trailing_comma_modes_are_idempotent() {
    for mode in [
        TrailingComma::Always,
        TrailingComma::Never,
        TrailingComma::Vertical,
    ] {
        let src = "class A{int[] x={alpha,beta,gamma};int[] y={p,q,};}";
        let once = fmt_trailing_narrow(src, mode);
        let twice = fmt_trailing_narrow(&once, mode);
        assert_eq!(once, twice, "trailing-comma {mode:?} must be idempotent");
    }
}

// ===== reorder-imports =====

#[test]
fn reorder_imports_sorts_basic() {
    check_reorder(
        "package a.b;import java.util.Map;import java.util.List;class C{}",
        expect![[r#"
            package a.b;
            import java.util.List;
            import java.util.Map;
            class C {}
        "#]],
    );
}

#[test]
fn reorder_imports_static_to_end() {
    // Non-static imports first (alphabetical), then static imports (alphabetical).
    check_reorder(
        "import static a.Z.z;import b.A;import static a.A.a;import b.B;class C{}",
        expect![[r#"
            import b.A;
            import b.B;
            import static a.A.a;
            import static a.Z.z;
            class C {}
        "#]],
    );
}

#[test]
fn reorder_imports_normalizes_blank_lines() {
    // Blank lines between imports are dropped; the package->block and block->class gaps stay.
    check_reorder(
        "package p;\n\nimport c.C;\n\nimport a.A;\nimport b.B;\n\nclass C {}\n",
        expect![[r#"
            package p;

            import a.A;
            import b.B;
            import c.C;

            class C {}
        "#]],
    );
}

#[test]
fn reorder_imports_comments_follow() {
    // A leading and a trailing comment glued to an import move with it when it is reordered.
    check_reorder(
        "import b.B;\n// lead for a\nimport a.A; // trail for a\nclass C {}\n",
        expect![[r#"
            // lead for a
            import a.A;  // trail for a
            import b.B;
            class C {}
        "#]],
    );
}

#[test]
fn reorder_imports_wildcard() {
    // `*` (0x2A) sorts before `.` (0x2E), so `a.b.*` precedes `a.b.C`. Locks the chosen order.
    check_reorder(
        "import a.b.C;import a.b.*;class X{}",
        expect![[r#"
            import a.b.*;
            import a.b.C;
            class X {}
        "#]],
    );
}

#[test]
fn reorder_imports_no_package_imports_first() {
    // No package decl: the file starts with imports; no leading blank line is introduced.
    check_reorder(
        "import c.C;import a.A;class X{}",
        expect![[r#"
        import a.A;
        import c.C;
        class X {}
    "#]],
    );
}

#[test]
fn reorder_imports_off_by_default_preserves_order() {
    // With the default config the import order is preserved exactly (strict-sequence invariant).
    check(
        "import java.util.Map;import java.util.List;class C{}",
        expect![[r#"
            import java.util.Map;
            import java.util.List;
            class C {}
        "#]],
    );
}

#[test]
fn reorder_imports_single_import_unchanged() {
    // A single import has nothing to sort (the `< 2` guard); output is unchanged.
    check_reorder(
        "import java.util.List;class C{}",
        expect![[r#"
        import java.util.List;
        class C {}
    "#]],
    );
}

#[test]
fn reorder_imports_already_sorted_is_idempotent() {
    let once = fmt_reorder("import a.A;\nimport b.B;\nclass C {}\n");
    let twice = fmt_reorder(&once);
    assert_eq!(once, twice);
}

#[test]
fn reorder_imports_idempotent_when_scrambled() {
    let once = fmt_reorder(
        "package p;\n\nimport c.C;\nimport a.A;\nimport static z.Z.z;\nimport b.B;\nclass C {}\n",
    );
    let twice = fmt_reorder(&once);
    assert_eq!(once, twice);
}

// ===== group-imports =====

#[test]
fn group_imports_basic() {
    // Default groups: java. / javax. / others (`*`) / static — each block sorted, blank-separated.
    check_group(
        "import com.foo.Bar;import java.util.List;import static org.junit.Assert.assertEquals;import javax.annotation.Nullable;class C{}",
        expect![[r#"
            import java.util.List;

            import javax.annotation.Nullable;

            import com.foo.Bar;

            import static org.junit.Assert.assertEquals;
            class C {}
        "#]],
    );
}

#[test]
fn group_imports_catch_all_block() {
    // Imports matching no prefix cluster in the `*` block, sorted, after the java./javax. blocks.
    check_group(
        "import org.b.B;import com.a.A;import java.util.List;class C{}",
        expect![[r#"
            import java.util.List;

            import com.a.A;
            import org.b.B;
            class C {}
        "#]],
    );
}

#[test]
fn group_imports_static_block_last() {
    // Every static import clusters in the trailing `static` block, sorted by qualified name.
    check_group(
        "import static b.B.b;import static a.A.a;import java.util.List;class C{}",
        expect![[r#"
            import java.util.List;

            import static a.A.a;
            import static b.B.b;
            class C {}
        "#]],
    );
}

#[test]
fn group_imports_empty_group_no_blank() {
    // No javax. import: a single blank separates java. from the catch-all (no stray blank line).
    check_group(
        "import com.x.X;import java.util.List;class C{}",
        expect![[r#"
            import java.util.List;

            import com.x.X;
            class C {}
        "#]],
    );
}

#[test]
fn group_imports_comment_follows() {
    // A leading and trailing comment glued to an import move with it into its group.
    check_group(
        "import com.b.B;\n// lead\nimport java.a.A; // trail\nclass C {}\n",
        expect![[r#"
            // lead
            import java.a.A;  // trail

            import com.b.B;
            class C {}
        "#]],
    );
}

#[test]
fn group_imports_independent_of_reorder() {
    // group-imports works on its own; it does not require reorder-imports to be enabled.
    let cfg = Config {
        group_imports: true,
        reorder_imports: false,
        ..Config::default()
    };
    let out = format_source("import b.B;import a.A;class C{}", &cfg).formatted;
    expect![[r#"
        import a.A;
        import b.B;
        class C {}
    "#]]
    .assert_eq(&out);
}

#[test]
fn group_imports_off_by_default_preserves_order() {
    // The default config leaves import order untouched (strict-sequence invariant).
    check(
        "import com.b.B;import java.a.A;class C{}",
        expect![[r#"
            import com.b.B;
            import java.a.A;
            class C {}
        "#]],
    );
}

#[test]
fn group_imports_idempotent() {
    let once = fmt_group(
        "package p;\n\nimport c.C;\nimport java.util.List;\nimport static z.Z.z;\nimport javax.x.X;\nclass C {}\n",
    );
    let twice = fmt_group(&once);
    assert_eq!(once, twice);
}

#[test]
fn group_imports_custom_longest_prefix_wins() {
    // With ["java.", "java.util.", "*"], java.util.List joins the longer "java.util." group, not
    // "java." — locking longest-match (list-order first-match would merge it with java.io.File).
    let out = fmt_group_with(
        "import java.io.File;import java.util.List;import com.x.X;class C{}",
        &["java.", "java.util.", "*"],
    );
    expect![[r#"
        import java.io.File;

        import java.util.List;

        import com.x.X;
        class C {}
    "#]]
    .assert_eq(&out);
}

#[test]
fn group_imports_overrides_reorder() {
    // With both on, group-imports wins: output equals group-only (the reorder value is irrelevant).
    let src = "import static a.Z.z;import b.B;import java.util.List;class C{}";
    let group_only = fmt_group(src);
    let both = {
        let cfg = Config {
            group_imports: true,
            reorder_imports: true,
            ..Config::default()
        };
        format_source(src, &cfg).formatted
    };
    assert_eq!(group_only, both);
}

// ---------------------------------------------------------------------------
// Binary-expression wrapping (`binop-separator`)
// ---------------------------------------------------------------------------

fn fmt_binop(src: &str, max_width: usize, binop_separator: BinopSeparator) -> String {
    let cfg = Config {
        max_width,
        binop_separator,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn short_binary_stays_flat_in_both_modes() {
    // The option only governs placement when wrapping happens; a fitting expression is
    // identical (and unwrapped) under both modes.
    let src = "class A{int x=a+b*c;}";
    let front = fmt_binop(src, 100, BinopSeparator::Front);
    let back = fmt_binop(src, 100, BinopSeparator::Back);
    assert_eq!(front, back);
    expect![[r#"
        class A {
            int x = a + b * c;
        }
    "#]]
    .assert_eq(&front);
}

#[test]
fn long_binary_wraps_operator_front_by_default() {
    // Past max-width(100) the additive run breaks at every operator, each leading its line.
    check(
        "class A{void m(){result=alphaOperandName+betaOperandName+gammaOperandName+deltaOperandName+epsilonOperandName;}}",
        expect![[r#"
            class A {
                void m() {
                    result = alphaOperandName
                        + betaOperandName
                        + gammaOperandName
                        + deltaOperandName
                        + epsilonOperandName;
                }
            }
        "#]],
    );
}

#[test]
fn long_binary_wraps_operator_back() {
    // The same source with `binop-separator = back`: each operator ends the broken line.
    expect![[r#"
        class A {
            void m() {
                result = alphaOperandName +
                    betaOperandName +
                    gammaOperandName +
                    deltaOperandName +
                    epsilonOperandName;
            }
        }
    "#]].assert_eq(&fmt_binop(
        "class A{void m(){result=alphaOperandName+betaOperandName+gammaOperandName+deltaOperandName+epsilonOperandName;}}",
        100,
        BinopSeparator::Back,
    ));
}

#[test]
fn mixed_precedence_breaks_at_lowest_first() {
    // Only the `||` level needs to break; the `&&`/`==` operands stay intact on their lines.
    expect![[r#"
        class A {
            void m() {
                flag = aLongName == bLongName && cLongName == dLongName
                    || eLongName == fLongName;
            }
        }
    "#]]
    .assert_eq(&fmt_binop(
        "class A{void m(){flag=aLongName==bLongName&&cLongName==dLongName||eLongName==fLongName;}}",
        65,
        BinopSeparator::Front,
    ));
}

#[test]
fn mixed_precedence_breaks_inner_level_when_narrower() {
    // Narrower still: the `&&` group also breaks, while each `==` unit stays on one line.
    expect![[r#"
        class A {
            void m() {
                flag = aLongName == bLongName
                    && cLongName == dLongName
                    || eLongName == fLongName;
            }
        }
    "#]]
    .assert_eq(&fmt_binop(
        "class A{void m(){flag=aLongName==bLongName&&cLongName==dLongName||eLongName==fLongName;}}",
        40,
        BinopSeparator::Front,
    ));
}

#[test]
fn multiplicative_stays_a_unit_when_additive_breaks() {
    // Breaks at `+` only; the higher-precedence `*` run rides along as one unit.
    expect![[r#"
        class A {
            void m() {
                total = alphaValue
                    + betaValue * gammaValue * deltaValue
                    + epsilonValue;
            }
        }
    "#]]
    .assert_eq(&fmt_binop(
        "class A{void m(){total=alphaValue+betaValue*gammaValue*deltaValue+epsilonValue;}}",
        55,
        BinopSeparator::Front,
    ));
}

#[test]
fn shift_operators_stay_fused_when_wrapped_front() {
    // `>>` / `>>>` are runs of `>` tokens; they stay fused at the front of their lines.
    expect![[r#"
        class A {
            void m() {
                mask = firstLongValue
                    >> secondShiftAmount
                    >>> thirdShiftAmount;
            }
        }
    "#]]
    .assert_eq(&fmt_binop(
        "class A{void m(){mask=firstLongValue>>secondShiftAmount>>>thirdShiftAmount;}}",
        40,
        BinopSeparator::Front,
    ));
}

#[test]
fn shift_operators_stay_fused_when_wrapped_back() {
    expect![[r#"
        class A {
            void m() {
                mask = firstLongValue >>
                    secondShiftAmount >>>
                    thirdShiftAmount;
            }
        }
    "#]]
    .assert_eq(&fmt_binop(
        "class A{void m(){mask=firstLongValue>>secondShiftAmount>>>thirdShiftAmount;}}",
        40,
        BinopSeparator::Back,
    ));
}

#[test]
fn instanceof_wraps_like_any_operator() {
    expect![[r#"
        class A {
            void m() {
                flag = someObjectReference
                    instanceof SomeVeryLongGenericTypeName;
            }
        }
    "#]]
    .assert_eq(&fmt_binop(
        "class A{void m(){flag=someObjectReference instanceof SomeVeryLongGenericTypeName;}}",
        40,
        BinopSeparator::Front,
    ));
}

#[test]
fn paren_operand_stays_a_unit() {
    // Parenthesized operands are opaque units; the break lands on the `*` between them.
    expect![[r#"
        class A {
            void m() {
                area = (widthValue + paddingValue)
                    * (heightValue + marginValue);
            }
        }
    "#]]
    .assert_eq(&fmt_binop(
        "class A{void m(){area=(widthValue+paddingValue)*(heightValue+marginValue);}}",
        45,
        BinopSeparator::Front,
    ));
}

#[test]
fn comment_on_operator_forces_break_front() {
    // A trailing line comment on the operator forces the whole run broken, and stays
    // glued to its `+`.
    let src = "class A{void m(){x = a + // why\nb;}}";
    let once = fmt_binop(src, 100, BinopSeparator::Front);
    expect![[r#"
        class A {
            void m() {
                x = a
                    +  // why
                    b;
            }
        }
    "#]]
    .assert_eq(&once);
    assert_eq!(once, fmt_binop(&once, 100, BinopSeparator::Front));
}

#[test]
fn comment_on_operator_forces_break_back() {
    let src = "class A{void m(){x = a + // why\nb;}}";
    let once = fmt_binop(src, 100, BinopSeparator::Back);
    expect![[r#"
        class A {
            void m() {
                x = a +  // why
                    b;
            }
        }
    "#]]
    .assert_eq(&once);
    assert_eq!(once, fmt_binop(&once, 100, BinopSeparator::Back));
}

#[test]
fn binary_inside_broken_arg_list_refits_at_its_column() {
    // The narrow fn-call-width breaks the call one argument per line; the binary argument
    // is re-measured at its row column and stays flat there.
    let cfg = Config {
        fn_call_width: 10,
        ..Config::default()
    };
    expect![[r#"
        class A {
            void m() {
                process(
                    alphaValue + betaValue,
                    gammaValue
                );
            }
        }
    "#]]
    .assert_eq(
        &format_source(
            "class A{void m(){process(alphaValue+betaValue,gammaValue);}}",
            &cfg,
        )
        .formatted,
    );
}

#[test]
fn binop_wrapping_is_idempotent() {
    let src =
        "class A{void m(){flag=aLongName==bLongName&&cLongName==dLongName||eLongName==fLongName;}}";
    for sep in [BinopSeparator::Front, BinopSeparator::Back] {
        let once = fmt_binop(src, 40, sep);
        let twice = fmt_binop(&once, 40, sep);
        assert_eq!(once, twice, "binop wrapping must be idempotent ({sep:?})");
    }
}

// ---------------------------------------------------------------------------
// Last-argument overflow (`overflow-delimited-expr`)
// ---------------------------------------------------------------------------

/// Format with `overflow-delimited-expr` enabled.
fn fmt_overflow(src: &str) -> String {
    let cfg = Config {
        overflow_delimited_expr: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_overflow(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_overflow(src));
}

#[test]
fn overflow_off_by_default_keeps_vertical_layout() {
    // Without the option a multi-line trailing lambda still breaks the list all-or-nothing.
    check(
        "class A{void m(){executor.submit(task,()->{run();});}}",
        expect![[r#"
            class A {
                void m() {
                    executor.submit(
                        task,
                        () -> {
                            run();
                        }
                    );
                }
            }
        "#]],
    );
}

#[test]
fn overflow_hangs_trailing_block_lambda() {
    check_overflow(
        "class A{void m(){executor.submit(task,()->{run();});}}",
        expect![[r#"
            class A {
                void m() {
                    executor.submit(task, () -> {
                        run();
                    });
                }
            }
        "#]],
    );
}

#[test]
fn overflow_hangs_sole_lambda_argument() {
    check_overflow(
        "class A{void m(){run(()->{go();});}}",
        expect![[r#"
            class A {
                void m() {
                    run(() -> {
                        go();
                    });
                }
            }
        "#]],
    );
}

#[test]
fn overflow_hangs_trailing_anonymous_class() {
    check_overflow(
        "class A{void m(){register(name,new Listener(){public void on(){go();}});}}",
        expect![[r#"
            class A {
                void m() {
                    register(name, new Listener() {
                        public void on() {
                            go();
                        }
                    });
                }
            }
        "#]],
    );
}

#[test]
fn overflow_hangs_trailing_array_creation() {
    // A trailing `new int[]{…}` broken by a narrow `array-width` hangs past the call line.
    let cfg = Config {
        overflow_delimited_expr: true,
        array_width: 10,
        ..Config::default()
    };
    expect![[r#"
        class A {
            void m() {
                fill(buf, new int [] {
                    alpha,
                    beta,
                    gamma
                });
            }
        }
    "#]]
    .assert_eq(&fmt_with(
        "class A{void m(){fill(buf,new int[]{alpha,beta,gamma});}}",
        &cfg,
    ));
}

#[test]
fn overflow_array_honors_vertical_trailing_comma() {
    // The hanging array breaks, so `trailing-comma = vertical` adds the comma; the outer
    // overflow group staying "flat" does not leak into the inner array's mode.
    let cfg = Config {
        overflow_delimited_expr: true,
        array_width: 10,
        trailing_comma: TrailingComma::Vertical,
        ..Config::default()
    };
    expect![[r#"
        class A {
            void m() {
                fill(buf, new int [] {
                    alpha,
                    beta,
                    gamma,
                });
            }
        }
    "#]]
    .assert_eq(&fmt_with(
        "class A{void m(){fill(buf,new int[]{alpha,beta,gamma});}}",
        &cfg,
    ));
}

#[test]
fn overflow_hangs_annotation_array() {
    let cfg = Config {
        overflow_delimited_expr: true,
        array_width: 10,
        ..Config::default()
    };
    expect![[r#"
        @Foo({
            alpha,
            beta,
            gamma
        }) class A {}
    "#]]
    .assert_eq(&fmt_with("@Foo({alpha,beta,gamma}) class A{}", &cfg));
}

#[test]
fn overflow_hangs_annotation_pair_value() {
    let cfg = Config {
        overflow_delimited_expr: true,
        array_width: 10,
        ..Config::default()
    };
    expect![[r#"
        @Foo(key = {
            alpha,
            beta,
            gamma
        }) class A {}
    "#]]
    .assert_eq(&fmt_with("@Foo(key={alpha,beta,gamma}) class A{}", &cfg));
}

#[test]
fn overflow_falls_back_when_earlier_argument_is_multiline() {
    // An earlier block lambda forces a break before the last item: the overflow layout is
    // unavailable and the all-or-nothing layout is kept, identical to the option being off.
    let src = "class A{void m(){foo(()->{a();},()->{b();});}}";
    let overflowed = fmt_overflow(src);
    assert_eq!(overflowed, fmt(src));
    expect![[r#"
        class A {
            void m() {
                foo(
                    () -> {
                        a();
                    },
                    () -> {
                        b();
                    }
                );
            }
        }
    "#]]
    .assert_eq(&overflowed);
}

#[test]
fn overflow_falls_back_past_fn_call_width() {
    // The overflow first line `(taskNameLong, () -> {` is 22 columns wide — past a
    // `fn-call-width` of 16 the list breaks vertically, exactly as without the option.
    let cfg = Config {
        overflow_delimited_expr: true,
        fn_call_width: 16,
        ..Config::default()
    };
    expect![[r#"
        class A {
            void m() {
                submit(
                    taskNameLong,
                    () -> {
                        run();
                    }
                );
            }
        }
    "#]]
    .assert_eq(&fmt_with(
        "class A{void m(){submit(taskNameLong,()->{run();});}}",
        &cfg,
    ));
}

#[test]
fn overflow_falls_back_on_comment_between_arguments() {
    // A comment in the leading items needs its own line; the vertical layout provides it and
    // the comment is preserved.
    check_overflow(
        "class A{void m(){foo(a, // note\n()->{run();});}}",
        expect![[r#"
            class A {
                void m() {
                    foo(
                        a,  // note
                        () -> {
                            run();
                        }
                    );
                }
            }
        "#]],
    );
}

#[test]
fn overflow_leaves_expression_bodied_lambda_alone() {
    // Expression-bodied lambdas are not delimited expressions; short calls stay flat and
    // long ones keep the all-or-nothing layout, identical to the option being off.
    let short = "class A{void m(){foo(a,x->x+1);}}";
    assert_eq!(fmt_overflow(short), fmt(short));
    check_overflow(
        short,
        expect![[r#"
            class A {
                void m() {
                    foo(a, x -> x + 1);
                }
            }
        "#]],
    );
    let long = "class A{void m(){perform(aVeryLongArgumentName,anotherLongArgumentName,x->aVeryLongCallTarget(x));}}";
    assert_eq!(fmt_overflow(long), fmt(long));
}

#[test]
fn overflow_ignores_non_trailing_lambda() {
    // Only the LAST argument may hang; a leading block lambda keeps the vertical layout.
    let src = "class A{void m(){foo(()->{a();},b);}}";
    assert_eq!(fmt_overflow(src), fmt(src));
    check_overflow(
        src,
        expect![[r#"
            class A {
                void m() {
                    foo(
                        () -> {
                            a();
                        },
                        b
                    );
                }
            }
        "#]],
    );
}

#[test]
fn overflow_nests() {
    // A hanging lambda body may itself contain an overflowing call.
    check_overflow(
        "class A{void m(){outer(a,()->{inner(b,()->{run();});});}}",
        expect![[r#"
            class A {
                void m() {
                    outer(a, () -> {
                        inner(b, () -> {
                            run();
                        });
                    });
                }
            }
        "#]],
    );
}

#[test]
fn overflow_applies_within_broken_chain_links() {
    // A multi-line lambda still breaks a method chain one call per line; the overflow then
    // applies within each link, hugging the lambda to its call.
    check_overflow(
        "class A{void m(){r=list.stream().map(x->{return x;}).collect(c);}}",
        expect![[r#"
            class A {
                void m() {
                    r = list.stream()
                        .map(x -> {
                            return x;
                        })
                        .collect(c);
                }
            }
        "#]],
    );
}

#[test]
fn overflow_is_idempotent() {
    let src = "class A{void m(){executor.submit(task,()->{run();});}}";
    let once = fmt_overflow(src);
    let twice = fmt_overflow(&once);
    assert_eq!(once, twice, "overflow layout must be idempotent");
}
