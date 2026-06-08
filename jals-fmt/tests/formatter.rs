//! Snapshot tests for the formatter.

use expect_test::{Expect, expect};
use jals_fmt::{Config, format_source};

fn fmt(src: &str) -> String {
    format_source(src, &Config::default()).formatted
}

fn fmt_with(src: &str, config: &Config) -> String {
    format_source(src, config).formatted
}

fn check(src: &str, expected: Expect) {
    expected.assert_eq(&fmt(src));
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
