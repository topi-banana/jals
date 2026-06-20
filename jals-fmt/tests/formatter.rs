//! Snapshot tests for the formatter.

use expect_test::{Expect, expect};
use jals_fmt::{
    AnnotationPlacement, BinopLayout, BinopSeparator, BraceStyle, ClosingParen, Config,
    ControlBraceStyle, FloatLiteralTrailingZero, FnParamsLayout, HexLiteralCase, IndentStyle,
    LineEnding, LiteralSuffixCase, SwitchCaseBody, TrailingComma, TypePunctuationDensity,
    format_source,
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

/// Format with `normalize-parameter-comments` enabled (otherwise default config).
fn fmt_param_comments(src: &str) -> String {
    let cfg = Config {
        normalize_parameter_comments: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_param_comments(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_param_comments(src));
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

/// Format with `annotation-placement = expanded` (each leading annotation on its own line).
fn fmt_expanded(src: &str) -> String {
    let cfg = Config {
        annotation_placement: AnnotationPlacement::Expanded,
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
                    f(int[].class);
                    f(String[].class);
                    f(java.lang.String[][].class);
                }
            }
        "#]],
    );
}

#[test]
fn array_method_refs() {
    check(
        "class A{void m(){f(String[]::new);f(int[]::new);f(int[][]::new);f(java.lang.String[][]::new);f(Map.Entry[]::new);}}",
        expect![[r#"
            class A {
                void m() {
                    f(String[]::new);
                    f(int[]::new);
                    f(int[][]::new);
                    f(java.lang.String[][]::new);
                    f(Map.Entry[]::new);
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
fn constructor_call_type_witness() {
    // `new <T>Foo()` keeps a space after `new`; leading `<T>this`/`<T>super` and qualified
    // `t.<T>super()` witnesses round-trip. (These all parse to fresh tree shapes.)
    check(
        "class C{Object a=new <Integer>T<Float>(\"\");C(){<Integer>super(\"x\");}C(int i){<Object>this();}void m(T t){t.<Object>super();}}",
        expect![[r#"
            class C {
                Object a = new <Integer> T<Float>("");
                C() {
                    <Integer> super("x");
                }
                C(int i) {
                    <Object> this();
                }
                void m(T t) {
                    t.<Object>super();
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

// --- closing-paren = hug ----------------------------------------------------

/// Format with `closing-paren = hug` (otherwise default config).
fn fmt_hug(src: &str) -> String {
    let cfg = Config {
        closing_paren: ClosingParen::Hug,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_hug(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_hug(src));
}

#[test]
fn closing_paren_hug_arg_list() {
    // The wrapped call keeps its `)` on the last argument's line instead of dedenting it.
    check_hug(
        "class C{void m(){foooo(aaaaaaaaaaaaaaaa,bbbbbbbbbbbbbbbb,cccccccccccccccc,dddddddddddddddd);}}",
        expect![[r#"
            class C {
                void m() {
                    foooo(
                        aaaaaaaaaaaaaaaa,
                        bbbbbbbbbbbbbbbb,
                        cccccccccccccccc,
                        dddddddddddddddd);
                }
            }
        "#]],
    );
}

#[test]
fn closing_paren_hug_param_list() {
    // A wrapped parameter list also hugs `) {`.
    check_hug(
        "class C{void method(int aaaaaaaaaaaaaaaa,int bbbbbbbbbbbbbbbb,int cccccccccccccccc,int dddddddddddddddd,int eeeeeeeeeeeeeeee){}}",
        expect![[r#"
            class C {
                void method(
                    int aaaaaaaaaaaaaaaa,
                    int bbbbbbbbbbbbbbbb,
                    int cccccccccccccccc,
                    int dddddddddddddddd,
                    int eeeeeeeeeeeeeeee) {}
            }
        "#]],
    );
}

#[test]
fn closing_paren_hug_record_header() {
    // A wrapped record header hugs `) {}`.
    check_hug(
        "record Rrrrrrr(int aaaaaaaaaaaaaaaa,int bbbbbbbbbbbbbbbb,int cccccccccccccccc,int ddddddddddddd){}",
        expect![[r#"
            record Rrrrrrr(
                int aaaaaaaaaaaaaaaa,
                int bbbbbbbbbbbbbbbb,
                int cccccccccccccccc,
                int ddddddddddddd) {}
        "#]],
    );
}

#[test]
fn closing_paren_hug_array_init_unaffected() {
    // The brace-delimited array initializer is never hugged — its `}` stays on its own line.
    check_hug(
        "class C{int[] a={aaaaaaaaaaaaaaaa,bbbbbbbbbbbbbbbb,cccccccccccccccc,dddddddddddddddd,eeeeeeeeeeeeeeee};}",
        expect![[r#"
            class C {
                int[] a = {
                    aaaaaaaaaaaaaaaa,
                    bbbbbbbbbbbbbbbb,
                    cccccccccccccccc,
                    dddddddddddddddd,
                    eeeeeeeeeeeeeeee
                };
            }
        "#]],
    );
}

#[test]
fn closing_paren_hug_overflow_flat_head() {
    // With `overflow-delimited-expr`, when the leading args fit on the call line the trailing
    // lambda overflows and `)` already hugs its `}` (the flat softline is nothing).
    let cfg = Config {
        closing_paren: ClosingParen::Hug,
        overflow_delimited_expr: true,
        ..Config::default()
    };
    let src = "class C{void m(){foooooooooooo(aaaaaaaaaaaaaaaaaaaa,bbbbbbbbbbbbbbbbbbbb,()->{doSomething();});}}";
    expect![[r#"
        class C {
            void m() {
                foooooooooooo(aaaaaaaaaaaaaaaaaaaa, bbbbbbbbbbbbbbbbbbbb, () -> {
                    doSomething();
                });
            }
        }
    "#]]
    .assert_eq(&fmt_with(src, &cfg));
}

#[test]
fn closing_paren_hug_overflow_broken_head() {
    // When the leading args are too wide to share the call line, the overflow group breaks
    // (degenerating to the all-or-nothing layout); hug still cuddles `)` onto the last item's
    // close instead of dedenting it onto its own line.
    let cfg = Config {
        closing_paren: ClosingParen::Hug,
        overflow_delimited_expr: true,
        ..Config::default()
    };
    let src = "class C{void m(){foooooooooooo(aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb,()->{doSomething();});}}";
    expect![[r#"
        class C {
            void m() {
                foooooooooooo(
                    aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,
                    bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb,
                    () -> {
                        doSomething();
                    });
            }
        }
    "#]]
    .assert_eq(&fmt_with(src, &cfg));
}

#[test]
fn closing_paren_hug_is_idempotent() {
    let cfg = Config {
        closing_paren: ClosingParen::Hug,
        ..Config::default()
    };
    let once = fmt_with(
        "class C{void m(){foooo(aaaaaaaaaaaaaaaa,bbbbbbbbbbbbbbbb,cccccccccccccccc,dddddddddddddddd);}}",
        &cfg,
    );
    let twice = fmt_with(&once, &cfg);
    assert_eq!(once, twice, "hug layout must be idempotent");
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
                    foo(); // trailing
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
            class A {} // same
            // below
            /* block */
        "#]],
    );
}

#[test]
fn trailing_line_comment_on_brace_forces_break() {
    // A `//` trailing comment on a closing brace (emitted via `lower_braced`'s `trailing_doc`)
    // must force the line to break before the next token's own trailing comment — otherwise
    // error-recovery shapes glue two `//` comments onto one physical line, where the second is
    // swallowed by the first on re-lex (dropping a comment and breaking idempotency).
    check(
        "class{{}// alpha\nclass// beta\n",
        expect![[r#"
            class { {} // alpha
            class // beta
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

#[test]
fn compact_source_file_top_level_members() {
    // JEP 512: top-level fields and methods (members of the file's implicit class)
    // format like ordinary class members and round-trip idempotently.
    check(
        "int count=0;void main(){System.out.println(count);}",
        expect![[r#"
            int count = 0;
            void main() {
                System.out.println(count);
            }
        "#]],
    );
    let once = fmt("int count=0;void main(){System.out.println(count);}");
    assert_eq!(fmt(&once), once, "format must be idempotent");
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
                int x = 1; // aaaa bbbb cccc dddd eeee ffff gggg
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

// --- empty-item-single-line ------------------------------------------------

/// Format with `empty-item-single-line = false` (defaults otherwise).
fn fmt_no_empty_single_line(src: &str) -> String {
    let cfg = Config {
        empty_item_single_line: false,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

/// Format with `empty-item-single-line = false` and `brace-style = next-line`.
fn fmt_no_empty_single_line_next_line(src: &str) -> String {
    let cfg = Config {
        empty_item_single_line: false,
        brace_style: BraceStyle::NextLine,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn empty_item_single_line_default_collapses_bodies() {
    // By default an empty declaration body collapses to `{}` on the header's line.
    expect![[r#"
        class Foo {}
    "#]]
    .assert_eq(&fmt("class Foo{}"));
}

#[test]
fn empty_item_single_line_off_expands_class_body() {
    expect![[r#"
        class Foo {
        }
    "#]]
    .assert_eq(&fmt_no_empty_single_line("class Foo{}"));
}

#[test]
fn empty_item_single_line_off_expands_member_bodies() {
    // The (non-empty) class body lays out normally; each empty member body expands, its `}`
    // landing at the member's own indent.
    expect![[r#"
        class C {
            void m() {
            }
            C() {
            }
            static {
            }
        }
    "#]]
    .assert_eq(&fmt_no_empty_single_line(
        "class C{void m(){}C(){}static{}}",
    ));
}

#[test]
fn empty_item_single_line_off_expands_every_type_kind() {
    // Every body that lowers to `CLASS_BODY` expands. (Enum bodies are `ENUM_BODY` with their own
    // dedicated lowering; an empty one always collapses to `{}` regardless of this option — see the
    // dedicated test.)
    expect![[r#"
        interface I {
        }
    "#]]
    .assert_eq(&fmt_no_empty_single_line("interface I{}"));
    expect![[r#"
        @interface A {
        }
    "#]]
    .assert_eq(&fmt_no_empty_single_line("@interface A{}"));
    expect![[r#"
        record R(int x) {
        }
    "#]]
    .assert_eq(&fmt_no_empty_single_line("record R(int x){}"));
}

#[test]
fn empty_enum_body_collapses_to_braces() {
    // An empty enum body is `{}` with no inner space, unconditionally — `empty-item-single-line`
    // never governs it (it has its own `ENUM_BODY` lowering), so it is identical with the option
    // on or off.
    expect![["enum E {}\n"]].assert_eq(&fmt_no_empty_single_line("enum E{}"));
    expect![["enum E {}\n"]].assert_eq(&fmt("enum E{}"));
}

#[test]
fn enum_constants_break_one_per_line() {
    // A non-empty enum body always breaks — one constant per line — even when it would fit.
    check(
        "enum Suit { DIAMONDS, HEARTS, CLUBS, SPADES }",
        expect![[r#"
            enum Suit {
                DIAMONDS,
                HEARTS,
                CLUBS,
                SPADES
            }
        "#]],
    );
}

#[test]
fn enum_terminator_glues_to_last_constant() {
    // No trailing comma: the terminator `;` glues onto the last constant.
    check(
        "enum E { ONE, TWO; }",
        expect![[r#"
            enum E {
                ONE,
                TWO;
            }
        "#]],
    );
}

#[test]
fn enum_trailing_comma_keeps_terminator_on_own_line() {
    // A trailing comma keeps the terminator `;` on its own line.
    check(
        "enum E { ONE, TWO,; }",
        expect![[r#"
            enum E {
                ONE,
                TWO,
                ;
            }
        "#]],
    );
}

#[test]
fn enum_no_constants_terminator_on_own_line() {
    // A body that starts with `;` (no constants) puts the terminator on its own line, then the
    // members below it.
    check(
        "public enum E { ; public static final String ONE = \"ONE\"; }",
        expect![[r#"
            public enum E {
                ;
                public static final String ONE = "ONE";
            }
        "#]],
    );
}

#[test]
fn enum_multiple_empty_declarations() {
    // The first `;` is the terminator; each subsequent `;` is an empty-declaration member, one
    // per line.
    check(
        "public enum Empty {;;;}",
        expect![[r#"
            public enum Empty {
                ;
                ;
                ;
            }
        "#]],
    );
}

#[test]
fn enum_preserves_blank_lines_between_constants() {
    // Blank lines between constants are preserved (clamped by the renderer); the blank after `{`
    // before the first constant is dropped, like a class body.
    check(
        "enum E {\n\n  A,\n  B,\n\n  C;\n}",
        expect![[r#"
            enum E {
                A,
                B,

                C;
            }
        "#]],
    );
}

#[test]
fn enum_members_after_terminator() {
    // Members after the terminator format like class-body items, each on its own line (source
    // blank lines preserved — here there were none, so none are inserted).
    check(
        "enum E { A, B; int x; E() {} }",
        expect![[r#"
            enum E {
                A,
                B;
                int x;
                E() {}
            }
        "#]],
    );
}

#[test]
fn enum_constant_with_class_body() {
    // A constant's class body recurses through the block formatter.
    check(
        "enum E { A { void m() {} }, B; }",
        expect![[r#"
            enum E {
                A {
                    void m() {}
                },
                B;
            }
        "#]],
    );
}

#[test]
fn enum_constant_annotations_expanded_vs_compact() {
    // Under `annotation-placement = expanded` each leading annotation breaks onto its own line;
    // under the default `compact` the constant stays inline.
    expect![[r#"
        enum E {
            @A
            ONE,
            TWO,
            @B
            @C
            THREE;
        }
    "#]]
    .assert_eq(&fmt_expanded("enum E { @A ONE, TWO, @B @C THREE; }"));
    expect![[r#"
        enum E {
            @A ONE,
            TWO,
            @B @C THREE;
        }
    "#]]
    .assert_eq(&fmt("enum E { @A ONE, TWO, @B @C THREE; }"));
}

#[test]
fn enum_comment_before_terminator_keeps_it_on_own_line() {
    // A leading comment on the terminator `;` blocks gluing (gluing would relocate the comment
    // above the constant), so the `;` stays on its own line and the comment is preserved.
    check(
        "enum E { ONE, TWO\n // keep\n ; }",
        expect![[r#"
            enum E {
                ONE,
                TWO
                // keep
                ;
            }
        "#]],
    );
}

#[test]
fn enum_dangling_comment_before_brace() {
    // A comment on its own line with no following token dangles before the closing brace and is
    // still emitted.
    check(
        "enum E { ONE;\n /* dangling */\n }",
        expect![[r#"
            enum E {
                ONE;
                /* dangling */
            }
        "#]],
    );
}

#[test]
fn empty_item_single_line_off_keeps_control_flow_collapsed() {
    // Control-flow / switch / bare blocks are never governed: they always stay `{}`.
    expect![[r#"
        class C {
            void m() {
                if (a) {}
                while (b) {}
                switch (x) {}
                {}
            }
        }
    "#]]
    .assert_eq(&fmt_no_empty_single_line(
        "class C{void m(){if(a){}while(b){}switch(x){}{}}}",
    ));
}

#[test]
fn empty_item_single_line_off_keeps_lambda_collapsed() {
    // A lambda body is not a declaration body, so an empty one stays `{}`.
    expect![[r#"
        class C {
            Runnable r = () -> {};
        }
    "#]]
    .assert_eq(&fmt_no_empty_single_line("class C{Runnable r=()->{};}"));
}

#[test]
fn empty_item_single_line_off_preserves_dangling_comment_body() {
    // A body whose only content is a comment dangling on `}` already takes the multi-line
    // path (it is not "empty"), so the option does not govern it and the comment is kept.
    expect![[r#"
        class Foo {
            // x
        }
    "#]]
    .assert_eq(&fmt_no_empty_single_line("class Foo{\n// x\n}"));
}

#[test]
fn empty_item_single_line_off_next_line_opens_brace_on_own_line() {
    expect![[r#"
        class Foo
        {
        }
    "#]]
    .assert_eq(&fmt_no_empty_single_line_next_line("class Foo{}"));
    expect![[r#"
        class C
        {
            void m()
            {
            }
        }
    "#]]
    .assert_eq(&fmt_no_empty_single_line_next_line("class C{void m(){}}"));
}

#[test]
fn empty_item_single_line_off_is_idempotent() {
    let once = fmt_no_empty_single_line("class C{void m(){}C(){}static{}class Inner{}}");
    let twice = fmt_no_empty_single_line(&once);
    assert_eq!(
        once, twice,
        "empty-item-single-line = false must be idempotent"
    );
}

// --- fn-single-line --------------------------------------------------------

/// Format with `fn-single-line = true` (defaults otherwise).
fn fmt_fn_single_line(src: &str) -> String {
    let cfg = Config {
        fn_single_line: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn fn_single_line_collapses_single_statement_method() {
    expect![[r#"
        class C {
            int foo() { return 1; }
        }
    "#]]
    .assert_eq(&fmt_fn_single_line("class C{int foo(){return 1;}}"));
}

#[test]
fn fn_single_line_collapses_constructor_and_initializer() {
    // The option governs every declaration body: methods, constructors, and initializers.
    expect![[r#"
        class C {
            C() { this.x = 1; }
            static { init(); }
        }
    "#]]
    .assert_eq(&fmt_fn_single_line(
        "class C{C(){this.x=1;}static{init();}}",
    ));
}

#[test]
fn fn_single_line_keeps_multi_statement_body_multiline() {
    // Two statements never collapse, even when they would fit on one line.
    expect![[r#"
        class C {
            void bar() {
                a();
                b();
            }
        }
    "#]]
    .assert_eq(&fmt_fn_single_line("class C{void bar(){a();b();}}"));
}

#[test]
fn fn_single_line_keeps_overflowing_body_multiline() {
    // A single statement that would overflow `max-width` falls back to the multi-line body.
    let cfg = Config {
        fn_single_line: true,
        max_width: 40,
        ..Config::default()
    };
    expect![[r#"
        class C {
            int wide() {
                return aVeryLongMethodCallThatOverflows();
            }
        }
    "#]]
    .assert_eq(
        &format_source(
            "class C{int wide(){return aVeryLongMethodCallThatOverflows();}}",
            &cfg,
        )
        .formatted,
    );
}

#[test]
fn fn_single_line_keeps_nested_block_body_multiline() {
    // A nested block forces a break inside the body, so it is never collapsed.
    expect![[r#"
        class C {
            void m() {
                if (x) {
                    y();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_fn_single_line("class C{void m(){if(x){y();}}}"));
}

#[test]
fn fn_single_line_keeps_commented_body_multiline() {
    // A body carrying a comment is never collapsed (comments must stay on their anchor).
    expect![[r#"
        class C {
            void m() {
                // keep
                return;
            }
        }
    "#]]
    .assert_eq(&fmt_fn_single_line("class C{void m(){\n// keep\nreturn;}}"));
}

#[test]
fn fn_single_line_keeps_body_multiline_when_header_has_trailing_comment() {
    // A trailing comment on a header token (here, the return type) renders as a line suffix that
    // flushes at the body's first newline. Collapsing the body to one line would relocate it past
    // the closing brace, re-anchoring it on the next parse and breaking idempotency — so the body
    // stays multi-line and the comment keeps its place.
    expect![[r#"
        class C {
            int foo() { /*c*/
                return 1;
            }
        }
    "#]]
    .assert_eq(&fmt_fn_single_line("class C{int/*c*/foo(){return 1;}}"));
}

#[test]
fn fn_single_line_header_trailing_comment_is_idempotent() {
    // The header-trailing-comment case must be idempotent (it previously collapsed once, then
    // expanded — the comment having moved past the brace flipped the collapse decision).
    let src = "class C{int foo()/*c*/{return 1;}}";
    let once = fmt_fn_single_line(src);
    let twice = fmt_fn_single_line(&once);
    assert_eq!(
        once, twice,
        "header-trailing-comment collapse must be idempotent"
    );
}

#[test]
fn fn_single_line_next_line_collapses_when_fits_else_opens_brace() {
    // Under `brace-style = next-line` a fitting single-statement body still collapses to one
    // line; an overflowing one falls back to the next-line brace layout.
    let cfg = Config {
        fn_single_line: true,
        brace_style: BraceStyle::NextLine,
        max_width: 40,
        ..Config::default()
    };
    expect![[r#"
        class C
        {
            int foo() { return 1; }
            int wide()
            {
                return aVeryLongMethodCallThatOverflows();
            }
        }
    "#]]
    .assert_eq(
        &format_source(
            "class C{int foo(){return 1;}int wide(){return aVeryLongMethodCallThatOverflows();}}",
            &cfg,
        )
        .formatted,
    );
}

#[test]
fn fn_single_line_off_by_default_keeps_body_multiline() {
    // With the option off (the default) a single-statement body stays multi-line.
    expect![[r#"
        class C {
            int foo() {
                return 1;
            }
        }
    "#]]
    .assert_eq(&fmt("class C{int foo(){return 1;}}"));
}

#[test]
fn fn_single_line_collapses_braceless_control_statement() {
    // The rule is "one statement with no forced break", not "one expression statement": a
    // braceless control statement is a single statement and collapses when it fits. A bare or
    // braced control block forces a break (see the nested-block test) and never collapses.
    expect![[r#"
        class C {
            void m() { if (a) only(); }
        }
    "#]]
    .assert_eq(&fmt_fn_single_line("class C{void m(){if(a)only();}}"));
}

#[test]
fn fn_single_line_is_idempotent() {
    let once =
        fmt_fn_single_line("class C{int foo(){return 1;}void bar(){a();b();}static{init();}}");
    let twice = fmt_fn_single_line(&once);
    assert_eq!(once, twice, "fn-single-line = true must be idempotent");
}

// --- force-multiline-blocks ------------------------------------------------

/// Format with `force-multiline-blocks = true` (defaults otherwise).
fn fmt_force_multiline_blocks(src: &str) -> String {
    let cfg = Config {
        force_multiline_blocks: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn force_multiline_blocks_expands_empty_type_body() {
    // An empty type body expands to a two-line `{` … `}` instead of collapsing to `{}`,
    // overriding the default `empty-item-single-line`.
    expect![[r#"
        class C {
        }
    "#]]
    .assert_eq(&fmt_force_multiline_blocks("class C{}"));
}

#[test]
fn force_multiline_blocks_expands_empty_method_body() {
    // An empty method / constructor / initializer body expands too.
    expect![[r#"
        class C {
            void m() {
            }
            C() {
            }
            static {
            }
        }
    "#]]
    .assert_eq(&fmt_force_multiline_blocks(
        "class C{void m(){}C(){}static{}}",
    ));
}

#[test]
fn force_multiline_blocks_expands_empty_control_flow_block() {
    // Goes beyond `empty-item-single-line` (declaration-only): an empty control-flow block,
    // which would otherwise always keep `{}`, also expands.
    expect![[r#"
        class C {
            void m() {
                if (true) {
                }
                while (x) {
                }
            }
        }
    "#]]
    .assert_eq(&fmt_force_multiline_blocks(
        "class C{void m(){if(true){}while(x){}}}",
    ));
}

#[test]
fn force_multiline_blocks_expands_empty_lambda_block() {
    // An empty lambda block expands as well.
    expect![[r#"
        class C {
            Runnable r = () -> {
            };
        }
    "#]]
    .assert_eq(&fmt_force_multiline_blocks(
        "class C{Runnable r = () -> {};}",
    ));
}

#[test]
fn force_multiline_blocks_overrides_fn_single_line() {
    // When both are on, `force-multiline-blocks` wins: a single-statement body never collapses.
    let cfg = Config {
        force_multiline_blocks: true,
        fn_single_line: true,
        ..Config::default()
    };
    expect![[r#"
        class C {
            int foo() {
                return 1;
            }
        }
    "#]]
    .assert_eq(&format_source("class C{int foo(){return 1;}}", &cfg).formatted);
}

#[test]
fn force_multiline_blocks_next_line_empty_body_opens_brace() {
    // Under `brace-style = next-line` an expanded empty declaration body opens its brace on its
    // own line.
    let cfg = Config {
        force_multiline_blocks: true,
        brace_style: BraceStyle::NextLine,
        ..Config::default()
    };
    expect![[r#"
        class C
        {
            void m()
            {
            }
        }
    "#]]
    .assert_eq(&format_source("class C{void m(){}}", &cfg).formatted);
}

#[test]
fn force_multiline_blocks_control_next_line_empty_block_opens_brace() {
    // Under `control-brace-style = next-line` an expanded empty control-flow block opens its
    // brace on its own line too.
    let cfg = Config {
        force_multiline_blocks: true,
        control_brace_style: ControlBraceStyle::NextLine,
        ..Config::default()
    };
    expect![[r#"
        class C {
            void m() {
                if (true)
                {
                }
            }
        }
    "#]]
    .assert_eq(&format_source("class C{void m(){if(true){}}}", &cfg).formatted);
}

#[test]
fn force_multiline_blocks_off_by_default_collapses_empties() {
    // With the option off (the default) empty bodies still collapse to `{}`.
    expect![[r#"
        class C {
            void m() {}
        }
    "#]]
    .assert_eq(&fmt("class C{void m(){}}"));
}

#[test]
fn force_multiline_blocks_is_idempotent() {
    let once = fmt_force_multiline_blocks(
        "class C{void m(){}C(){}Runnable r = () -> {};void n(){if(true){}}}",
    );
    let twice = fmt_force_multiline_blocks(&once);
    assert_eq!(
        once, twice,
        "force-multiline-blocks = true must be idempotent"
    );
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
            int[] x = {
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
            int[] x = {alpha, beta, gamma};
        }
    "#]]
    .assert_eq(&fmt_array_init("class A{int[] x={alpha,beta,gamma};}", 200));
}

#[test]
fn array_width_breaks_new_array_creation() {
    // `new T[]{…}` carries the same ARRAY_INIT node, so it honors array-width too.
    expect![[r#"
        class A {
            int[] x = new int[] {
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
            int[] x = {a, b, c};
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
            int[] x = {a, b, c,};
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
            int[] x = {a, b, c,};
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
            int[] x = {a, b, c};
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
            int[] x = {a, b, c};
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
            int[] x = {
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
            int[] x = {
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
            int[] x = {
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
            int[] x = {a, b, c,}; /* keep */
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

#[test]
fn trailing_comma_unclosed_array_is_not_synthesized() {
    // Regression: an array initializer left unclosed by error recovery (no `}`) must NOT gain a
    // synthesized trailing comma. With no closing brace the comma would not be trailing — on a
    // re-parse it reads as an item separator that pulls the following token into the list,
    // breaking idempotency. The source is preserved exactly here (no comma added after `beta`).
    expect![[r#"
        class A { int[] x = {
            alpha,
            beta
    "#]]
    .assert_eq(&fmt_trailing_narrow(
        "class A{int[] x={alpha,beta",
        TrailingComma::Vertical,
    ));
}

#[test]
fn trailing_comma_unclosed_array_is_idempotent() {
    // The non-idempotency this guards against only appears across two passes, so assert both
    // modes that synthesize a comma reach a fixed point on an unclosed initializer.
    for mode in [TrailingComma::Always, TrailingComma::Vertical] {
        let src = "class A{int[] x={alpha,beta";
        let once = fmt_trailing_narrow(src, mode);
        let twice = fmt_trailing_narrow(&once, mode);
        assert_eq!(
            once, twice,
            "unclosed-array trailing-comma {mode:?} must be idempotent"
        );
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
            import a.A; // trail for a
            import b.B;
            class C {}
        "#]],
    );
}

#[test]
fn module_import_formats_inline() {
    // `import module M;` (JEP 511) lays out with normal spacing under the default config.
    check(
        "import module java.base;class C{}",
        expect![[r#"
            import module java.base;
            class C {}
        "#]],
    );
}

#[test]
fn module_decl_empty_body_collapses() {
    // A module declaration's body is a braced declaration body like a class body: an empty one
    // collapses to `{}` (not `{ }`) under the default `empty-item-single-line`.
    check(
        "open module com.google.m { }",
        expect![[r#"
            open module com.google.m {}
        "#]],
    );
    check(
        "module com.google.m {}",
        expect![[r#"
            module com.google.m {}
        "#]],
    );
}

#[test]
fn module_decl_directives_break_and_indent() {
    // A non-empty module body lays its directives out one per indented line, like class members.
    check(
        "module com.google.m { requires java.base; exports com.foo; }",
        expect![[r#"
            module com.google.m {
                requires java.base;
                exports com.foo;
            }
        "#]],
    );
}

#[test]
fn module_decl_empty_body_next_line_brace() {
    // Like a type body, the module body's brace follows `brace-style`: an empty body still
    // collapses to `{}` under `next-line` (matching the class-body behavior).
    let cfg = Config {
        brace_style: BraceStyle::NextLine,
        ..Config::default()
    };
    expect![[r#"
        module com.google.m {}
    "#]]
    .assert_eq(&fmt_with("module com.google.m { }", &cfg));
}

#[test]
fn reorder_imports_module_to_front() {
    // Module imports lead their own tier: module, then ordinary (alphabetical), then static.
    check_reorder(
        "import b.B;import module java.base;import static a.A.a;import a.A;class C{}",
        expect![[r#"
            import module java.base;
            import a.A;
            import b.B;
            import static a.A.a;
            class C {}
        "#]],
    );
}

#[test]
fn reorder_imports_module_comment_follows() {
    // A leading comment glued to a module import moves with it when it is reordered to the front.
    check_reorder(
        "import b.B;\n// lead for mod\nimport module java.base;\nclass C {}\n",
        expect![[r#"
            // lead for mod
            import module java.base;
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
fn group_imports_module_block_first() {
    // Module imports cluster in a leading block, before every prefix group and the static block.
    check_group(
        "import static a.A.a;import com.foo.Bar;import module java.base;import java.util.List;class C{}",
        expect![[r#"
            import module java.base;

            import java.util.List;

            import com.foo.Bar;

            import static a.A.a;
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
            import java.a.A; // trail

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
                    + // why
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
                x = a + // why
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

fn fmt_binop_layout(
    src: &str,
    max_width: usize,
    binop_separator: BinopSeparator,
    binop_layout: BinopLayout,
) -> String {
    let cfg = Config {
        max_width,
        binop_separator,
        binop_layout,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn compressed_layout_packs_operands_per_line() {
    // The same input as `long_binary_wraps_operator_front_by_default`, but `binop-layout =
    // compressed`: instead of one operand per line, operands pack up to max-width(100) and only
    // the overflowing operand starts a new line, the operator leading it (google-java-format's
    // layout).
    expect![[r#"
        class A {
            void m() {
                result = alphaOperandName + betaOperandName + gammaOperandName + deltaOperandName
                    + epsilonOperandName;
            }
        }
    "#]]
    .assert_eq(&fmt_binop_layout(
        "class A{void m(){result=alphaOperandName+betaOperandName+gammaOperandName+deltaOperandName+epsilonOperandName;}}",
        100,
        BinopSeparator::Front,
        BinopLayout::Compressed,
    ));
}

#[test]
fn compressed_layout_packs_each_precedence_level() {
    // Mixed precedence: the outer `||` and the inner `&&` each fill independently; an `==` unit
    // that fits stays whole on its line.
    expect![[r#"
        class A {
            void m() {
                flag = aLongName == bLongName && cLongName == dLongName
                    || eLongName == fLongName;
            }
        }
    "#]]
    .assert_eq(&fmt_binop_layout(
        "class A{void m(){flag=aLongName==bLongName&&cLongName==dLongName||eLongName==fLongName;}}",
        65,
        BinopSeparator::Front,
        BinopLayout::Compressed,
    ));
}

#[test]
fn compressed_layout_is_idempotent() {
    // Re-formatting the packed output reproduces the same wrapping, under both separators.
    let src = "class A{void m(){total=aa+bb+cc+dd+ee+ff+gg+hh+ii+jj+kk+ll+mm+nn+oo+pp+qq+rr+ss;}}";
    for sep in [BinopSeparator::Front, BinopSeparator::Back] {
        let once = fmt_binop_layout(src, 40, sep, BinopLayout::Compressed);
        let twice = fmt_binop_layout(&once, 40, sep, BinopLayout::Compressed);
        assert_eq!(
            once, twice,
            "compressed binop layout must be idempotent ({sep:?})"
        );
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
                fill(buf, new int[] {
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
                fill(buf, new int[] {
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
                        a, // note
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

// ---------------------------------------------------------------------------
// Ternary wrapping (`single-line-if-else-max-width`)
// ---------------------------------------------------------------------------

fn fmt_ternary(src: &str, width: usize) -> String {
    let cfg = Config {
        single_line_if_else_max_width: width,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn fmt_ternary_back(src: &str, width: usize) -> String {
    let cfg = Config {
        single_line_if_else_max_width: width,
        binop_separator: BinopSeparator::Back,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn ternary_within_width_stays_flat() {
    // A ternary whose flat width fits the budget keeps the inline form, with the `:` spacing
    // following the colon options (default: no space before, one after).
    expect![[r#"
        class C {
            int m() {
                return x > 0 ? 1: 2;
            }
        }
    "#]]
    .assert_eq(&fmt_ternary("class C{int m(){return x>0?1:2;}}", 50));
}

#[test]
fn ternary_exceeding_width_wraps_front() {
    // Past the budget the ternary wraps, `?` and `:` leading the continuation lines (front).
    expect![[r#"
        class C {
            int m() {
                return someCondition
                    ? thisIsARatherLongThenExpression
                    : theElseBranchValue;
            }
        }
    "#]]
    .assert_eq(&fmt_ternary(
        "class C{int m(){return someCondition?thisIsARatherLongThenExpression:theElseBranchValue;}}",
        50,
    ));
}

#[test]
fn ternary_exceeding_width_wraps_back() {
    // The same source under `binop-separator = back`: `?` and `:` trail the broken lines.
    expect![[r#"
        class C {
            int m() {
                return someCondition ?
                    thisIsARatherLongThenExpression:
                    theElseBranchValue;
            }
        }
    "#]]
    .assert_eq(&fmt_ternary_back(
        "class C{int m(){return someCondition?thisIsARatherLongThenExpression:theElseBranchValue;}}",
        50,
    ));
}

#[test]
fn ternary_zero_width_always_wraps() {
    // A width of `0` forces even a tiny ternary to wrap.
    expect![[r#"
        class C {
            int m() {
                return x > 0
                    ? 1
                    : 2;
            }
        }
    "#]]
    .assert_eq(&fmt_ternary("class C{int m(){return x>0?1:2;}}", 0));
}

#[test]
fn ternary_wrap_respects_colon_spacing() {
    // The wrapped `:` line honors `space-before-colon`: with it on, a space precedes the `:`.
    let cfg = Config {
        single_line_if_else_max_width: 0,
        space_before_colon: true,
        ..Config::default()
    };
    expect![[r#"
        class C {
            int m() {
                return x > 0
                    ? 1
                    : 2;
            }
        }
    "#]]
    .assert_eq(&fmt_with("class C{int m(){return x>0?1:2;}}", &cfg));
}

#[test]
fn ternary_nested_wraps_independently() {
    // A nested ternary (the else branch is itself a ternary) is its own group; each wraps when
    // it exceeds the budget.
    expect![[r#"
        class C {
            int m() {
                return firstCondition
                    ? firstValueExpression
                    : secondConditionHere
                        ? secondValueExpression
                        : thirdFallbackValue;
            }
        }
    "#]]
    .assert_eq(&fmt_ternary(
        "class C{int m(){return firstCondition?firstValueExpression:secondConditionHere?secondValueExpression:thirdFallbackValue;}}",
        50,
    ));
}

#[test]
fn ternary_wrap_is_idempotent() {
    let src = "class C{int m(){return someCondition?thisIsARatherLongThenExpression:theElseBranchValue;}}";
    let once = fmt_ternary(src, 50);
    let twice = fmt_ternary(&once, 50);
    assert_eq!(once, twice, "ternary wrapping must be idempotent");
}

// ---------------------------------------------------------------------------
// Colon spacing (`space-before-colon` / `space-after-colon`)
// ---------------------------------------------------------------------------

fn fmt_colon(src: &str, space_before_colon: bool, space_after_colon: bool) -> String {
    let config = Config {
        space_before_colon,
        space_after_colon,
        // Keep the legacy `case x:` body inline so these tests stay focused on colon *spacing*
        // (and double as `same-line` coverage); the default `Always` would break the body out.
        switch_case_body: SwitchCaseBody::SameLine,
        ..Config::default()
    };
    fmt_with(src, &config)
}

/// A source exercising every Java colon context: a ternary, an enhanced `for`, a labeled
/// statement, an `assert` message, and `case` / `default` switch labels.
const COLON_SRC: &str = "class C{void m(int x){int y=x>0?1:2;for(int i:list){use(i);}outer:for(;;){break outer;}assert x>0:\"m\";switch(x){case 1:a();break;case 2:case 3:b();break;default:c();}}}";

#[test]
fn colon_default_no_space_before_one_space_after() {
    // Defaults (`space-before-colon = false`, `space-after-colon = true`) give idiomatic
    // `label:` / `case x:` spacing, applied uniformly to ternary / for-each / assert too.
    expect![[r#"
        class C {
            void m(int x) {
                int y = x > 0 ? 1: 2;
                for (int i: list) {
                    use(i);
                }
                outer: for (;;) {
                    break outer;
                }
                assert x > 0: "m";
                switch (x) {
                    case 1: a(); break;
                    case 2: case 3: b(); break;
                    default: c();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_colon(COLON_SRC, false, true));
}

#[test]
fn colon_space_before_adds_space_in_every_context() {
    // `space-before-colon = true` puts a space before every colon, uniformly.
    expect![[r#"
        class C {
            void m(int x) {
                int y = x > 0 ? 1 : 2;
                for (int i : list) {
                    use(i);
                }
                outer : for (;;) {
                    break outer;
                }
                assert x > 0 : "m";
                switch (x) {
                    case 1 : a(); break;
                    case 2 : case 3 : b(); break;
                    default : c();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_colon(COLON_SRC, true, true));
}

#[test]
fn colon_no_space_after_tightens_every_context() {
    // `space-after-colon = false` removes the space after every colon, uniformly.
    expect![[r#"
        class C {
            void m(int x) {
                int y = x > 0 ? 1:2;
                for (int i:list) {
                    use(i);
                }
                outer:for (;;) {
                    break outer;
                }
                assert x > 0:"m";
                switch (x) {
                    case 1:a(); break;
                    case 2:case 3:b(); break;
                    default:c();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_colon(COLON_SRC, false, false));
}

#[test]
fn colon_method_reference_is_never_affected() {
    // `::` is a distinct token (`COLON_COLON`); colon spacing never touches it, and the
    // fusion-safety net keeps a ternary colon from joining a following `::` into `:::`.
    check(
        "class C{void m(){var r=cond?Foo::bar:Baz::qux;}}",
        expect![[r#"
            class C {
                void m() {
                    var r = cond ? Foo::bar: Baz::qux;
                }
            }
        "#]],
    );
}

#[test]
fn colon_spacing_is_idempotent() {
    for before in [false, true] {
        for after in [false, true] {
            let once = fmt_colon(COLON_SRC, before, after);
            let twice = fmt_colon(&once, before, after);
            assert_eq!(
                once, twice,
                "colon spacing must be idempotent (before={before}, after={after})"
            );
        }
    }
}

fn fmt_operator_colon(src: &str) -> String {
    let config = Config {
        space_around_operator_colon: true,
        // Keep the legacy `case x:` body inline so the output stays focused on colon *spacing*;
        // the default `Always` would break the body out.
        switch_case_body: SwitchCaseBody::SameLine,
        ..Config::default()
    };
    fmt_with(src, &config)
}

#[test]
fn operator_colon_spaces_only_for_each_ternary_and_assert() {
    // `space-around-operator-colon` adds a space before the colon that separates two operands —
    // the ternary, the enhanced `for`, and the `assert` message — while the label colons (a
    // labeled statement, a `switch` `case` / `default`) keep hugging, following
    // `space-before-colon` (off here). This is google-java-format's colon spacing.
    expect![[r#"
        class C {
            void m(int x) {
                int y = x > 0 ? 1 : 2;
                for (int i : list) {
                    use(i);
                }
                outer: for (;;) {
                    break outer;
                }
                assert x > 0 : "m";
                switch (x) {
                    case 1: a(); break;
                    case 2: case 3: b(); break;
                    default: c();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_operator_colon(COLON_SRC));
}

#[test]
fn operator_colon_hugs_an_unnamed_for_each_variable() {
    // google-java-format spaces a named for-each colon (`for (Order a : ys)`) but hugs the colon
    // of an unnamed `_` loop variable (`for (Order _: xs)`); the `UNDERSCORE` token before the
    // colon suppresses the space even with `space-around-operator-colon` on.
    expect![[r#"
        class C {
            void m() {
                for (Order _: xs) {}
                for (Order a : ys) {}
            }
        }
    "#]]
    .assert_eq(&fmt_operator_colon(
        "class C{void m(){for(Order _:xs){}for(Order a:ys){}}}",
    ));
}

#[test]
fn operator_colon_spacing_is_idempotent() {
    let once = fmt_operator_colon(COLON_SRC);
    let twice = fmt_operator_colon(&once);
    assert_eq!(once, twice, "operator-colon spacing must be idempotent");
}

// ---------------------------------------------------------------------------
// Type-punctuation density (`type-punctuation-density`)
// ---------------------------------------------------------------------------

fn fmt_type_punct(src: &str, density: TypePunctuationDensity) -> String {
    let config = Config {
        type_punctuation_density: density,
        ..Config::default()
    };
    fmt_with(src, &config)
}

/// A source exercising every intersection-type `&` context — a single-bound and a multi-bound
/// type parameter, and a cast intersection — alongside a bitwise-AND expression that must stay
/// untouched.
const TYPE_PUNCT_SRC: &str = "class C<T extends A & B>{<U extends X & Y & Z> void m(){Object o=(A & B) x;int z=a & b;boolean f=(p && q) & r;}}";

#[test]
fn type_punctuation_density_wide_keeps_spaces() {
    // `wide` (the default) keeps a space around every intersection `&`, matching prior behavior.
    expect![[r#"
        class C<T extends A & B> {
            <U extends X & Y & Z> void m() {
                Object o = (A & B) x;
                int z = a & b;
                boolean f = (p && q) & r;
            }
        }
    "#]]
    .assert_eq(&fmt_type_punct(
        TYPE_PUNCT_SRC,
        TypePunctuationDensity::Wide,
    ));
}

#[test]
fn type_punctuation_density_compressed_tightens_only_type_amp() {
    // `compressed` removes the space around `&` in type-parameter bounds and cast
    // intersections, but never touches the bitwise-AND operator (`a & b`, `(p && q) & r`).
    expect![[r#"
        class C<T extends A&B> {
            <U extends X&Y&Z> void m() {
                Object o = (A&B) x;
                int z = a & b;
                boolean f = (p && q) & r;
            }
        }
    "#]]
    .assert_eq(&fmt_type_punct(
        TYPE_PUNCT_SRC,
        TypePunctuationDensity::Compressed,
    ));
}

#[test]
fn type_punctuation_density_is_idempotent() {
    for density in [
        TypePunctuationDensity::Wide,
        TypePunctuationDensity::Compressed,
    ] {
        let once = fmt_type_punct(TYPE_PUNCT_SRC, density);
        let twice = fmt_type_punct(&once, density);
        assert_eq!(
            once, twice,
            "type-punctuation density must be idempotent ({density:?})"
        );
    }
}

// ----- fn-params-layout -------------------------------------------------------------------

/// Format with a given parameter-list layout at the default width.
fn fmt_params(src: &str, layout: FnParamsLayout) -> String {
    let cfg = Config {
        fn_params_layout: layout,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

/// Format with a parameter-list layout and a narrow `max-width` to force wrapping.
fn fmt_params_narrow(src: &str, layout: FnParamsLayout, max_width: usize) -> String {
    let cfg = Config {
        fn_params_layout: layout,
        max_width,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

/// A four-parameter method whose flat signature fits the default width but not a narrow one.
const PARAMS_SRC: &str = "class A{void m(int alpha,String beta,long gamma,double delta){}}";

#[test]
fn fn_params_tall_keeps_one_line_when_it_fits() {
    // Tall (the default): a parameter list that fits stays on one line.
    expect![[r#"
        class A {
            void m(int alpha, String beta, long gamma, double delta) {}
        }
    "#]]
    .assert_eq(&fmt_params(PARAMS_SRC, FnParamsLayout::Tall));
}

#[test]
fn fn_params_tall_breaks_all_or_nothing() {
    // Tall under a narrow width: all-or-nothing, one parameter per line.
    expect![[r#"
        class A {
            void m(
                int alpha,
                String beta,
                long gamma,
                double delta
            ) {}
        }
    "#]]
    .assert_eq(&fmt_params_narrow(PARAMS_SRC, FnParamsLayout::Tall, 40));
}

#[test]
fn fn_params_vertical_breaks_even_when_it_fits() {
    // Vertical: one parameter per line even though the list would fit on one line.
    expect![[r#"
        class A {
            void m(
                int alpha,
                String beta,
                long gamma,
                double delta
            ) {}
        }
    "#]]
    .assert_eq(&fmt_params(PARAMS_SRC, FnParamsLayout::Vertical));
}

#[test]
fn fn_params_vertical_single_param_still_breaks() {
    // A single parameter still goes on its own line under Vertical.
    expect![[r#"
        class A {
            void m(
                int only
            ) {}
        }
    "#]]
    .assert_eq(&fmt_params(
        "class A{void m(int only){}}",
        FnParamsLayout::Vertical,
    ));
}

#[test]
fn fn_params_vertical_empty_list_stays_inline() {
    // An empty parameter list has nothing to break: it stays `()`.
    expect![[r#"
        class A {
            void m() {}
        }
    "#]]
    .assert_eq(&fmt_params("class A{void m(){}}", FnParamsLayout::Vertical));
}

#[test]
fn fn_params_compressed_packs_as_many_as_fit() {
    // Compressed under a narrow width: pack parameters per line, wrapping at the width.
    expect![[r#"
        class A {
            void m(
                int alpha, String beta,
                long gamma, double delta
            ) {}
        }
    "#]]
    .assert_eq(&fmt_params_narrow(
        PARAMS_SRC,
        FnParamsLayout::Compressed,
        40,
    ));
}

#[test]
fn fn_params_compressed_keeps_one_line_when_it_fits() {
    // Compressed that fits stays on one line, just like Tall.
    expect![[r#"
        class A {
            void m(int alpha, String beta, long gamma, double delta) {}
        }
    "#]]
    .assert_eq(&fmt_params(PARAMS_SRC, FnParamsLayout::Compressed));
}

#[test]
fn fn_params_layout_only_affects_params_not_call_args() {
    // Vertical breaks the *parameter* list but leaves a call's *argument* list inline.
    expect![[r#"
        class A {
            void m(
                int a,
                int b
            ) {
                f(x, y, z);
            }
        }
    "#]]
    .assert_eq(&fmt_params(
        "class A{void m(int a,int b){f(x,y,z);}}",
        FnParamsLayout::Vertical,
    ));
}

#[test]
fn fn_params_layout_modes_are_idempotent() {
    for layout in [
        FnParamsLayout::Tall,
        FnParamsLayout::Compressed,
        FnParamsLayout::Vertical,
    ] {
        let once = fmt_params_narrow(PARAMS_SRC, layout, 40);
        let twice = fmt_params_narrow(&once, layout, 40);
        assert_eq!(
            once, twice,
            "fn-params-layout {layout:?} must be idempotent"
        );
    }
}

#[test]
fn type_use_annotation_inner_type() {
    // JSR 308 inner-type annotation: the `.` hugs the following annotation (`Outer.@A Inner`).
    check(
        "class C{Outer. @A Inner f(){return null;}}",
        expect![[r#"
            class C {
                Outer.@A Inner f() {
                    return null;
                }
            }
        "#]],
    );
}

#[test]
fn type_use_annotation_wildcard() {
    // JSR 308 annotation before a wildcard `?`.
    check(
        "class C{MyList<@A ?> f(){return null;}}",
        expect![[r#"
            class C {
                MyList<@A ?> f() {
                    return null;
                }
            }
        "#]],
    );
}

#[test]
fn type_use_annotation_varargs() {
    // JSR 308 annotation on a varargs element type (`Object @A...`).
    check(
        "class C{void m(Object @A ... xs){}}",
        expect![[r#"
            class C {
                void m(Object @A... xs) {}
            }
        "#]],
    );
}

#[test]
fn type_use_annotation_cast() {
    // JSR 308 annotated type in a cast (`(@A Long) y`).
    check(
        "class C{Object m(){return (@A Long) y;}}",
        expect![[r#"
            class C {
                Object m() {
                    return (@A Long) y;
                }
            }
        "#]],
    );
}

#[test]
fn non_sealed_modifier_renders_tight() {
    // `non-sealed` is one keyword: its `non` `-` `sealed` tokens stay tight (not `non - sealed`).
    check(
        "class C{public sealed class S permits N{}non-sealed class N extends S{}}",
        expect![[r#"
            class C {
                public sealed class S permits N {}
                non-sealed class N extends S {}
            }
        "#]],
    );
}

// ===== reorder-modifiers =====

/// Format with `reorder-modifiers` enabled.
fn fmt_reorder_mods(src: &str) -> String {
    let cfg = Config {
        reorder_modifiers: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_reorder_mods(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_reorder_mods(src));
}

#[test]
fn reorder_modifiers_sorts_type_method_and_field() {
    check_reorder_mods(
        "final public class C{static private final int x=0;synchronized public void m(){}}",
        expect![[r#"
            public final class C {
                private static final int x = 0;
                public synchronized void m() {}
            }
        "#]],
    );
}

#[test]
fn reorder_modifiers_hoists_annotations_to_front() {
    check_reorder_mods(
        "class C{public @Override static void m(){}}",
        expect![[r#"
            class C {
                @Override public static void m() {}
            }
        "#]],
    );
}

#[test]
fn reorder_modifiers_keeps_relative_annotation_order() {
    // `@A` is a leading declaration annotation (before a keyword) and is hoisted to the front;
    // `@B` sits directly before the type, so it is a trailing type-use annotation and is kept in
    // place after the sorted keywords instead of being hoisted.
    check_reorder_mods(
        "class C{static @A public @B int x=0;}",
        expect![[r#"
            class C {
                @A public static @B int x = 0;
            }
        "#]],
    );
}

#[test]
fn reorder_modifiers_orders_sealed_and_non_sealed() {
    check_reorder_mods(
        "class C{final sealed class S{}final non-sealed class N{}}",
        expect![[r#"
            class C {
                sealed final class S {}
                non-sealed final class N {}
            }
        "#]],
    );
}

#[test]
fn reorder_modifiers_keeps_attached_comment_glued() {
    // A comment leading a modifier stays glued to it and moves with it when reordered: the
    // `// keep` comment anchored to `static` follows `static` past `public`.
    check_reorder_mods(
        "class C{\n// keep\nstatic public int x=0;}",
        expect![[r#"
            class C {
                public
                // keep
                static int x = 0;
            }
        "#]],
    );
}

#[test]
fn reorder_modifiers_off_by_default_preserves_order() {
    // With the option off (the default), the source modifier order is preserved exactly.
    check(
        "class C{final public static int x=0;}",
        expect![[r#"
            class C {
                final public static int x = 0;
            }
        "#]],
    );
}

#[test]
fn reorder_modifiers_is_idempotent() {
    let src =
        "final public class C{static @A private final int x=0;synchronized public void m(){}}";
    let once = fmt_reorder_mods(src);
    let twice = fmt_reorder_mods(&once);
    assert_eq!(once, twice, "reorder-modifiers must be idempotent");
}

#[test]
fn reorder_modifiers_skips_stray_error_recovery_modifiers() {
    // Regression: a `MODIFIERS` node produced by error recovery does not sit in a real
    // declaration — its parent is a `CLASS_BODY` / `SOURCE_FILE` / recovery node, never a member
    // or type declaration — so it is left in source order. Hoisting its annotation would change
    // the significant-token *sequence* such that re-parsing the output regroups tokens into a
    // different tree, which never reaches a fixed point (e.g. the `@` of `<public@` would be
    // absorbed into the preceding `<…>` as a type-parameter annotation). Source order is preserved
    // and stays idempotent.

    // `public @` directly under a class body keeps its order (no hoist to `@public`).
    let once = fmt_reorder_mods("class{public@=");
    assert_eq!(once, "class { public @=\n");
    assert_eq!(
        once,
        fmt_reorder_mods(&once),
        "stray modifiers must be idempotent"
    );

    // The original repro: the stray `MODIFIERS` sits under `SOURCE_FILE` next to a `<…>`.
    let once = fmt_reorder_mods("<public@");
    assert_eq!(
        once,
        fmt_reorder_mods(&once),
        "stray modifiers must be idempotent"
    );
}

// --- annotation-placement -------------------------------------------------------------------

fn fmt_annotation_placement(src: &str, placement: AnnotationPlacement) -> String {
    let cfg = Config {
        annotation_placement: placement,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_expanded(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_annotation_placement(
        src,
        AnnotationPlacement::Expanded,
    ));
}

#[test]
fn annotation_placement_compact_keeps_annotations_inline() {
    // `compact` (the default) reproduces the prior behavior: annotations are pulled inline onto
    // the declaration's line, collapsing the source's line break.
    expect![[r#"
        @Foo @Bar class D {
            @Inject private int x;
        }
    "#]]
    .assert_eq(&fmt_annotation_placement(
        "@Foo\n@Bar class D{@Inject private int x;}",
        AnnotationPlacement::Compact,
    ));
}

#[test]
fn annotation_placement_expanded_breaks_type_annotations() {
    check_expanded(
        "@Foo @Bar class C{}",
        expect![[r#"
            @Foo
            @Bar
            class C {}
        "#]],
    );
}

#[test]
fn annotation_placement_expanded_breaks_method_annotation() {
    check_expanded(
        "class C{@Override public void m(){}}",
        expect![[r#"
            class C {
                @Override
                public void m() {}
            }
        "#]],
    );
}

#[test]
fn annotation_placement_expanded_breaks_field_annotation() {
    check_expanded(
        "class C{@Inject private int x;}",
        expect![[r#"
            class C {
                @Inject
                private int x;
            }
        "#]],
    );
}

#[test]
fn annotation_placement_expanded_breaks_lone_marker() {
    // A lone annotation with no keyword modifier before the type is syntactically a declaration
    // annotation and breaks onto its own line. (google-java-format keeps it inline only when its
    // `@Target` is `TYPE_USE`, e.g. `@Nullable Foo m()`, which a syntactic formatter cannot
    // resolve — so only annotations *after* a keyword are recognized as type-use here.)
    check_expanded(
        "class C{@Override void m(){}}",
        expect![[r#"
            class C {
                @Override
                void m() {}
            }
        "#]],
    );
}

#[test]
fn annotation_placement_expanded_keeps_annotation_arguments() {
    // Each leading declaration annotation (here before the `public` keyword) breaks; its argument
    // list is untouched.
    check_expanded(
        "class C{@Foo(\"x\") @Bar(a=1) public void m(){}}",
        expect![[r#"
            class C {
                @Foo("x")
                @Bar(a = 1)
                public void m() {}
            }
        "#]],
    );
}

#[test]
fn annotation_placement_expanded_keeps_parameter_annotation_inline() {
    // A parameter's annotation is never broken out — it stays inline with the parameter, even when
    // the method's own leading declaration annotation (`@Override`, before `public`) breaks.
    check_expanded(
        "class C{@Override public void m(@NonNull String s){}}",
        expect![[r#"
            class C {
                @Override
                public void m(@NonNull String s) {}
            }
        "#]],
    );
}

#[test]
fn annotation_placement_expanded_keeps_type_use_annotation_inline() {
    // A type-use annotation lives in the type, not in the leading MODIFIERS, so it is unaffected.
    check_expanded(
        "class C{Outer. @A Inner f(){return null;}}",
        expect![[r#"
            class C {
                Outer.@A Inner f() {
                    return null;
                }
            }
        "#]],
    );
}

#[test]
fn annotation_placement_expanded_keeps_interleaved_annotation_inline() {
    // An annotation after a keyword (not in the leading run) stays inline — only the leading
    // contiguous run breaks. Here the run is empty, so nothing breaks.
    check_expanded(
        "class C{public @A static int x;}",
        expect![[r#"
            class C {
                public @A static int x;
            }
        "#]],
    );
}

#[test]
fn annotation_placement_expanded_breaks_local_variable_annotation() {
    // A local-variable declaration is a declaration-level target, so its annotation breaks too.
    check_expanded(
        "class C{void q(){@SuppressWarnings(\"x\") final var y=f();}}",
        expect![[r#"
            class C {
                void q() {
                    @SuppressWarnings("x")
                    final var y = f();
                }
            }
        "#]],
    );
}

#[test]
fn annotation_placement_expanded_composes_with_reorder_modifiers() {
    // The leading declaration annotation `@Foo` is hoisted to the front (reorder-modifiers) and
    // broken onto its own line (annotation-placement = expanded); the trailing type-use annotation
    // `@Bar`, sitting directly before the type, stays in place and inline.
    let cfg = Config {
        reorder_modifiers: true,
        annotation_placement: AnnotationPlacement::Expanded,
        ..Config::default()
    };
    expect![[r#"
        class C {
            @Foo
            public static @Bar int x;
        }
    "#]]
    .assert_eq(&fmt_with("class C{static @Foo public @Bar int x;}", &cfg));
}

#[test]
fn annotation_placement_expanded_keeps_single_line_body() {
    // The header break does not force a `fn-single-line` body to break. `@Override` breaks here
    // because the `public` keyword separates it from the type (a leading declaration annotation).
    let cfg = Config {
        fn_single_line: true,
        annotation_placement: AnnotationPlacement::Expanded,
        ..Config::default()
    };
    expect![[r#"
        class C {
            @Override
            public int m() { return 1; }
        }
    "#]]
    .assert_eq(&fmt_with(
        "class C{@Override public int m(){return 1;}}",
        &cfg,
    ));
}

#[test]
fn annotation_placement_modes_are_idempotent() {
    let src = "@A @B class C{@Override int x=0;@Foo(\"y\") void m(@NonNull String s){}static @Bar int z;@Deprecated public @Nullable Object f(){return null;}@Deprecated @Nullable C(){}}";
    for placement in [AnnotationPlacement::Compact, AnnotationPlacement::Expanded] {
        let once = fmt_annotation_placement(src, placement);
        let twice = fmt_annotation_placement(&once, placement);
        assert_eq!(
            once, twice,
            "annotation-placement {placement:?} must be idempotent"
        );
    }
}

// ----- type-use vs declaration annotation distinction (google-java-format parity) --------

/// Format with `reorder-modifiers` + `annotation-placement = expanded` (the google-java-format
/// preset), to exercise how the two compose around type-use annotations.
fn fmt_reorder_expanded(src: &str) -> String {
    let cfg = Config {
        reorder_modifiers: true,
        annotation_placement: AnnotationPlacement::Expanded,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn type_use_annotation_after_keyword_stays_inline() {
    // `@Nullable` sits after `public`, directly before the return type, so it annotates the type
    // and stays inline (google-java-format keeps `public @Nullable Object foo()`).
    check_expanded(
        "class C{public @Nullable Object foo(){}}",
        expect![[r#"
            class C {
                public @Nullable Object foo() {}
            }
        "#]],
    );
}

#[test]
fn declaration_annotation_breaks_while_type_use_stays_inline() {
    // `@Deprecated` precedes the `public` keyword (declaration annotation → own line); `@Nullable`
    // follows it, directly before the type (type-use → inline).
    check_expanded(
        "class C{@Deprecated public @Nullable Object foo(){}}",
        expect![[r#"
            class C {
                @Deprecated
                public @Nullable Object foo() {}
            }
        "#]],
    );
}

#[test]
fn constructor_annotations_break_having_no_type() {
    // A constructor has no type, so its annotations are all declaration annotations and each
    // breaks onto its own line (no trailing type-use run).
    check_expanded(
        "class C{@Deprecated @Nullable C(){}}",
        expect![[r#"
            class C {
                @Deprecated
                @Nullable
                C() {}
            }
        "#]],
    );
}

#[test]
fn reorder_and_expanded_keep_type_use_annotation_in_place() {
    // The google-java-format preset (reorder + expanded): `@Deprecated` breaks, the keyword stays
    // canonical, and the type-use `@Nullable` is neither hoisted nor broken.
    expect![[r#"
        class C {
            @Deprecated
            public @Nullable Object foo() {}
        }
    "#]]
    .assert_eq(&fmt_reorder_expanded(
        "class C{@Deprecated public @Nullable Object foo(){}}",
    ));
}

#[test]
fn reorder_keeps_type_use_annotation_inline_compact() {
    // With `reorder-modifiers` alone (compact), a type-use annotation directly before the type is
    // kept in place rather than hoisted ahead of the keyword (cf. google-java-format's B20577626).
    check_reorder_mods(
        "class C{private @Mock GsaConfigFlags mGsaConfig;}",
        expect![[r#"
            class C {
                private @Mock GsaConfigFlags mGsaConfig;
            }
        "#]],
    );
}

// ----- hex-literal-case -------------------------------------------------------------------

fn fmt_hex(src: &str, hex_literal_case: HexLiteralCase) -> String {
    let config = Config {
        hex_literal_case,
        ..Config::default()
    };
    fmt_with(src, &config)
}

/// A source exercising hex integers (plain, with `_` separators, with an `l`/`L` suffix), hex
/// floats (with a `p` exponent and an `f`/`d` suffix), and non-hex literals (decimal, octal,
/// binary, decimal float) that must stay byte-for-byte unchanged.
const HEX_SRC: &str = "class C{int a=0xCafe;int b=0XdeadL;long c=0xDEAD_beefl;double d=0xA.bP1F;float e=0Xf.0p-2d;int f=255;int g=0777;int h=0b1010;double i=3.14F;}";

#[test]
fn hex_literal_case_preserve_keeps_source_case() {
    // The default leaves every literal exactly as written.
    expect![[r#"
        class C {
            int a = 0xCafe;
            int b = 0XdeadL;
            long c = 0xDEAD_beefl;
            double d = 0xA.bP1F;
            float e = 0Xf.0p-2d;
            int f = 255;
            int g = 0777;
            int h = 0b1010;
            double i = 3.14F;
        }
    "#]]
    .assert_eq(&fmt_hex(HEX_SRC, HexLiteralCase::Preserve));
}

#[test]
fn hex_literal_case_upper_uppercases_only_hex_digits() {
    // Hex mantissa digits become upper case; the `0x`/`0X` prefix, the `p` exponent, and the
    // `l`/`f`/`d` suffix keep their case. Non-hex literals are untouched.
    expect![[r#"
        class C {
            int a = 0xCAFE;
            int b = 0XDEADL;
            long c = 0xDEAD_BEEFl;
            double d = 0xA.BP1F;
            float e = 0XF.0p-2d;
            int f = 255;
            int g = 0777;
            int h = 0b1010;
            double i = 3.14F;
        }
    "#]]
    .assert_eq(&fmt_hex(HEX_SRC, HexLiteralCase::Upper));
}

#[test]
fn hex_literal_case_lower_lowercases_only_hex_digits() {
    // Mirror image of the upper-case test: only the hex mantissa digits change.
    expect![[r#"
        class C {
            int a = 0xcafe;
            int b = 0XdeadL;
            long c = 0xdead_beefl;
            double d = 0xa.bP1F;
            float e = 0Xf.0p-2d;
            int f = 255;
            int g = 0777;
            int h = 0b1010;
            double i = 3.14F;
        }
    "#]]
    .assert_eq(&fmt_hex(HEX_SRC, HexLiteralCase::Lower));
}

#[test]
fn hex_literal_case_is_idempotent() {
    for case in [
        HexLiteralCase::Preserve,
        HexLiteralCase::Upper,
        HexLiteralCase::Lower,
    ] {
        let once = fmt_hex(HEX_SRC, case);
        let twice = fmt_hex(&once, case);
        assert_eq!(
            once, twice,
            "hex-literal-case must be idempotent ({case:?})"
        );
    }
}

// ----- float-literal-trailing-zero --------------------------------------------------------

fn fmt_float(src: &str, float_literal_trailing_zero: FloatLiteralTrailingZero) -> String {
    let config = Config {
        float_literal_trailing_zero,
        ..Config::default()
    };
    fmt_with(src, &config)
}

/// A source exercising the in-scope boundary (`1.0` / `1.` / `1.00`, with and without an `f`
/// suffix or an `e` exponent) and the out-of-scope literals that must stay byte-for-byte
/// unchanged: a non-zero fraction (`1.5`), a leading-dot float (`.5`), a dotless float (`1e10`),
/// a hex float (`0x1.0p3`), and an integer (`123`).
const FLOAT_SRC: &str = "class C{double a=1.0;double b=1.;double c=1.00;double d=1.5;double e=.5;double f=0.0;float g=1.0f;float h=1.f;double i=1.0e10;double j=1e10;double k=0x1.0p3;int l=123;}";

#[test]
fn float_literal_trailing_zero_preserve_keeps_source() {
    // The default leaves every literal exactly as written.
    expect![[r#"
        class C {
            double a = 1.0;
            double b = 1.;
            double c = 1.00;
            double d = 1.5;
            double e = .5;
            double f = 0.0;
            float g = 1.0f;
            float h = 1.f;
            double i = 1.0e10;
            double j = 1e10;
            double k = 0x1.0p3;
            int l = 123;
        }
    "#]]
    .assert_eq(&fmt_float(FLOAT_SRC, FloatLiteralTrailingZero::Preserve));
}

#[test]
fn float_literal_trailing_zero_always_adds_the_zero() {
    // Every empty-fraction decimal float gains a single trailing zero (`1.` → `1.0`,
    // `1.f` → `1.0f`); fractions that already have a digit, dotless / leading-dot / hex floats,
    // and integers are untouched.
    expect![[r#"
        class C {
            double a = 1.0;
            double b = 1.0;
            double c = 1.00;
            double d = 1.5;
            double e = .5;
            double f = 0.0;
            float g = 1.0f;
            float h = 1.0f;
            double i = 1.0e10;
            double j = 1e10;
            double k = 0x1.0p3;
            int l = 123;
        }
    "#]]
    .assert_eq(&fmt_float(FLOAT_SRC, FloatLiteralTrailingZero::Always));
}

#[test]
fn float_literal_trailing_zero_never_strips_the_zero() {
    // Every all-zero fraction is stripped to a bare dot (`1.0` / `1.00` → `1.`, `1.0f` → `1.f`,
    // `1.0e10` → `1.e10`); non-zero fractions, the leading-dot `.5`, dotless / hex floats, and
    // integers are untouched.
    expect![[r#"
        class C {
            double a = 1.;
            double b = 1.;
            double c = 1.;
            double d = 1.5;
            double e = .5;
            double f = 0.;
            float g = 1.f;
            float h = 1.f;
            double i = 1.e10;
            double j = 1e10;
            double k = 0x1.0p3;
            int l = 123;
        }
    "#]]
    .assert_eq(&fmt_float(FLOAT_SRC, FloatLiteralTrailingZero::Never));
}

#[test]
fn float_literal_trailing_zero_is_idempotent() {
    for mode in [
        FloatLiteralTrailingZero::Preserve,
        FloatLiteralTrailingZero::Always,
        FloatLiteralTrailingZero::Never,
    ] {
        let once = fmt_float(FLOAT_SRC, mode);
        let twice = fmt_float(&once, mode);
        assert_eq!(
            once, twice,
            "float-literal-trailing-zero must be idempotent ({mode:?})"
        );
    }
}

// ----- literal-suffix-case ----------------------------------------------------------------

fn fmt_suffix(src: &str, literal_suffix_case: LiteralSuffixCase) -> String {
    let config = Config {
        literal_suffix_case,
        ..Config::default()
    };
    fmt_with(src, &config)
}

/// A source exercising the in-scope suffixes — the integer `l`/`L` (decimal and hex) and the
/// floating-point `f`/`F`/`d`/`D` (decimal and hex) — alongside the out-of-scope literals that
/// must stay byte-for-byte unchanged: an unsuffixed integer (`255`), a hex integer whose trailing
/// `f` is a *digit* not a suffix (`0xabcdef`), and an unsuffixed float (`1.5`).
const SUFFIX_SRC: &str = "class C{long a=123l;long b=123L;long c=0xCAFEl;float d=1.5f;float e=2.5F;double f=3.5d;double g=4.5D;float h=0x1p3f;int i=255;int j=0xabcdef;double k=1.5;}";

#[test]
fn literal_suffix_case_preserve_keeps_source() {
    // The default leaves every literal exactly as written.
    expect![[r#"
        class C {
            long a = 123l;
            long b = 123L;
            long c = 0xCAFEl;
            float d = 1.5f;
            float e = 2.5F;
            double f = 3.5d;
            double g = 4.5D;
            float h = 0x1p3f;
            int i = 255;
            int j = 0xabcdef;
            double k = 1.5;
        }
    "#]]
    .assert_eq(&fmt_suffix(SUFFIX_SRC, LiteralSuffixCase::Preserve));
}

#[test]
fn literal_suffix_case_upper_uppercases_only_the_suffix() {
    // Every trailing type-suffix letter becomes upper case (`123l` → `123L`, `1.5f` → `1.5F`).
    // The hex digits keep their case (`hex-literal-case` is off), the unsuffixed literals are
    // untouched, and a hex integer's trailing `f` digit (`0xabcdef`) is *not* a suffix.
    expect![[r#"
        class C {
            long a = 123L;
            long b = 123L;
            long c = 0xCAFEL;
            float d = 1.5F;
            float e = 2.5F;
            double f = 3.5D;
            double g = 4.5D;
            float h = 0x1p3F;
            int i = 255;
            int j = 0xabcdef;
            double k = 1.5;
        }
    "#]]
    .assert_eq(&fmt_suffix(SUFFIX_SRC, LiteralSuffixCase::Upper));
}

#[test]
fn literal_suffix_case_lower_lowercases_only_the_suffix() {
    // Mirror image of the upper-case test: only the trailing suffix letter changes, and the
    // `0xabcdef` digit `f` stays put.
    expect![[r#"
        class C {
            long a = 123l;
            long b = 123l;
            long c = 0xCAFEl;
            float d = 1.5f;
            float e = 2.5f;
            double f = 3.5d;
            double g = 4.5d;
            float h = 0x1p3f;
            int i = 255;
            int j = 0xabcdef;
            double k = 1.5;
        }
    "#]]
    .assert_eq(&fmt_suffix(SUFFIX_SRC, LiteralSuffixCase::Lower));
}

#[test]
fn literal_suffix_case_is_idempotent() {
    for case in [
        LiteralSuffixCase::Preserve,
        LiteralSuffixCase::Upper,
        LiteralSuffixCase::Lower,
    ] {
        let once = fmt_suffix(SUFFIX_SRC, case);
        let twice = fmt_suffix(&once, case);
        assert_eq!(
            once, twice,
            "literal-suffix-case must be idempotent ({case:?})"
        );
    }
}

// ---------------------------------------------------------------------------
// continuation-indent
//
// A `continuation-indent` of `n` indents the wrapped (continuation) lines of an expression /
// statement by `n` columns, independently of the block-body indent (`indent-width`). The
// default (`None`) falls back to `indent-width`, leaving output unchanged.
// ---------------------------------------------------------------------------

fn fmt_cont(src: &str, n: usize) -> String {
    let cfg = Config {
        continuation_indent: Some(n),
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn fmt_cont_narrow(src: &str, n: usize, max_width: usize) -> String {
    let cfg = Config {
        continuation_indent: Some(n),
        max_width,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

#[test]
fn continuation_indent_wraps_binary_at_configured_width() {
    // The statement sits at column 8 (two block levels); wrapped operands hang 8 columns past it
    // (column 16) instead of the 4-column block indent.
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
    "#]].assert_eq(&fmt_cont(
        "class A{void m(){result=alphaOperandName+betaOperandName+gammaOperandName+deltaOperandName+epsilonOperandName;}}",
        8,
    ));
}

#[test]
fn continuation_indent_wraps_method_chain() {
    // Each wrapped `.call()` hangs `continuation-indent` (8) past the receiver line.
    expect![[r#"
        class A {
            void m() {
                source.stream()
                        .filter(predicate)
                        .map(mapper)
                        .collect(collector);
            }
        }
    "#]]
    .assert_eq(&fmt_cont_narrow(
        "class A{void m(){source.stream().filter(predicate).map(mapper).collect(collector);}}",
        8,
        40,
    ));
}

#[test]
fn continuation_indent_wraps_call_arguments() {
    // Wrapped arguments hang `continuation-indent` (8) past the call line.
    expect![[r#"
        class A {
            void m() {
                compute(
                        alphaArg,
                        betaArg,
                        gammaArg,
                        deltaArg,
                        epsilonArg
                );
            }
        }
    "#]]
    .assert_eq(&fmt_cont_narrow(
        "class A{void m(){compute(alphaArg,betaArg,gammaArg,deltaArg,epsilonArg);}}",
        8,
        40,
    ));
}

#[test]
fn continuation_indent_wraps_parameters_but_not_body() {
    // The wrapped parameters hang 8 past the method header (column 4 + 8 = 12), while the body
    // statement keeps the 4-column block indent (column 8). This is the block-vs-continuation
    // split.
    expect![[r#"
        class A {
            void method(
                    int alpha,
                    String beta,
                    long gamma,
                    double delta
            ) {
                int x = 1;
            }
        }
    "#]]
    .assert_eq(&fmt_cont_narrow(
        "class A{void method(int alpha,String beta,long gamma,double delta){int x=1;}}",
        8,
        40,
    ));
}

#[test]
fn continuation_indent_wraps_ternary() {
    // The `?` / `:` continuation lines hang `continuation-indent` (8) past the condition.
    expect![[r#"
        class A {
            void m() {
                int v = conditionExpr
                        ? thenValueExpression
                        : elseValueExpression;
            }
        }
    "#]]
    .assert_eq(&fmt_cont_narrow(
        "class A{void m(){int v=conditionExpr?thenValueExpression:elseValueExpression;}}",
        8,
        30,
    ));
}

#[test]
fn continuation_indent_composes_block_and_continuation() {
    // Three distinct indents in one snapshot: wrapped params hang 8 past the header (col 12),
    // body statements use the 4-column block indent (col 8), and the body's wrapped binary hangs
    // a further 8 (col 16).
    expect![[r#"
        class A {
            int method(
                    int alphaParam,
                    int betaParam,
                    int gammaParam
            ) {
                return alphaParam
                        + betaParam
                        + gammaParam
                        + alphaParam
                        + betaParam;
            }
        }
    "#]].assert_eq(&fmt_cont_narrow(
        "class A{int method(int alphaParam,int betaParam,int gammaParam){return alphaParam+betaParam+gammaParam+alphaParam+betaParam;}}",
        8,
        40,
    ));
}

#[test]
fn continuation_indent_default_matches_indent_width() {
    // `continuation-indent = Some(4)` is byte-identical to the default (`None`), which falls back
    // to `indent-width = 4`. Guards the fallback path.
    let src = "class A{void m(){result=alphaOperandName+betaOperandName+gammaOperandName+deltaOperandName+epsilonOperandName;}}";
    assert_eq!(fmt_cont(src, 4), fmt(src));
}

#[test]
fn continuation_indent_is_idempotent() {
    let src = "class A{int method(int alphaParam,int betaParam,int gammaParam){return alphaParam+betaParam+gammaParam+alphaParam+betaParam;}}";
    let once = fmt_cont_narrow(src, 8, 40);
    let twice = fmt_cont_narrow(&once, 8, 40);
    assert_eq!(once, twice, "continuation-indent must be idempotent");
}

#[test]
fn continuation_indent_ignored_in_tab_style() {
    // In tab style every indent step is one tab, so `continuation-indent` is ignored and the
    // output stays a whole number of tabs (no stray spaces).
    let cfg = Config {
        indent_style: IndentStyle::Tab,
        continuation_indent: Some(8),
        max_width: 40,
        ..Config::default()
    };
    let out = format_source(
        "class A{void m(){result=alphaOperandName+betaOperandName+gammaName;}}",
        &cfg,
    )
    .formatted;
    assert!(
        !out.lines()
            .any(|l| l.starts_with('\t') && l.trim_start_matches('\t').starts_with(' ')),
        "tab-style indentation must not mix tabs then spaces:\n{out}"
    );
    expect![[r#"
        class A {
        	void m() {
        		result = alphaOperandName
        			+ betaOperandName
        			+ gammaName;
        	}
        }
    "#]]
    .assert_eq(&out);
}

// ----- normalize-parameter-comments -------------------------------------------------------

#[test]
fn parameter_comments_are_normalized_and_hugged() {
    // Each `/*name=*/` becomes the canonical `/* name= */` (interior whitespace collapsed,
    // varargs `...` kept) and hugs the following argument on the same line.
    check_param_comments(
        "class C{void m(){f(/*a=*/1,/*xs...=*/2,/*  b  =  */3);}}",
        expect![[r#"
            class C {
                void m() {
                    f(/* a= */ 1, /* xs...= */ 2, /* b= */ 3);
                }
            }
        "#]],
    );
}

#[test]
fn parameter_comments_break_one_per_line_each_hugged() {
    // When the argument list wraps, each `/* name= */ value` group stays together on its line.
    check_param_comments(
        "class C{void m(){foo(/*alpha=*/111,/*beta=*/222,/*gamma=*/333,/*delta=*/444,/*epsilon=*/555);}}",
        expect![[r#"
            class C {
                void m() {
                    foo(
                        /* alpha= */ 111,
                        /* beta= */ 222,
                        /* gamma= */ 333,
                        /* delta= */ 444,
                        /* epsilon= */ 555
                    );
                }
            }
        "#]],
    );
}

#[test]
fn parameter_comments_off_by_default_keeps_them_verbatim() {
    // With the option off (the default), the comment text is untouched and it stays a trailing
    // line-suffix of the preceding token, exactly as before.
    check(
        "class C{void m(){f(/*a=*/1);}}",
        expect![[r#"
            class C {
                void m() {
                    f(1); /*a=*/
                }
            }
        "#]],
    );
}

#[test]
fn parameter_comments_leave_non_matching_comments_untouched() {
    // A non-matching block comment (`/* hello */`, `/* = */`, a leading-digit name `/*1=*/`) and
    // Javadoc (`/** doc */`) are neither rewritten nor hugged — they keep their text and their
    // ordinary trailing placement.
    check_param_comments(
        "class C{void m(){f(/* hello */1,/* = */2,/*1=*/3);g(/** doc */4);}}",
        expect![[r#"
            class C {
                void m() {
                    f(1, 2, 3); /* hello */ /* = */ /*1=*/
                    g(4); /** doc */
                }
            }
        "#]],
    );
}

#[test]
fn parameter_comments_normalization_is_idempotent() {
    let src = "class C{void m(){f(/*a=*/1,/*xs...=*/2);}}";
    let once = fmt_param_comments(src);
    let twice = fmt_param_comments(&once);
    assert_eq!(
        once, twice,
        "normalize-parameter-comments must be idempotent"
    );
}

/// Format with `inline-block-comments` enabled (otherwise default config).
fn fmt_inline_block(src: &str) -> String {
    let cfg = Config {
        inline_block_comments: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_inline_block(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_inline_block(src));
}

#[test]
fn inline_block_comments_keeps_embedded_marker_in_place() {
    check_inline_block(
        "class N{void f(){java.lang./* @MarkerAnnotation */ String s=null;}}",
        expect![[r#"
            class N {
                void f() {
                    java.lang./* @MarkerAnnotation */ String s = null;
                }
            }
        "#]],
    );
}

#[test]
fn inline_block_comments_hugs_consecutive_embedded_comments() {
    check_inline_block(
        "class N{void f(){java.lang./* a */ /* b */ String s=null;}}",
        expect![[r#"
            class N {
                void f() {
                    java.lang./* a */ /* b */ String s = null;
                }
            }
        "#]],
    );
}

#[test]
fn inline_block_comments_leaves_trailing_comment_relocated() {
    // The comments are followed by a NEWLINE before the next significant token, so they are
    // genuine trailing comments of `1` (not embedded) and stay flushed to end of line even with
    // the option on.
    check_inline_block(
        "class N{int x=1 /* x */ /* y */\n+2;}",
        expect![[r#"
            class N {
                int x = 1 + 2; /* x */ /* y */
            }
        "#]],
    );
}

#[test]
fn inline_block_comments_off_relocates_embedded_marker() {
    // Default config (option off): the embedded comment is relocated to end of line.
    check(
        "class N{void f(){java.lang./* @MarkerAnnotation */ String s=null;}}",
        expect![[r#"
            class N {
                void f() {
                    java.lang.String s = null; /* @MarkerAnnotation */
                }
            }
        "#]],
    );
}

#[test]
fn inline_block_comments_keeps_comment_before_closing_brace() {
    // A same-line block comment immediately before a body's closing brace has no following
    // *content* token to hug — the brace is emitted directly, never through `CommentMap::token`.
    // It must dangle on its own line inside the body rather than be dropped.
    check_inline_block(
        "class C{ int x; /* b */ }",
        expect![[r#"
            class C {
                int x;
                /* b */
            }
        "#]],
    );
}

#[test]
fn inline_block_comments_keeps_comment_in_empty_body() {
    // The lone comment of an otherwise empty body still dangles inside it instead of being
    // dropped when the empty body would otherwise collapse to `{}`.
    check_inline_block(
        "class C{/* b */}",
        expect![[r#"
            class C {
                /* b */
            }
        "#]],
    );
}

#[test]
fn inline_block_comments_is_idempotent() {
    let src = "class N{void f(){java.lang./* @A */ String s=null;int y=1 /* m */ * 2;}}";
    let once = fmt_inline_block(src);
    let twice = fmt_inline_block(&once);
    assert_eq!(once, twice, "inline-block-comments must be idempotent");
}

/// Format with `tabular-array-initializers` enabled, in a Google-like 2-space layout so the
/// preserved grid is easy to read.
fn fmt_tabular(src: &str) -> String {
    let cfg = Config {
        indent_width: 2,
        max_width: 100,
        array_width: 100,
        tabular_array_initializers: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_tabular(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_tabular(src));
}

#[test]
fn tabular_array_initializer_preserves_grid() {
    // A table-shaped initializer (rows of equal element counts) keeps its source row breaks
    // even though the elements would otherwise fit on one line — google-java-format's
    // `TabularMixedSignInitializer` behavior.
    check_tabular(
        "public class T {\n  private static final double[] f = {\n    95.0, 75.0, -95.0, 75.0,\n    -95.0, 75.0, +95.0, 75.0\n  };\n}\n",
        expect![[r#"
            public class T {
              private static final double[] f = {
                95.0, 75.0, -95.0, 75.0,
                -95.0, 75.0, +95.0, 75.0
              };
            }
        "#]],
    );
}

#[test]
fn tabular_array_initializer_with_short_final_row() {
    // The final row may hold fewer elements than the others.
    check_tabular(
        "class T {\n  int[] g = {\n    1, 2, 3,\n    4, 5, 6,\n    7, 8\n  };\n}\n",
        expect![[r#"
            class T {
              int[] g = {
                1, 2, 3,
                4, 5, 6,
                7, 8
              };
            }
        "#]],
    );
}

#[test]
fn tabular_array_initializer_off_by_default_collapses() {
    // With the option off (the default), a grid initializer that fits collapses to one line.
    check(
        "class T {\n  int[] g = {\n    1, 2,\n    3, 4\n  };\n}\n",
        expect![[r#"
            class T {
                int[] g = {1, 2, 3, 4};
            }
        "#]],
    );
}

#[test]
fn tabular_falls_back_for_irregular_rows() {
    // Rows of unequal length (the last is longer than the first) are not a table; the
    // initializer wraps by width and collapses when it fits.
    check_tabular(
        "class T {\n  int[] g = {\n    1, 2,\n    3, 4, 5\n  };\n}\n",
        expect![[r#"
            class T {
              int[] g = {1, 2, 3, 4, 5};
            }
        "#]],
    );
}

#[test]
fn tabular_falls_back_for_single_row() {
    // A single source row is not a table; it collapses by width.
    check_tabular(
        "class T {\n  int[] g = {\n    1, 2, 3\n  };\n}\n",
        expect![[r#"
            class T {
              int[] g = {1, 2, 3};
            }
        "#]],
    );
}

#[test]
fn tabular_nested_outer_grid_inner_collapses() {
    // The outer initializer is a one-column table (each inner array on its own row); the inner
    // single-line arrays are not tables and stay on their row.
    check_tabular(
        "class T {\n  int[][] m = {\n    {1, 2},\n    {3, 4}\n  };\n}\n",
        expect![[r#"
            class T {
              int[][] m = {
                {1, 2},
                {3, 4}
              };
            }
        "#]],
    );
}

#[test]
fn tabular_array_initializer_is_idempotent() {
    let src = "public class T {\n  private static final int[] g = {\n    1, 2, 3,\n    4, 5, 6\n  };\n}\n";
    let once = fmt_tabular(src);
    let twice = fmt_tabular(&once);
    assert_eq!(once, twice, "tabular-array-initializers must be idempotent");
}

/// Format with `switch-expression-on-new-line` enabled, in a Google-like 2-space / +4
/// continuation layout so the broken switch is easy to read.
fn fmt_switch_nl(src: &str) -> String {
    let cfg = Config {
        indent_width: 2,
        continuation_indent: Some(4),
        max_width: 100,
        switch_expression_on_new_line: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_switch_nl(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_switch_nl(src));
}

#[test]
fn switch_on_new_line_breaks_local_var_initializer() {
    // A local variable initialized with a switch expression breaks after `=`; the switch lands at
    // +4 (continuation) and its cases at +2 from the switch — google-java-format's layout.
    check_switch_nl(
        "class T { void f(String v) { int x = switch (v) { case \"a\" -> 1; default -> 0; }; } }",
        expect![[r#"
            class T {
              void f(String v) {
                int x =
                    switch (v) {
                      case "a" -> 1;
                      default -> 0;
                    };
              }
            }
        "#]],
    );
}

#[test]
fn switch_on_new_line_breaks_field_initializer() {
    // A field initializer behaves the same as a local one.
    check_switch_nl(
        "class T { int x = switch (v) { case 1 -> 1; default -> 0; }; }",
        expect![[r#"
            class T {
              int x =
                  switch (v) {
                    case 1 -> 1;
                    default -> 0;
                  };
            }
        "#]],
    );
}

#[test]
fn switch_on_new_line_breaks_assignment() {
    // A plain assignment (`x = switch …`) breaks after `=` too, not just declarations.
    check_switch_nl(
        "class T { void f() { x = switch (v) { case 1 -> 1; default -> 0; }; } }",
        expect![[r#"
            class T {
              void f() {
                x =
                    switch (v) {
                      case 1 -> 1;
                      default -> 0;
                    };
              }
            }
        "#]],
    );
}

#[test]
fn switch_on_new_line_keeps_return_inline() {
    // `return switch …` has no `=`, so it stays on the `return` line even with the option on.
    check_switch_nl(
        "class T { int f(String v) { return switch (v) { case \"a\" -> 1; default -> 0; }; } }",
        expect![[r#"
            class T {
              int f(String v) {
                return switch (v) {
                  case "a" -> 1;
                  default -> 0;
                };
              }
            }
        "#]],
    );
}

#[test]
fn switch_on_new_line_off_by_default_keeps_inline() {
    // With the option off (the default), the switch rides on the `=` line.
    check(
        "class T { int x = switch (v) { case 1 -> 1; default -> 0; }; }",
        expect![[r#"
            class T {
                int x = switch (v) {
                    case 1 -> 1;
                    default -> 0;
                };
            }
        "#]],
    );
}

#[test]
fn switch_on_new_line_is_idempotent() {
    let src =
        "class T { void f(String v) { int x = switch (v) { case \"a\" -> 1; default -> 0; }; } }";
    let once = fmt_switch_nl(src);
    let twice = fmt_switch_nl(&once);
    assert_eq!(
        once, twice,
        "switch-expression-on-new-line must be idempotent"
    );
}

/// Format with `wrap-case-labels` enabled, in a Google-like 2-space / +4 continuation layout so the
/// wrapped `case` list is easy to read.
fn fmt_wrap_case(src: &str) -> String {
    let cfg = Config {
        indent_width: 2,
        continuation_indent: Some(4),
        max_width: 100,
        wrap_case_labels: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_wrap_case(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_wrap_case(src));
}

const WRAP_CASE_SRC: &str = "class T { String m(MyEnum e) { return switch (e) { case SOME_RATHER_LONG_NAME_1, SOME_RATHER_LONG_NAME_2, SOME_RATHER_LONG_NAME_3, SOME_RATHER_LONG_NAME_4, SOME_RATHER_LONG_NAME_5, SOME_RATHER_LONG_NAME_6, SOME_RATHER_LONG_NAME_7 -> {} case SOME_RATHER_LONG_NAME_8 -> {} }; } }";

#[test]
fn wrap_case_labels_breaks_long_arrow_list() {
    // A `case` arm whose constant list overflows the column limit wraps: the first constant stays on
    // the `case` line, each later one hangs at +4 (continuation), the comma stays attached, and the
    // `-> {}` rides on the last constant's line — google-java-format's `ExpressionSwitch` layout. A
    // short arm (the second one) stays on one line.
    check_wrap_case(
        WRAP_CASE_SRC,
        expect![[r#"
            class T {
              String m(MyEnum e) {
                return switch (e) {
                  case SOME_RATHER_LONG_NAME_1,
                      SOME_RATHER_LONG_NAME_2,
                      SOME_RATHER_LONG_NAME_3,
                      SOME_RATHER_LONG_NAME_4,
                      SOME_RATHER_LONG_NAME_5,
                      SOME_RATHER_LONG_NAME_6,
                      SOME_RATHER_LONG_NAME_7 -> {}
                  case SOME_RATHER_LONG_NAME_8 -> {}
                };
              }
            }
        "#]],
    );
}

#[test]
fn wrap_case_labels_keeps_short_arrow_list_flat() {
    // A short constant list that fits stays on one line (all-or-nothing group).
    check_wrap_case(
        "class T { String m(MyEnum e) { return switch (e) { case CASE_A, CASE_B -> {} }; } }",
        expect![[r#"
            class T {
              String m(MyEnum e) {
                return switch (e) {
                  case CASE_A, CASE_B -> {}
                };
              }
            }
        "#]],
    );
}

#[test]
fn wrap_case_labels_breaks_long_colon_group() {
    // The legacy colon form wraps the same way; the `:` rides on the last constant's line and the
    // body breaks below (the default `switch-case-body = always`).
    check_wrap_case(
        "class T { void m(MyEnum e) { switch (e) { case SOME_RATHER_LONG_NAME_1, SOME_RATHER_LONG_NAME_2, SOME_RATHER_LONG_NAME_3, SOME_RATHER_LONG_NAME_4, SOME_RATHER_LONG_NAME_5, SOME_RATHER_LONG_NAME_6: doStuff(); break; } } }",
        expect![[r#"
            class T {
              void m(MyEnum e) {
                switch (e) {
                  case SOME_RATHER_LONG_NAME_1,
                      SOME_RATHER_LONG_NAME_2,
                      SOME_RATHER_LONG_NAME_3,
                      SOME_RATHER_LONG_NAME_4,
                      SOME_RATHER_LONG_NAME_5,
                      SOME_RATHER_LONG_NAME_6:
                    doStuff();
                    break;
                }
              }
            }
        "#]],
    );
}

#[test]
fn wrap_case_labels_keeps_guard_glued_to_constant() {
    // A `when` guard stays glued to its constant on the same wrapped line — it is part of the
    // constant chunk, not a separate break point.
    check_wrap_case(
        "class T { int m(Object o) { return switch (o) { case Integer i when LOOOOOOOOOOONG_CONDITION_AAAA, Long l when LOOOOOOOOOOONG_CONDITION_BBBB -> 1; default -> 0; }; } }",
        expect![[r#"
            class T {
              int m(Object o) {
                return switch (o) {
                  case Integer i when LOOOOOOOOOOONG_CONDITION_AAAA,
                      Long l when LOOOOOOOOOOONG_CONDITION_BBBB -> 1;
                  default -> 0;
                };
              }
            }
        "#]],
    );
}

#[test]
fn wrap_case_labels_leaves_default_and_single_constant_untouched() {
    // A bare `default` and a single-constant `case` never wrap, even when long.
    check_wrap_case(
        "class T { int m(int x) { return switch (x) { case SOME_SINGLE_BUT_RATHER_LONG_CONSTANT_NAME_THAT_IS_VERY_LONG_INDEED_X -> 1; default -> 0; }; } }",
        expect![[r#"
            class T {
              int m(int x) {
                return switch (x) {
                  case SOME_SINGLE_BUT_RATHER_LONG_CONSTANT_NAME_THAT_IS_VERY_LONG_INDEED_X -> 1;
                  default -> 0;
                };
              }
            }
        "#]],
    );
}

#[test]
fn wrap_case_labels_off_by_default_keeps_flat() {
    // With the option off, the same long list stays on one (overflowing) line — opt-in only.
    let cfg = Config {
        indent_width: 2,
        continuation_indent: Some(4),
        max_width: 100,
        wrap_case_labels: false,
        ..Config::default()
    };
    expect![[r#"
        class T {
          String m(MyEnum e) {
            return switch (e) {
              case SOME_RATHER_LONG_NAME_1, SOME_RATHER_LONG_NAME_2, SOME_RATHER_LONG_NAME_3, SOME_RATHER_LONG_NAME_4, SOME_RATHER_LONG_NAME_5, SOME_RATHER_LONG_NAME_6, SOME_RATHER_LONG_NAME_7 -> {}
              case SOME_RATHER_LONG_NAME_8 -> {}
            };
          }
        }
    "#]]
        .assert_eq(&fmt_with(WRAP_CASE_SRC, &cfg));
}

#[test]
fn wrap_case_labels_is_idempotent() {
    let once = fmt_wrap_case(WRAP_CASE_SRC);
    let twice = fmt_wrap_case(&once);
    assert_eq!(once, twice, "wrap-case-labels must be idempotent");
}

#[test]
fn switch_arrow_body_arrow_comment_hangs_at_continuation() {
    // A trailing `//` comment on `->` forces the body onto its own line; the body hangs at the
    // label + one continuation (`case` at +8, body at +12), like the legacy colon form indents.
    check_switch_nl(
        "class T { void main() {int x = switch (e) {case \"a\" -> //hello\n0; case \"b\"-> 1;}; }}",
        expect![[r#"
            class T {
              void main() {
                int x =
                    switch (e) {
                      case "a" -> //hello
                          0;
                      case "b" -> 1;
                    };
              }
            }
        "#]],
    );
}

#[test]
fn switch_arrow_body_leading_comment_hangs_at_continuation() {
    // A leading comment on the body's first token (its own line between `->` and the body) hangs
    // both the comment and the body at the label + one continuation, the same as the arrow form.
    check_switch_nl(
        "class T { void main() {int x = switch (e) {case \"a\" ->\n//c\n0; default -> 1;}; }}",
        expect![[r#"
            class T {
              void main() {
                int x =
                    switch (e) {
                      case "a" ->
                          //c
                          0;
                      default -> 1;
                    };
              }
            }
        "#]],
    );
}

#[test]
fn switch_arrow_body_arrow_comment_default_config() {
    // Default config: `continuation_cols` falls back to `indent-width` (4). The switch rides on the
    // `=` line, `case` lands at +12, and the comment-broken body at +16.
    check(
        "class T { void main() {int x = switch (e) {case \"a\" -> //hello\n0; case \"b\"-> 1;}; }}",
        expect![[r#"
            class T {
                void main() {
                    int x = switch (e) {
                        case "a" -> //hello
                            0;
                        case "b" -> 1;
                    };
                }
            }
        "#]],
    );
}

#[test]
fn switch_arrow_block_body_with_arrow_comment_keeps_block_at_label() {
    // A `{ … }` body is excluded from the continuation hang: even with a trailing comment on `->`,
    // its `{` stays at the label level and it aligns its own `}` with the label.
    check_switch_nl(
        "class T { void main() {switch (e) {case A -> //c\n{ f(); } default -> { g(); }}}}",
        expect![[r#"
            class T {
              void main() {
                switch (e) {
                  case A -> //c
                  {
                    f();
                  }
                  default -> {
                    g();
                  }
                }
              }
            }
        "#]],
    );
}

#[test]
fn switch_arrow_comment_free_body_unchanged() {
    // No comment after `->` ⇒ the continuation hang never fires; comment-free bodies are byte-for-
    // byte unchanged (the body rides on the arrow line).
    check_switch_nl(
        "class T { int x = switch (v) { case 1 -> 1; default -> 0; }; }",
        expect![[r#"
            class T {
              int x =
                  switch (v) {
                    case 1 -> 1;
                    default -> 0;
                  };
            }
        "#]],
    );
}

#[test]
fn switch_arrow_commented_body_is_idempotent() {
    let src = "class T { void main() {int x = switch (e) {case \"a\" -> //hello\n0; case \"b\" ->\n//c\n1;}; }}";
    let once = fmt_switch_nl(src);
    let twice = fmt_switch_nl(&once);
    assert_eq!(
        once, twice,
        "a comment-hung arrow switch body must be idempotent"
    );
}

// ----- switch-case-body -------------------------------------------------------------------

/// Format with an explicit `switch-case-body` mode at the default 4-space layout.
fn fmt_case_body(src: &str, mode: SwitchCaseBody) -> String {
    let cfg = Config {
        switch_case_body: mode,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

/// A legacy (colon-form) switch exercising a single-statement case, a multi-statement case, a
/// fall-through (`case 2: case 3:`), and a `default`.
const LEGACY_SWITCH_SRC: &str = "class C{void m(int x){switch(x){case 0:return 0;case 1:a();break;case 2:case 3:b();break;default:c();}}}";

#[test]
fn switch_case_body_always_breaks_and_indents() {
    // The default: every label on its own line, every body statement broken out and indented
    // one level (google-java-format's legacy-switch layout).
    expect![[r#"
        class C {
            void m(int x) {
                switch (x) {
                    case 0:
                        return 0;
                    case 1:
                        a();
                        break;
                    case 2:
                    case 3:
                        b();
                        break;
                    default:
                        c();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_case_body(LEGACY_SWITCH_SRC, SwitchCaseBody::Always));
}

#[test]
fn switch_case_body_single_line_keeps_single_statement_inline() {
    // A lone label with a single statement stays on the colon line; a multi-statement body and
    // a fall-through still break and indent.
    expect![[r#"
        class C {
            void m(int x) {
                switch (x) {
                    case 0: return 0;
                    case 1:
                        a();
                        break;
                    case 2:
                    case 3:
                        b();
                        break;
                    default: c();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_case_body(
        LEGACY_SWITCH_SRC,
        SwitchCaseBody::SingleLine,
    ));
}

#[test]
fn switch_case_body_same_line_keeps_all_inline() {
    // The prior behavior: the whole group stays inline on the label line.
    expect![[r#"
        class C {
            void m(int x) {
                switch (x) {
                    case 0: return 0;
                    case 1: a(); break;
                    case 2: case 3: b(); break;
                    default: c();
                }
            }
        }
    "#]]
    .assert_eq(&fmt_case_body(LEGACY_SWITCH_SRC, SwitchCaseBody::SameLine));
}

#[test]
fn switch_case_body_always_indents_label_and_body_comments() {
    // A comment before a label stays on the label's line; a comment before a body statement is
    // indented with the body (the motivating google-java-format `LegacySwitchComment` case).
    let cfg = Config {
        indent_width: 2,
        switch_case_body: SwitchCaseBody::Always,
        ..Config::default()
    };
    let src = "class T{int f(String v){switch(v){\n// about a\ncase \"a\":\nreturn 0;\ncase \"b\":\n// about b\nreturn 1;\n}}}";
    expect![[r#"
        class T {
          int f(String v) {
            switch (v) {
              // about a
              case "a":
                return 0;
              case "b":
                // about b
                return 1;
            }
          }
        }
    "#]]
    .assert_eq(&format_source(src, &cfg).formatted);
}

#[test]
fn switch_case_body_is_idempotent() {
    for mode in [
        SwitchCaseBody::Always,
        SwitchCaseBody::SingleLine,
        SwitchCaseBody::SameLine,
    ] {
        let once = fmt_case_body(LEGACY_SWITCH_SRC, mode);
        let twice = fmt_case_body(&once, mode);
        assert_eq!(
            once, twice,
            "switch-case-body must be idempotent ({mode:?})"
        );
    }
}

/// Format with `blank-line-at-block-start` enabled (otherwise default config).
fn fmt_blank_line_at_block_start(src: &str) -> String {
    let cfg = Config {
        blank_line_at_block_start: true,
        ..Config::default()
    };
    format_source(src, &cfg).formatted
}

fn check_blank_line_at_block_start(src: &str, expected: Expect) {
    expected.assert_eq(&fmt_blank_line_at_block_start(src));
}

#[test]
fn blank_line_at_block_start_keeps_class_body_leading_blank() {
    check_blank_line_at_block_start(
        "class Fields {\n\n  int a = 1;\n  int b = 1;\n}\n",
        expect![[r#"
            class Fields {

                int a = 1;
                int b = 1;
            }
        "#]],
    );
}

#[test]
fn blank_line_at_block_start_keeps_method_and_control_block_leading_blank() {
    // The leading blank is preserved in every braced body: a method block and a nested `if` block.
    check_blank_line_at_block_start(
        "class C {\n\n  void m() {\n\n    if (a) {\n\n      x();\n    }\n  }\n}\n",
        expect![[r#"
            class C {

                void m() {

                    if (a) {

                        x();
                    }
                }
            }
        "#]],
    );
}

#[test]
fn blank_line_at_block_start_keeps_blank_before_leading_comment() {
    // The blank line precedes a leading comment on the first item; it anchors on the comment (via
    // `break_before`'s leading-comment path) and is preserved with the comment.
    check_blank_line_at_block_start(
        "class C {\n\n  // hi\n  int x = 1;\n}\n",
        expect![[r#"
            class C {

                // hi
                int x = 1;
            }
        "#]],
    );
}

#[test]
fn blank_line_at_block_start_off_drops_leading_blank() {
    // Default config (option off): the leading blank after `{` is dropped, the prior behavior.
    check(
        "class Fields {\n\n  int a = 1;\n  int b = 1;\n}\n",
        expect![[r#"
            class Fields {
                int a = 1;
                int b = 1;
            }
        "#]],
    );
}

#[test]
fn blank_line_at_block_start_is_idempotent() {
    let src = "class C {\n\n  void m() {\n\n    x();\n  }\n}\n";
    let once = fmt_blank_line_at_block_start(src);
    let twice = fmt_blank_line_at_block_start(&once);
    assert_eq!(once, twice, "blank-line-at-block-start must be idempotent");
}
