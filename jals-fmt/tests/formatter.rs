//! Snapshot tests for the formatter.

use expect_test::{Expect, expect};
use jals_fmt::{BraceStyle, Config, ControlBraceStyle, LineEnding, format_source};

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
