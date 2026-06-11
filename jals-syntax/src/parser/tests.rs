//! Parser snapshot tests: expect-test CST dumps plus lossless spot checks.

use super::*;
use expect_test::{Expect, expect};

/// Pretty-prints the parse result's syntax tree with indentation, showing kind and range (for tests).
/// Tokens also show the text. Trailing errors follow as `error: ...` lines.
fn debug_tree(parse: &Parse) -> String {
    use std::fmt::Write;
    let mut buf = String::new();
    let mut indent = 0;
    for event in parse.syntax().preorder_with_tokens() {
        use rowan::WalkEvent::{Enter, Leave};
        match event {
            Enter(elem) => {
                let kind = elem.kind();
                let range = elem.text_range();
                match elem {
                    rowan::NodeOrToken::Node(_) => {
                        let _ = writeln!(buf, "{:indent$}{kind:?}@{range:?}", "");
                    }
                    rowan::NodeOrToken::Token(t) => {
                        let _ = writeln!(buf, "{:indent$}{kind:?}@{range:?} {:?}", "", t.text());
                    }
                }
                indent += 2;
            }
            Leave(_) => indent -= 2,
        }
    }
    for err in parse.errors() {
        let _ = writeln!(buf, "error {:?}: {}", err.range(), err.message());
    }
    buf
}

/// Confirm that the tree dump matches the snapshot and is lossless.
fn check(src: &str, expected: Expect) {
    let parse = parse(src);
    expected.assert_eq(&debug_tree(&parse));
    assert_eq!(
        parse.syntax().text().to_string(),
        src,
        "lossless invariant violated"
    );
}

/// lossless: the syntax tree's text equals the input.
fn assert_lossless(src: &str) {
    let parse = parse(src);
    assert_eq!(parse.syntax().text().to_string(), src);
}

#[test]
fn package_and_imports() {
    check(
        "package a.b.c;\nimport java.util.List;\nimport static a.B.c;\nimport a.b.*;\n",
        expect![[r#"
            SOURCE_FILE@0..73
              PACKAGE_DECL@0..14
                PACKAGE_KW@0..7 "package"
                QUALIFIED_NAME@7..13
                  WHITESPACE@7..8 " "
                  IDENT@8..9 "a"
                  DOT@9..10 "."
                  IDENT@10..11 "b"
                  DOT@11..12 "."
                  IDENT@12..13 "c"
                SEMICOLON@13..14 ";"
              IMPORT_DECL@14..37
                NEWLINE@14..15 "\n"
                IMPORT_KW@15..21 "import"
                QUALIFIED_NAME@21..36
                  WHITESPACE@21..22 " "
                  IDENT@22..26 "java"
                  DOT@26..27 "."
                  IDENT@27..31 "util"
                  DOT@31..32 "."
                  IDENT@32..36 "List"
                SEMICOLON@36..37 ";"
              IMPORT_DECL@37..58
                NEWLINE@37..38 "\n"
                IMPORT_KW@38..44 "import"
                WHITESPACE@44..45 " "
                STATIC_KW@45..51 "static"
                QUALIFIED_NAME@51..57
                  WHITESPACE@51..52 " "
                  IDENT@52..53 "a"
                  DOT@53..54 "."
                  IDENT@54..55 "B"
                  DOT@55..56 "."
                  IDENT@56..57 "c"
                SEMICOLON@57..58 ";"
              IMPORT_DECL@58..72
                NEWLINE@58..59 "\n"
                IMPORT_KW@59..65 "import"
                QUALIFIED_NAME@65..71
                  WHITESPACE@65..66 " "
                  IDENT@66..67 "a"
                  DOT@67..68 "."
                  IDENT@68..69 "b"
                  DOT@69..70 "."
                  STAR@70..71 "*"
                SEMICOLON@71..72 ";"
              NEWLINE@72..73 "\n"
        "#]],
    );
}

#[test]
fn module_info_directives() {
    check(
        "open module a.b {\n  requires transitive java.base;\n  exports p.q to m.n;\n  uses p.S;\n  provides p.S with p.Impl;\n}\n",
        expect![[r#"
            SOURCE_FILE@0..115
              MODULE_DECL@0..114
                MODIFIERS@0..0
                OPEN_KW@0..4 "open"
                WHITESPACE@4..5 " "
                MODULE_KW@5..11 "module"
                QUALIFIED_NAME@11..15
                  WHITESPACE@11..12 " "
                  IDENT@12..13 "a"
                  DOT@13..14 "."
                  IDENT@14..15 "b"
                MODULE_BODY@15..114
                  WHITESPACE@15..16 " "
                  LBRACE@16..17 "{"
                  REQUIRES_DIRECTIVE@17..50
                    NEWLINE@17..18 "\n"
                    WHITESPACE@18..20 "  "
                    REQUIRES_KW@20..28 "requires"
                    WHITESPACE@28..29 " "
                    TRANSITIVE_KW@29..39 "transitive"
                    QUALIFIED_NAME@39..49
                      WHITESPACE@39..40 " "
                      IDENT@40..44 "java"
                      DOT@44..45 "."
                      IDENT@45..49 "base"
                    SEMICOLON@49..50 ";"
                  EXPORTS_DIRECTIVE@50..72
                    NEWLINE@50..51 "\n"
                    WHITESPACE@51..53 "  "
                    EXPORTS_KW@53..60 "exports"
                    QUALIFIED_NAME@60..64
                      WHITESPACE@60..61 " "
                      IDENT@61..62 "p"
                      DOT@62..63 "."
                      IDENT@63..64 "q"
                    WHITESPACE@64..65 " "
                    TO_KW@65..67 "to"
                    QUALIFIED_NAME@67..71
                      WHITESPACE@67..68 " "
                      IDENT@68..69 "m"
                      DOT@69..70 "."
                      IDENT@70..71 "n"
                    SEMICOLON@71..72 ";"
                  USES_DIRECTIVE@72..84
                    NEWLINE@72..73 "\n"
                    WHITESPACE@73..75 "  "
                    USES_KW@75..79 "uses"
                    QUALIFIED_NAME@79..83
                      WHITESPACE@79..80 " "
                      IDENT@80..81 "p"
                      DOT@81..82 "."
                      IDENT@82..83 "S"
                    SEMICOLON@83..84 ";"
                  PROVIDES_DIRECTIVE@84..112
                    NEWLINE@84..85 "\n"
                    WHITESPACE@85..87 "  "
                    PROVIDES_KW@87..95 "provides"
                    QUALIFIED_NAME@95..99
                      WHITESPACE@95..96 " "
                      IDENT@96..97 "p"
                      DOT@97..98 "."
                      IDENT@98..99 "S"
                    WHITESPACE@99..100 " "
                    WITH_KW@100..104 "with"
                    QUALIFIED_NAME@104..111
                      WHITESPACE@104..105 " "
                      IDENT@105..106 "p"
                      DOT@106..107 "."
                      IDENT@107..111 "Impl"
                    SEMICOLON@111..112 ";"
                  NEWLINE@112..113 "\n"
                  RBRACE@113..114 "}"
              NEWLINE@114..115 "\n"
        "#]],
    );
}

#[test]
fn module_lossless_edge_cases() {
    // `transitive` / `to` / `with` are restricted keywords: also valid as names.
    assert_lossless("@Deprecated module m {}\n");
    assert_lossless("module m { requires transitive; }\n"); // module *named* transitive
    assert_lossless("module m { requires static a.b; }\n");
    assert_lossless("module foo.bar { opens a.b to c.d, e.f; }\n");
    assert_lossless("module m { exports to.pkg to other.mod; }\n");
    // `module` / `requires` remain ordinary identifiers outside module context.
    assert_lossless("class C { int module = 1; void requires() {} }\n");
}

#[test]
fn class_with_field_and_method() {
    check(
        "public final class Foo<T> extends Bar implements I {\n  private int x = 1;\n  void m(int a) { return; }\n}\n",
        expect![[r#"
            SOURCE_FILE@0..104
              CLASS_DECL@0..103
                MODIFIERS@0..12
                  PUBLIC_KW@0..6 "public"
                  WHITESPACE@6..7 " "
                  FINAL_KW@7..12 "final"
                WHITESPACE@12..13 " "
                CLASS_KW@13..18 "class"
                WHITESPACE@18..19 " "
                IDENT@19..22 "Foo"
                TYPE_PARAMS@22..25
                  LT@22..23 "<"
                  TYPE_PARAM@23..24
                    IDENT@23..24 "T"
                  GT@24..25 ">"
                EXTENDS_CLAUSE@25..37
                  WHITESPACE@25..26 " "
                  EXTENDS_KW@26..33 "extends"
                  TYPE@33..37
                    WHITESPACE@33..34 " "
                    IDENT@34..37 "Bar"
                IMPLEMENTS_CLAUSE@37..50
                  WHITESPACE@37..38 " "
                  IMPLEMENTS_KW@38..48 "implements"
                  TYPE@48..50
                    WHITESPACE@48..49 " "
                    IDENT@49..50 "I"
                CLASS_BODY@50..103
                  WHITESPACE@50..51 " "
                  LBRACE@51..52 "{"
                  FIELD_DECL@52..73
                    MODIFIERS@52..62
                      NEWLINE@52..53 "\n"
                      WHITESPACE@53..55 "  "
                      PRIVATE_KW@55..62 "private"
                    TYPE@62..66
                      WHITESPACE@62..63 " "
                      INT_KW@63..66 "int"
                    WHITESPACE@66..67 " "
                    IDENT@67..68 "x"
                    WHITESPACE@68..69 " "
                    EQ@69..70 "="
                    LITERAL@70..72
                      WHITESPACE@70..71 " "
                      INT_LITERAL@71..72 "1"
                    SEMICOLON@72..73 ";"
                  METHOD_DECL@73..101
                    MODIFIERS@73..73
                    TYPE@73..80
                      NEWLINE@73..74 "\n"
                      WHITESPACE@74..76 "  "
                      VOID_KW@76..80 "void"
                    WHITESPACE@80..81 " "
                    IDENT@81..82 "m"
                    PARAM_LIST@82..89
                      LPAREN@82..83 "("
                      PARAM@83..88
                        MODIFIERS@83..83
                        TYPE@83..86
                          INT_KW@83..86 "int"
                        WHITESPACE@86..87 " "
                        IDENT@87..88 "a"
                      RPAREN@88..89 ")"
                    BLOCK@89..101
                      WHITESPACE@89..90 " "
                      LBRACE@90..91 "{"
                      RETURN_STMT@91..99
                        WHITESPACE@91..92 " "
                        RETURN_KW@92..98 "return"
                        SEMICOLON@98..99 ";"
                      WHITESPACE@99..100 " "
                      RBRACE@100..101 "}"
                  NEWLINE@101..102 "\n"
                  RBRACE@102..103 "}"
              NEWLINE@103..104 "\n"
        "#]],
    );
}

#[test]
fn generics_nested_close() {
    check(
        "class C { Map<K, List<V>> m; }",
        expect![[r#"
            SOURCE_FILE@0..30
              CLASS_DECL@0..30
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..30
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  FIELD_DECL@9..28
                    MODIFIERS@9..9
                    TYPE@9..25
                      WHITESPACE@9..10 " "
                      IDENT@10..13 "Map"
                      TYPE_ARGS@13..25
                        LT@13..14 "<"
                        TYPE@14..15
                          IDENT@14..15 "K"
                        COMMA@15..16 ","
                        TYPE@16..24
                          WHITESPACE@16..17 " "
                          IDENT@17..21 "List"
                          TYPE_ARGS@21..24
                            LT@21..22 "<"
                            TYPE@22..23
                              IDENT@22..23 "V"
                            GT@23..24 ">"
                        GT@24..25 ">"
                    WHITESPACE@25..26 " "
                    IDENT@26..27 "m"
                    SEMICOLON@27..28 ";"
                  WHITESPACE@28..29 " "
                  RBRACE@29..30 "}"
        "#]],
    );
}

#[test]
fn expr_precedence_and_shifts() {
    check(
        "class C { int f() { return a + b * c >> d && e; } }",
        expect![[r#"
            SOURCE_FILE@0..51
              CLASS_DECL@0..51
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..51
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..49
                    MODIFIERS@9..9
                    TYPE@9..13
                      WHITESPACE@9..10 " "
                      INT_KW@10..13 "int"
                    WHITESPACE@13..14 " "
                    IDENT@14..15 "f"
                    PARAM_LIST@15..17
                      LPAREN@15..16 "("
                      RPAREN@16..17 ")"
                    BLOCK@17..49
                      WHITESPACE@17..18 " "
                      LBRACE@18..19 "{"
                      RETURN_STMT@19..47
                        WHITESPACE@19..20 " "
                        RETURN_KW@20..26 "return"
                        BINARY_EXPR@26..46
                          BINARY_EXPR@26..41
                            BINARY_EXPR@26..36
                              NAME_REF@26..28
                                WHITESPACE@26..27 " "
                                IDENT@27..28 "a"
                              WHITESPACE@28..29 " "
                              PLUS@29..30 "+"
                              BINARY_EXPR@30..36
                                NAME_REF@30..32
                                  WHITESPACE@30..31 " "
                                  IDENT@31..32 "b"
                                WHITESPACE@32..33 " "
                                STAR@33..34 "*"
                                NAME_REF@34..36
                                  WHITESPACE@34..35 " "
                                  IDENT@35..36 "c"
                            WHITESPACE@36..37 " "
                            GT@37..38 ">"
                            GT@38..39 ">"
                            NAME_REF@39..41
                              WHITESPACE@39..40 " "
                              IDENT@40..41 "d"
                          WHITESPACE@41..42 " "
                          AMP_AMP@42..44 "&&"
                          NAME_REF@44..46
                            WHITESPACE@44..45 " "
                            IDENT@45..46 "e"
                        SEMICOLON@46..47 ";"
                      WHITESPACE@47..48 " "
                      RBRACE@48..49 "}"
                  WHITESPACE@49..50 " "
                  RBRACE@50..51 "}"
        "#]],
    );
}

#[test]
fn non_sealed_modifier() {
    check(
        "non-sealed class C { }",
        expect![[r#"
            SOURCE_FILE@0..22
              CLASS_DECL@0..22
                MODIFIERS@0..10
                  NON_SEALED_KW@0..10
                    IDENT@0..3 "non"
                    MINUS@3..4 "-"
                    IDENT@4..10 "sealed"
                WHITESPACE@10..11 " "
                CLASS_KW@11..16 "class"
                WHITESPACE@16..17 " "
                IDENT@17..18 "C"
                CLASS_BODY@18..22
                  WHITESPACE@18..19 " "
                  LBRACE@19..20 "{"
                  WHITESPACE@20..21 " "
                  RBRACE@21..22 "}"
        "#]],
    );
}

#[test]
fn error_recovery_in_member() {
    check(
        "class C { @ int ; void m() { } }",
        expect![[r#"
            SOURCE_FILE@0..32
              CLASS_DECL@0..32
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..32
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  FIELD_DECL@9..17
                    MODIFIERS@9..11
                      ANNOTATION@9..11
                        WHITESPACE@9..10 " "
                        AT@10..11 "@"
                        QUALIFIED_NAME@11..11
                    TYPE@11..15
                      WHITESPACE@11..12 " "
                      INT_KW@12..15 "int"
                    WHITESPACE@15..16 " "
                    SEMICOLON@16..17 ";"
                  METHOD_DECL@17..30
                    MODIFIERS@17..17
                    TYPE@17..22
                      WHITESPACE@17..18 " "
                      VOID_KW@18..22 "void"
                    WHITESPACE@22..23 " "
                    IDENT@23..24 "m"
                    PARAM_LIST@24..26
                      LPAREN@24..25 "("
                      RPAREN@25..26 ")"
                    BLOCK@26..30
                      WHITESPACE@26..27 " "
                      LBRACE@27..28 "{"
                      WHITESPACE@28..29 " "
                      RBRACE@29..30 "}"
                  WHITESPACE@30..31 " "
                  RBRACE@31..32 "}"
            error 11..11: expected IDENT
            error 15..15: expected IDENT
        "#]],
    );
}

#[test]
fn error_recovery_unclosed_block() {
    check(
        "class C { void m() { ",
        expect![[r#"
            SOURCE_FILE@0..21
              CLASS_DECL@0..20
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..20
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..20
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..20
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
              WHITESPACE@20..21 " "
            error 20..20: expected RBRACE
            error 20..20: expected RBRACE
        "#]],
    );
}

#[test]
fn instanceof_and_var_local() {
    check(
        "class C { void m() { var s = o; if (o instanceof String) return; } }",
        expect![[r#"
            SOURCE_FILE@0..68
              CLASS_DECL@0..68
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..68
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..66
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..66
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      LOCAL_VAR_DECL@20..31
                        MODIFIERS@20..20
                        TYPE@20..24
                          WHITESPACE@20..21 " "
                          VAR_KW@21..24 "var"
                        WHITESPACE@24..25 " "
                        IDENT@25..26 "s"
                        WHITESPACE@26..27 " "
                        EQ@27..28 "="
                        NAME_REF@28..30
                          WHITESPACE@28..29 " "
                          IDENT@29..30 "o"
                        SEMICOLON@30..31 ";"
                      IF_STMT@31..64
                        WHITESPACE@31..32 " "
                        IF_KW@32..34 "if"
                        WHITESPACE@34..35 " "
                        LPAREN@35..36 "("
                        BINARY_EXPR@36..55
                          NAME_REF@36..37
                            IDENT@36..37 "o"
                          WHITESPACE@37..38 " "
                          INSTANCEOF_KW@38..48 "instanceof"
                          TYPE@48..55
                            WHITESPACE@48..49 " "
                            IDENT@49..55 "String"
                        RPAREN@55..56 ")"
                        RETURN_STMT@56..64
                          WHITESPACE@56..57 " "
                          RETURN_KW@57..63 "return"
                          SEMICOLON@63..64 ";"
                      WHITESPACE@64..65 " "
                      RBRACE@65..66 "}"
                  WHITESPACE@66..67 " "
                  RBRACE@67..68 "}"
        "#]],
    );
}

#[test]
fn interface_with_default_method() {
    check(
        "interface I extends A, B { int C = 1; default void m() { } void n(); }",
        expect![[r#"
            SOURCE_FILE@0..70
              INTERFACE_DECL@0..70
                MODIFIERS@0..0
                INTERFACE_KW@0..9 "interface"
                WHITESPACE@9..10 " "
                IDENT@10..11 "I"
                EXTENDS_CLAUSE@11..24
                  WHITESPACE@11..12 " "
                  EXTENDS_KW@12..19 "extends"
                  TYPE@19..21
                    WHITESPACE@19..20 " "
                    IDENT@20..21 "A"
                  COMMA@21..22 ","
                  TYPE@22..24
                    WHITESPACE@22..23 " "
                    IDENT@23..24 "B"
                CLASS_BODY@24..70
                  WHITESPACE@24..25 " "
                  LBRACE@25..26 "{"
                  FIELD_DECL@26..37
                    MODIFIERS@26..26
                    TYPE@26..30
                      WHITESPACE@26..27 " "
                      INT_KW@27..30 "int"
                    WHITESPACE@30..31 " "
                    IDENT@31..32 "C"
                    WHITESPACE@32..33 " "
                    EQ@33..34 "="
                    LITERAL@34..36
                      WHITESPACE@34..35 " "
                      INT_LITERAL@35..36 "1"
                    SEMICOLON@36..37 ";"
                  METHOD_DECL@37..58
                    MODIFIERS@37..45
                      WHITESPACE@37..38 " "
                      DEFAULT_KW@38..45 "default"
                    TYPE@45..50
                      WHITESPACE@45..46 " "
                      VOID_KW@46..50 "void"
                    WHITESPACE@50..51 " "
                    IDENT@51..52 "m"
                    PARAM_LIST@52..54
                      LPAREN@52..53 "("
                      RPAREN@53..54 ")"
                    BLOCK@54..58
                      WHITESPACE@54..55 " "
                      LBRACE@55..56 "{"
                      WHITESPACE@56..57 " "
                      RBRACE@57..58 "}"
                  METHOD_DECL@58..68
                    MODIFIERS@58..58
                    TYPE@58..63
                      WHITESPACE@58..59 " "
                      VOID_KW@59..63 "void"
                    WHITESPACE@63..64 " "
                    IDENT@64..65 "n"
                    PARAM_LIST@65..67
                      LPAREN@65..66 "("
                      RPAREN@66..67 ")"
                    SEMICOLON@67..68 ";"
                  WHITESPACE@68..69 " "
                  RBRACE@69..70 "}"
        "#]],
    );
}

#[test]
fn enum_with_constants_and_body() {
    check(
        "enum E implements I { A, B(1), C { void m() {} }; final int x; E() {} }",
        expect![[r#"
            SOURCE_FILE@0..71
              ENUM_DECL@0..71
                MODIFIERS@0..0
                ENUM_KW@0..4 "enum"
                WHITESPACE@4..5 " "
                IDENT@5..6 "E"
                IMPLEMENTS_CLAUSE@6..19
                  WHITESPACE@6..7 " "
                  IMPLEMENTS_KW@7..17 "implements"
                  TYPE@17..19
                    WHITESPACE@17..18 " "
                    IDENT@18..19 "I"
                ENUM_BODY@19..71
                  WHITESPACE@19..20 " "
                  LBRACE@20..21 "{"
                  ENUM_CONSTANT@21..23
                    WHITESPACE@21..22 " "
                    IDENT@22..23 "A"
                  COMMA@23..24 ","
                  ENUM_CONSTANT@24..29
                    WHITESPACE@24..25 " "
                    IDENT@25..26 "B"
                    ARG_LIST@26..29
                      LPAREN@26..27 "("
                      LITERAL@27..28
                        INT_LITERAL@27..28 "1"
                      RPAREN@28..29 ")"
                  COMMA@29..30 ","
                  ENUM_CONSTANT@30..48
                    WHITESPACE@30..31 " "
                    IDENT@31..32 "C"
                    CLASS_BODY@32..48
                      WHITESPACE@32..33 " "
                      LBRACE@33..34 "{"
                      METHOD_DECL@34..46
                        MODIFIERS@34..34
                        TYPE@34..39
                          WHITESPACE@34..35 " "
                          VOID_KW@35..39 "void"
                        WHITESPACE@39..40 " "
                        IDENT@40..41 "m"
                        PARAM_LIST@41..43
                          LPAREN@41..42 "("
                          RPAREN@42..43 ")"
                        BLOCK@43..46
                          WHITESPACE@43..44 " "
                          LBRACE@44..45 "{"
                          RBRACE@45..46 "}"
                      WHITESPACE@46..47 " "
                      RBRACE@47..48 "}"
                  SEMICOLON@48..49 ";"
                  FIELD_DECL@49..62
                    MODIFIERS@49..55
                      WHITESPACE@49..50 " "
                      FINAL_KW@50..55 "final"
                    TYPE@55..59
                      WHITESPACE@55..56 " "
                      INT_KW@56..59 "int"
                    WHITESPACE@59..60 " "
                    IDENT@60..61 "x"
                    SEMICOLON@61..62 ";"
                  CONSTRUCTOR_DECL@62..69
                    MODIFIERS@62..62
                    WHITESPACE@62..63 " "
                    IDENT@63..64 "E"
                    PARAM_LIST@64..66
                      LPAREN@64..65 "("
                      RPAREN@65..66 ")"
                    BLOCK@66..69
                      WHITESPACE@66..67 " "
                      LBRACE@67..68 "{"
                      RBRACE@68..69 "}"
                  WHITESPACE@69..70 " "
                  RBRACE@70..71 "}"
        "#]],
    );
}

#[test]
fn record_with_components() {
    check(
        "record Point(int x, int y) implements Shape { Point { } int sum() { return x + y; } }",
        expect![[r#"
            SOURCE_FILE@0..85
              RECORD_DECL@0..85
                MODIFIERS@0..0
                RECORD_KW@0..6 "record"
                WHITESPACE@6..7 " "
                IDENT@7..12 "Point"
                RECORD_HEADER@12..26
                  LPAREN@12..13 "("
                  RECORD_COMPONENT@13..18
                    MODIFIERS@13..13
                    TYPE@13..16
                      INT_KW@13..16 "int"
                    WHITESPACE@16..17 " "
                    IDENT@17..18 "x"
                  COMMA@18..19 ","
                  RECORD_COMPONENT@19..25
                    MODIFIERS@19..19
                    TYPE@19..23
                      WHITESPACE@19..20 " "
                      INT_KW@20..23 "int"
                    WHITESPACE@23..24 " "
                    IDENT@24..25 "y"
                  RPAREN@25..26 ")"
                IMPLEMENTS_CLAUSE@26..43
                  WHITESPACE@26..27 " "
                  IMPLEMENTS_KW@27..37 "implements"
                  TYPE@37..43
                    WHITESPACE@37..38 " "
                    IDENT@38..43 "Shape"
                CLASS_BODY@43..85
                  WHITESPACE@43..44 " "
                  LBRACE@44..45 "{"
                  CONSTRUCTOR_DECL@45..55
                    MODIFIERS@45..45
                    WHITESPACE@45..46 " "
                    IDENT@46..51 "Point"
                    BLOCK@51..55
                      WHITESPACE@51..52 " "
                      LBRACE@52..53 "{"
                      WHITESPACE@53..54 " "
                      RBRACE@54..55 "}"
                  METHOD_DECL@55..83
                    MODIFIERS@55..55
                    TYPE@55..59
                      WHITESPACE@55..56 " "
                      INT_KW@56..59 "int"
                    WHITESPACE@59..60 " "
                    IDENT@60..63 "sum"
                    PARAM_LIST@63..65
                      LPAREN@63..64 "("
                      RPAREN@64..65 ")"
                    BLOCK@65..83
                      WHITESPACE@65..66 " "
                      LBRACE@66..67 "{"
                      RETURN_STMT@67..81
                        WHITESPACE@67..68 " "
                        RETURN_KW@68..74 "return"
                        BINARY_EXPR@74..80
                          NAME_REF@74..76
                            WHITESPACE@74..75 " "
                            IDENT@75..76 "x"
                          WHITESPACE@76..77 " "
                          PLUS@77..78 "+"
                          NAME_REF@78..80
                            WHITESPACE@78..79 " "
                            IDENT@79..80 "y"
                        SEMICOLON@80..81 ";"
                      WHITESPACE@81..82 " "
                      RBRACE@82..83 "}"
                  WHITESPACE@83..84 " "
                  RBRACE@84..85 "}"
        "#]],
    );
}

#[test]
fn annotation_type_decl() {
    check(
        "@interface Ann { String value() default \"x\"; int n(); }",
        expect![[r#"
            SOURCE_FILE@0..55
              ANNOTATION_TYPE_DECL@0..55
                MODIFIERS@0..0
                AT@0..1 "@"
                INTERFACE_KW@1..10 "interface"
                WHITESPACE@10..11 " "
                IDENT@11..14 "Ann"
                CLASS_BODY@14..55
                  WHITESPACE@14..15 " "
                  LBRACE@15..16 "{"
                  METHOD_DECL@16..44
                    MODIFIERS@16..16
                    TYPE@16..23
                      WHITESPACE@16..17 " "
                      IDENT@17..23 "String"
                    WHITESPACE@23..24 " "
                    IDENT@24..29 "value"
                    PARAM_LIST@29..31
                      LPAREN@29..30 "("
                      RPAREN@30..31 ")"
                    ANNOTATION_DEFAULT@31..43
                      WHITESPACE@31..32 " "
                      DEFAULT_KW@32..39 "default"
                      LITERAL@39..43
                        WHITESPACE@39..40 " "
                        STRING_LITERAL@40..43 "\"x\""
                    SEMICOLON@43..44 ";"
                  METHOD_DECL@44..53
                    MODIFIERS@44..44
                    TYPE@44..48
                      WHITESPACE@44..45 " "
                      INT_KW@45..48 "int"
                    WHITESPACE@48..49 " "
                    IDENT@49..50 "n"
                    PARAM_LIST@50..52
                      LPAREN@50..51 "("
                      RPAREN@51..52 ")"
                    SEMICOLON@52..53 ";"
                  WHITESPACE@53..54 " "
                  RBRACE@54..55 "}"
        "#]],
    );
}

#[test]
fn sealed_and_permits() {
    check(
        "public sealed interface Shape permits Circle, Square { }",
        expect![[r#"
            SOURCE_FILE@0..56
              INTERFACE_DECL@0..56
                MODIFIERS@0..13
                  PUBLIC_KW@0..6 "public"
                  WHITESPACE@6..7 " "
                  SEALED_KW@7..13 "sealed"
                WHITESPACE@13..14 " "
                INTERFACE_KW@14..23 "interface"
                WHITESPACE@23..24 " "
                IDENT@24..29 "Shape"
                PERMITS_CLAUSE@29..52
                  WHITESPACE@29..30 " "
                  PERMITS_KW@30..37 "permits"
                  TYPE@37..44
                    WHITESPACE@37..38 " "
                    IDENT@38..44 "Circle"
                  COMMA@44..45 ","
                  TYPE@45..52
                    WHITESPACE@45..46 " "
                    IDENT@46..52 "Square"
                CLASS_BODY@52..56
                  WHITESPACE@52..53 " "
                  LBRACE@53..54 "{"
                  WHITESPACE@54..55 " "
                  RBRACE@55..56 "}"
        "#]],
    );
}

#[test]
fn annotation_with_args() {
    check(
        "@Foo(name = \"a\", values = {1, 2}) @Bar(3) class C { }",
        expect![[r#"
            SOURCE_FILE@0..53
              CLASS_DECL@0..53
                MODIFIERS@0..41
                  ANNOTATION@0..33
                    AT@0..1 "@"
                    QUALIFIED_NAME@1..4
                      IDENT@1..4 "Foo"
                    ANNOTATION_ARG_LIST@4..33
                      LPAREN@4..5 "("
                      ANNOTATION_PAIR@5..15
                        IDENT@5..9 "name"
                        WHITESPACE@9..10 " "
                        EQ@10..11 "="
                        LITERAL@11..15
                          WHITESPACE@11..12 " "
                          STRING_LITERAL@12..15 "\"a\""
                      COMMA@15..16 ","
                      ANNOTATION_PAIR@16..32
                        WHITESPACE@16..17 " "
                        IDENT@17..23 "values"
                        WHITESPACE@23..24 " "
                        EQ@24..25 "="
                        ARRAY_INIT@25..32
                          WHITESPACE@25..26 " "
                          LBRACE@26..27 "{"
                          LITERAL@27..28
                            INT_LITERAL@27..28 "1"
                          COMMA@28..29 ","
                          LITERAL@29..31
                            WHITESPACE@29..30 " "
                            INT_LITERAL@30..31 "2"
                          RBRACE@31..32 "}"
                      RPAREN@32..33 ")"
                  ANNOTATION@33..41
                    WHITESPACE@33..34 " "
                    AT@34..35 "@"
                    QUALIFIED_NAME@35..38
                      IDENT@35..38 "Bar"
                    ANNOTATION_ARG_LIST@38..41
                      LPAREN@38..39 "("
                      LITERAL@39..40
                        INT_LITERAL@39..40 "3"
                      RPAREN@40..41 ")"
                WHITESPACE@41..42 " "
                CLASS_KW@42..47 "class"
                WHITESPACE@47..48 " "
                IDENT@48..49 "C"
                CLASS_BODY@49..53
                  WHITESPACE@49..50 " "
                  LBRACE@50..51 "{"
                  WHITESPACE@51..52 " "
                  RBRACE@52..53 "}"
        "#]],
    );
}

#[test]
fn for_each_and_classic_for() {
    check(
        "class C { void m() { for (var x : xs) f(x); for (int i = 0; i < n; i++) g(i); } }",
        expect![[r#"
            SOURCE_FILE@0..81
              CLASS_DECL@0..81
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..81
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..79
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..79
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      FOR_EACH_STMT@20..43
                        WHITESPACE@20..21 " "
                        FOR_KW@21..24 "for"
                        WHITESPACE@24..25 " "
                        LPAREN@25..26 "("
                        MODIFIERS@26..26
                        TYPE@26..29
                          VAR_KW@26..29 "var"
                        WHITESPACE@29..30 " "
                        IDENT@30..31 "x"
                        WHITESPACE@31..32 " "
                        COLON@32..33 ":"
                        NAME_REF@33..36
                          WHITESPACE@33..34 " "
                          IDENT@34..36 "xs"
                        RPAREN@36..37 ")"
                        EXPR_STMT@37..43
                          CALL_EXPR@37..42
                            NAME_REF@37..39
                              WHITESPACE@37..38 " "
                              IDENT@38..39 "f"
                            ARG_LIST@39..42
                              LPAREN@39..40 "("
                              NAME_REF@40..41
                                IDENT@40..41 "x"
                              RPAREN@41..42 ")"
                          SEMICOLON@42..43 ";"
                      FOR_STMT@43..77
                        WHITESPACE@43..44 " "
                        FOR_KW@44..47 "for"
                        WHITESPACE@47..48 " "
                        LPAREN@48..49 "("
                        LOCAL_VAR_DECL@49..58
                          MODIFIERS@49..49
                          TYPE@49..52
                            INT_KW@49..52 "int"
                          WHITESPACE@52..53 " "
                          IDENT@53..54 "i"
                          WHITESPACE@54..55 " "
                          EQ@55..56 "="
                          LITERAL@56..58
                            WHITESPACE@56..57 " "
                            INT_LITERAL@57..58 "0"
                        SEMICOLON@58..59 ";"
                        BINARY_EXPR@59..65
                          NAME_REF@59..61
                            WHITESPACE@59..60 " "
                            IDENT@60..61 "i"
                          WHITESPACE@61..62 " "
                          LT@62..63 "<"
                          NAME_REF@63..65
                            WHITESPACE@63..64 " "
                            IDENT@64..65 "n"
                        SEMICOLON@65..66 ";"
                        POSTFIX_EXPR@66..70
                          NAME_REF@66..68
                            WHITESPACE@66..67 " "
                            IDENT@67..68 "i"
                          PLUS_PLUS@68..70 "++"
                        RPAREN@70..71 ")"
                        EXPR_STMT@71..77
                          CALL_EXPR@71..76
                            NAME_REF@71..73
                              WHITESPACE@71..72 " "
                              IDENT@72..73 "g"
                            ARG_LIST@73..76
                              LPAREN@73..74 "("
                              NAME_REF@74..75
                                IDENT@74..75 "i"
                              RPAREN@75..76 ")"
                          SEMICOLON@76..77 ";"
                      WHITESPACE@77..78 " "
                      RBRACE@78..79 "}"
                  WHITESPACE@79..80 " "
                  RBRACE@80..81 "}"
        "#]],
    );
}

#[test]
fn for_each_wildcard_type() {
    check(
        "class C { void m(Map<K, V> mm) { for (Map.Entry<? extends K, ? super V> e : mm.entrySet()) g(e); for (List<?> x : xs) h(x); } }",
        expect![[r#"
            SOURCE_FILE@0..127
              CLASS_DECL@0..127
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..127
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..125
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..30
                      LPAREN@16..17 "("
                      PARAM@17..29
                        MODIFIERS@17..17
                        TYPE@17..26
                          IDENT@17..20 "Map"
                          TYPE_ARGS@20..26
                            LT@20..21 "<"
                            TYPE@21..22
                              IDENT@21..22 "K"
                            COMMA@22..23 ","
                            TYPE@23..25
                              WHITESPACE@23..24 " "
                              IDENT@24..25 "V"
                            GT@25..26 ">"
                        WHITESPACE@26..27 " "
                        IDENT@27..29 "mm"
                      RPAREN@29..30 ")"
                    BLOCK@30..125
                      WHITESPACE@30..31 " "
                      LBRACE@31..32 "{"
                      FOR_EACH_STMT@32..96
                        WHITESPACE@32..33 " "
                        FOR_KW@33..36 "for"
                        WHITESPACE@36..37 " "
                        LPAREN@37..38 "("
                        MODIFIERS@38..38
                        TYPE@38..71
                          IDENT@38..41 "Map"
                          DOT@41..42 "."
                          IDENT@42..47 "Entry"
                          TYPE_ARGS@47..71
                            LT@47..48 "<"
                            QUESTION@48..49 "?"
                            WHITESPACE@49..50 " "
                            EXTENDS_KW@50..57 "extends"
                            TYPE@57..59
                              WHITESPACE@57..58 " "
                              IDENT@58..59 "K"
                            COMMA@59..60 ","
                            WHITESPACE@60..61 " "
                            QUESTION@61..62 "?"
                            WHITESPACE@62..63 " "
                            SUPER_KW@63..68 "super"
                            TYPE@68..70
                              WHITESPACE@68..69 " "
                              IDENT@69..70 "V"
                            GT@70..71 ">"
                        WHITESPACE@71..72 " "
                        IDENT@72..73 "e"
                        WHITESPACE@73..74 " "
                        COLON@74..75 ":"
                        CALL_EXPR@75..89
                          FIELD_ACCESS@75..87
                            NAME_REF@75..78
                              WHITESPACE@75..76 " "
                              IDENT@76..78 "mm"
                            DOT@78..79 "."
                            IDENT@79..87 "entrySet"
                          ARG_LIST@87..89
                            LPAREN@87..88 "("
                            RPAREN@88..89 ")"
                        RPAREN@89..90 ")"
                        EXPR_STMT@90..96
                          CALL_EXPR@90..95
                            NAME_REF@90..92
                              WHITESPACE@90..91 " "
                              IDENT@91..92 "g"
                            ARG_LIST@92..95
                              LPAREN@92..93 "("
                              NAME_REF@93..94
                                IDENT@93..94 "e"
                              RPAREN@94..95 ")"
                          SEMICOLON@95..96 ";"
                      FOR_EACH_STMT@96..123
                        WHITESPACE@96..97 " "
                        FOR_KW@97..100 "for"
                        WHITESPACE@100..101 " "
                        LPAREN@101..102 "("
                        MODIFIERS@102..102
                        TYPE@102..109
                          IDENT@102..106 "List"
                          TYPE_ARGS@106..109
                            LT@106..107 "<"
                            QUESTION@107..108 "?"
                            GT@108..109 ">"
                        WHITESPACE@109..110 " "
                        IDENT@110..111 "x"
                        WHITESPACE@111..112 " "
                        COLON@112..113 ":"
                        NAME_REF@113..116
                          WHITESPACE@113..114 " "
                          IDENT@114..116 "xs"
                        RPAREN@116..117 ")"
                        EXPR_STMT@117..123
                          CALL_EXPR@117..122
                            NAME_REF@117..119
                              WHITESPACE@117..118 " "
                              IDENT@118..119 "h"
                            ARG_LIST@119..122
                              LPAREN@119..120 "("
                              NAME_REF@120..121
                                IDENT@120..121 "x"
                              RPAREN@121..122 ")"
                          SEMICOLON@122..123 ";"
                      WHITESPACE@123..124 " "
                      RBRACE@124..125 "}"
                  WHITESPACE@125..126 " "
                  RBRACE@126..127 "}"
        "#]],
    );
}

#[test]
fn try_catch_finally_resources() {
    check(
        "class C { void m() { try (var r = open()) { use(r); } catch (IOException | E e) { } finally { } } }",
        expect![[r#"
            SOURCE_FILE@0..99
              CLASS_DECL@0..99
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..99
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..97
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..97
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      TRY_STMT@20..95
                        WHITESPACE@20..21 " "
                        TRY_KW@21..24 "try"
                        RESOURCE_LIST@24..41
                          WHITESPACE@24..25 " "
                          LPAREN@25..26 "("
                          RESOURCE@26..40
                            MODIFIERS@26..26
                            TYPE@26..29
                              VAR_KW@26..29 "var"
                            WHITESPACE@29..30 " "
                            IDENT@30..31 "r"
                            WHITESPACE@31..32 " "
                            EQ@32..33 "="
                            CALL_EXPR@33..40
                              NAME_REF@33..38
                                WHITESPACE@33..34 " "
                                IDENT@34..38 "open"
                              ARG_LIST@38..40
                                LPAREN@38..39 "("
                                RPAREN@39..40 ")"
                          RPAREN@40..41 ")"
                        BLOCK@41..53
                          WHITESPACE@41..42 " "
                          LBRACE@42..43 "{"
                          EXPR_STMT@43..51
                            CALL_EXPR@43..50
                              NAME_REF@43..47
                                WHITESPACE@43..44 " "
                                IDENT@44..47 "use"
                              ARG_LIST@47..50
                                LPAREN@47..48 "("
                                NAME_REF@48..49
                                  IDENT@48..49 "r"
                                RPAREN@49..50 ")"
                            SEMICOLON@50..51 ";"
                          WHITESPACE@51..52 " "
                          RBRACE@52..53 "}"
                        CATCH_CLAUSE@53..83
                          WHITESPACE@53..54 " "
                          CATCH_KW@54..59 "catch"
                          WHITESPACE@59..60 " "
                          LPAREN@60..61 "("
                          MODIFIERS@61..61
                          TYPE@61..72
                            IDENT@61..72 "IOException"
                          WHITESPACE@72..73 " "
                          PIPE@73..74 "|"
                          TYPE@74..76
                            WHITESPACE@74..75 " "
                            IDENT@75..76 "E"
                          WHITESPACE@76..77 " "
                          IDENT@77..78 "e"
                          RPAREN@78..79 ")"
                          BLOCK@79..83
                            WHITESPACE@79..80 " "
                            LBRACE@80..81 "{"
                            WHITESPACE@81..82 " "
                            RBRACE@82..83 "}"
                        FINALLY_CLAUSE@83..95
                          WHITESPACE@83..84 " "
                          FINALLY_KW@84..91 "finally"
                          BLOCK@91..95
                            WHITESPACE@91..92 " "
                            LBRACE@92..93 "{"
                            WHITESPACE@93..94 " "
                            RBRACE@94..95 "}"
                      WHITESPACE@95..96 " "
                      RBRACE@96..97 "}"
                  WHITESPACE@97..98 " "
                  RBRACE@98..99 "}"
        "#]],
    );
}

#[test]
fn switch_statement_with_patterns() {
    check(
        "class C { void m(Object o) { switch (o) { case Integer i when i > 0 -> f(i); case String s -> g(s); default -> h(); } } }",
        expect![[r#"
            SOURCE_FILE@0..121
              CLASS_DECL@0..121
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..121
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..119
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..26
                      LPAREN@16..17 "("
                      PARAM@17..25
                        MODIFIERS@17..17
                        TYPE@17..23
                          IDENT@17..23 "Object"
                        WHITESPACE@23..24 " "
                        IDENT@24..25 "o"
                      RPAREN@25..26 ")"
                    BLOCK@26..119
                      WHITESPACE@26..27 " "
                      LBRACE@27..28 "{"
                      SWITCH_STMT@28..117
                        WHITESPACE@28..29 " "
                        SWITCH_KW@29..35 "switch"
                        WHITESPACE@35..36 " "
                        LPAREN@36..37 "("
                        NAME_REF@37..38
                          IDENT@37..38 "o"
                        RPAREN@38..39 ")"
                        SWITCH_BLOCK@39..117
                          WHITESPACE@39..40 " "
                          LBRACE@40..41 "{"
                          SWITCH_RULE@41..76
                            SWITCH_LABEL@41..67
                              WHITESPACE@41..42 " "
                              CASE_KW@42..46 "case"
                              TYPE_PATTERN@46..56
                                TYPE@46..54
                                  WHITESPACE@46..47 " "
                                  IDENT@47..54 "Integer"
                                WHITESPACE@54..55 " "
                                IDENT@55..56 "i"
                              GUARD@56..67
                                WHITESPACE@56..57 " "
                                WHEN_KW@57..61 "when"
                                BINARY_EXPR@61..67
                                  NAME_REF@61..63
                                    WHITESPACE@61..62 " "
                                    IDENT@62..63 "i"
                                  WHITESPACE@63..64 " "
                                  GT@64..65 ">"
                                  LITERAL@65..67
                                    WHITESPACE@65..66 " "
                                    INT_LITERAL@66..67 "0"
                            WHITESPACE@67..68 " "
                            ARROW@68..70 "->"
                            CALL_EXPR@70..75
                              NAME_REF@70..72
                                WHITESPACE@70..71 " "
                                IDENT@71..72 "f"
                              ARG_LIST@72..75
                                LPAREN@72..73 "("
                                NAME_REF@73..74
                                  IDENT@73..74 "i"
                                RPAREN@74..75 ")"
                            SEMICOLON@75..76 ";"
                          SWITCH_RULE@76..99
                            SWITCH_LABEL@76..90
                              WHITESPACE@76..77 " "
                              CASE_KW@77..81 "case"
                              TYPE_PATTERN@81..90
                                TYPE@81..88
                                  WHITESPACE@81..82 " "
                                  IDENT@82..88 "String"
                                WHITESPACE@88..89 " "
                                IDENT@89..90 "s"
                            WHITESPACE@90..91 " "
                            ARROW@91..93 "->"
                            CALL_EXPR@93..98
                              NAME_REF@93..95
                                WHITESPACE@93..94 " "
                                IDENT@94..95 "g"
                              ARG_LIST@95..98
                                LPAREN@95..96 "("
                                NAME_REF@96..97
                                  IDENT@96..97 "s"
                                RPAREN@97..98 ")"
                            SEMICOLON@98..99 ";"
                          SWITCH_RULE@99..115
                            SWITCH_LABEL@99..107
                              WHITESPACE@99..100 " "
                              DEFAULT_KW@100..107 "default"
                            WHITESPACE@107..108 " "
                            ARROW@108..110 "->"
                            CALL_EXPR@110..114
                              NAME_REF@110..112
                                WHITESPACE@110..111 " "
                                IDENT@111..112 "h"
                              ARG_LIST@112..114
                                LPAREN@112..113 "("
                                RPAREN@113..114 ")"
                            SEMICOLON@114..115 ";"
                          WHITESPACE@115..116 " "
                          RBRACE@116..117 "}"
                      WHITESPACE@117..118 " "
                      RBRACE@118..119 "}"
                  WHITESPACE@119..120 " "
                  RBRACE@120..121 "}"
        "#]],
    );
}

#[test]
fn switch_arrow_bare_and_multiple_enum_labels() {
    // Arrow switch with bare/multiple constant labels (`case A, B ->`) and a
    // guarded label (`case C when b ->`). The trailing `->` must stay the
    // rule arrow (not a lambda), and `when` must start a guard (not a binding).
    check(
        "class C { int m(E e, boolean b) { return switch (e) { case A, B -> 1; case C when b -> 2; default -> 0; }; } }",
        expect![[r#"
            SOURCE_FILE@0..110
              CLASS_DECL@0..110
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..110
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..108
                    MODIFIERS@9..9
                    TYPE@9..13
                      WHITESPACE@9..10 " "
                      INT_KW@10..13 "int"
                    WHITESPACE@13..14 " "
                    IDENT@14..15 "m"
                    PARAM_LIST@15..31
                      LPAREN@15..16 "("
                      PARAM@16..19
                        MODIFIERS@16..16
                        TYPE@16..17
                          IDENT@16..17 "E"
                        WHITESPACE@17..18 " "
                        IDENT@18..19 "e"
                      COMMA@19..20 ","
                      PARAM@20..30
                        MODIFIERS@20..20
                        TYPE@20..28
                          WHITESPACE@20..21 " "
                          BOOLEAN_KW@21..28 "boolean"
                        WHITESPACE@28..29 " "
                        IDENT@29..30 "b"
                      RPAREN@30..31 ")"
                    BLOCK@31..108
                      WHITESPACE@31..32 " "
                      LBRACE@32..33 "{"
                      RETURN_STMT@33..106
                        WHITESPACE@33..34 " "
                        RETURN_KW@34..40 "return"
                        SWITCH_EXPR@40..105
                          WHITESPACE@40..41 " "
                          SWITCH_KW@41..47 "switch"
                          WHITESPACE@47..48 " "
                          LPAREN@48..49 "("
                          NAME_REF@49..50
                            IDENT@49..50 "e"
                          RPAREN@50..51 ")"
                          SWITCH_BLOCK@51..105
                            WHITESPACE@51..52 " "
                            LBRACE@52..53 "{"
                            SWITCH_RULE@53..69
                              SWITCH_LABEL@53..63
                                WHITESPACE@53..54 " "
                                CASE_KW@54..58 "case"
                                NAME_REF@58..60
                                  WHITESPACE@58..59 " "
                                  IDENT@59..60 "A"
                                COMMA@60..61 ","
                                NAME_REF@61..63
                                  WHITESPACE@61..62 " "
                                  IDENT@62..63 "B"
                              WHITESPACE@63..64 " "
                              ARROW@64..66 "->"
                              LITERAL@66..68
                                WHITESPACE@66..67 " "
                                INT_LITERAL@67..68 "1"
                              SEMICOLON@68..69 ";"
                            SWITCH_RULE@69..89
                              SWITCH_LABEL@69..83
                                WHITESPACE@69..70 " "
                                CASE_KW@70..74 "case"
                                NAME_REF@74..76
                                  WHITESPACE@74..75 " "
                                  IDENT@75..76 "C"
                                GUARD@76..83
                                  WHITESPACE@76..77 " "
                                  WHEN_KW@77..81 "when"
                                  NAME_REF@81..83
                                    WHITESPACE@81..82 " "
                                    IDENT@82..83 "b"
                              WHITESPACE@83..84 " "
                              ARROW@84..86 "->"
                              LITERAL@86..88
                                WHITESPACE@86..87 " "
                                INT_LITERAL@87..88 "2"
                              SEMICOLON@88..89 ";"
                            SWITCH_RULE@89..103
                              SWITCH_LABEL@89..97
                                WHITESPACE@89..90 " "
                                DEFAULT_KW@90..97 "default"
                              WHITESPACE@97..98 " "
                              ARROW@98..100 "->"
                              LITERAL@100..102
                                WHITESPACE@100..101 " "
                                INT_LITERAL@101..102 "0"
                              SEMICOLON@102..103 ";"
                            WHITESPACE@103..104 " "
                            RBRACE@104..105 "}"
                        SEMICOLON@105..106 ";"
                      WHITESPACE@106..107 " "
                      RBRACE@107..108 "}"
                  WHITESPACE@108..109 " "
                  RBRACE@109..110 "}"
        "#]],
    );
}

#[test]
fn switch_expression_with_yield() {
    check(
        "class C { int m(int x) { return switch (x) { case 1: yield 10; default: yield 0; }; } }",
        expect![[r#"
            SOURCE_FILE@0..87
              CLASS_DECL@0..87
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..87
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..85
                    MODIFIERS@9..9
                    TYPE@9..13
                      WHITESPACE@9..10 " "
                      INT_KW@10..13 "int"
                    WHITESPACE@13..14 " "
                    IDENT@14..15 "m"
                    PARAM_LIST@15..22
                      LPAREN@15..16 "("
                      PARAM@16..21
                        MODIFIERS@16..16
                        TYPE@16..19
                          INT_KW@16..19 "int"
                        WHITESPACE@19..20 " "
                        IDENT@20..21 "x"
                      RPAREN@21..22 ")"
                    BLOCK@22..85
                      WHITESPACE@22..23 " "
                      LBRACE@23..24 "{"
                      RETURN_STMT@24..83
                        WHITESPACE@24..25 " "
                        RETURN_KW@25..31 "return"
                        SWITCH_EXPR@31..82
                          WHITESPACE@31..32 " "
                          SWITCH_KW@32..38 "switch"
                          WHITESPACE@38..39 " "
                          LPAREN@39..40 "("
                          NAME_REF@40..41
                            IDENT@40..41 "x"
                          RPAREN@41..42 ")"
                          SWITCH_BLOCK@42..82
                            WHITESPACE@42..43 " "
                            LBRACE@43..44 "{"
                            SWITCH_GROUP@44..62
                              SWITCH_LABEL@44..51
                                WHITESPACE@44..45 " "
                                CASE_KW@45..49 "case"
                                LITERAL@49..51
                                  WHITESPACE@49..50 " "
                                  INT_LITERAL@50..51 "1"
                              COLON@51..52 ":"
                              YIELD_STMT@52..62
                                WHITESPACE@52..53 " "
                                YIELD_KW@53..58 "yield"
                                LITERAL@58..61
                                  WHITESPACE@58..59 " "
                                  INT_LITERAL@59..61 "10"
                                SEMICOLON@61..62 ";"
                            SWITCH_GROUP@62..80
                              SWITCH_LABEL@62..70
                                WHITESPACE@62..63 " "
                                DEFAULT_KW@63..70 "default"
                              COLON@70..71 ":"
                              YIELD_STMT@71..80
                                WHITESPACE@71..72 " "
                                YIELD_KW@72..77 "yield"
                                LITERAL@77..79
                                  WHITESPACE@77..78 " "
                                  INT_LITERAL@78..79 "0"
                                SEMICOLON@79..80 ";"
                            WHITESPACE@80..81 " "
                            RBRACE@81..82 "}"
                        SEMICOLON@82..83 ";"
                      WHITESPACE@83..84 " "
                      RBRACE@84..85 "}"
                  WHITESPACE@85..86 " "
                  RBRACE@86..87 "}"
        "#]],
    );
}

#[test]
fn record_pattern_in_instanceof() {
    check(
        "class C { void m(Object o) { if (o instanceof Point(int x, int y)) f(); } }",
        expect![[r#"
            SOURCE_FILE@0..75
              CLASS_DECL@0..75
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..75
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..73
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..26
                      LPAREN@16..17 "("
                      PARAM@17..25
                        MODIFIERS@17..17
                        TYPE@17..23
                          IDENT@17..23 "Object"
                        WHITESPACE@23..24 " "
                        IDENT@24..25 "o"
                      RPAREN@25..26 ")"
                    BLOCK@26..73
                      WHITESPACE@26..27 " "
                      LBRACE@27..28 "{"
                      IF_STMT@28..71
                        WHITESPACE@28..29 " "
                        IF_KW@29..31 "if"
                        WHITESPACE@31..32 " "
                        LPAREN@32..33 "("
                        BINARY_EXPR@33..65
                          NAME_REF@33..34
                            IDENT@33..34 "o"
                          WHITESPACE@34..35 " "
                          INSTANCEOF_KW@35..45 "instanceof"
                          RECORD_PATTERN@45..65
                            TYPE@45..51
                              WHITESPACE@45..46 " "
                              IDENT@46..51 "Point"
                            LPAREN@51..52 "("
                            TYPE_PATTERN@52..57
                              TYPE@52..55
                                INT_KW@52..55 "int"
                              WHITESPACE@55..56 " "
                              IDENT@56..57 "x"
                            COMMA@57..58 ","
                            TYPE_PATTERN@58..64
                              TYPE@58..62
                                WHITESPACE@58..59 " "
                                INT_KW@59..62 "int"
                              WHITESPACE@62..63 " "
                              IDENT@63..64 "y"
                            RPAREN@64..65 ")"
                        RPAREN@65..66 ")"
                        EXPR_STMT@66..71
                          CALL_EXPR@66..70
                            NAME_REF@66..68
                              WHITESPACE@66..67 " "
                              IDENT@67..68 "f"
                            ARG_LIST@68..70
                              LPAREN@68..69 "("
                              RPAREN@69..70 ")"
                          SEMICOLON@70..71 ";"
                      WHITESPACE@71..72 " "
                      RBRACE@72..73 "}"
                  WHITESPACE@73..74 " "
                  RBRACE@74..75 "}"
        "#]],
    );
}

#[test]
fn unnamed_variables_in_statements() {
    check(
        "class C { void m(int[] a) { int _ = 0, _ = 1; for (var _ : a) {} try (Lock _ = null) {} catch (Exception | Error _) {} } }",
        expect![[r#"
            SOURCE_FILE@0..122
              CLASS_DECL@0..122
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..122
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..120
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..25
                      LPAREN@16..17 "("
                      PARAM@17..24
                        MODIFIERS@17..17
                        TYPE@17..22
                          INT_KW@17..20 "int"
                          LBRACK@20..21 "["
                          RBRACK@21..22 "]"
                        WHITESPACE@22..23 " "
                        IDENT@23..24 "a"
                      RPAREN@24..25 ")"
                    BLOCK@25..120
                      WHITESPACE@25..26 " "
                      LBRACE@26..27 "{"
                      LOCAL_VAR_DECL@27..45
                        MODIFIERS@27..27
                        TYPE@27..31
                          WHITESPACE@27..28 " "
                          INT_KW@28..31 "int"
                        WHITESPACE@31..32 " "
                        UNDERSCORE@32..33 "_"
                        WHITESPACE@33..34 " "
                        EQ@34..35 "="
                        LITERAL@35..37
                          WHITESPACE@35..36 " "
                          INT_LITERAL@36..37 "0"
                        COMMA@37..38 ","
                        WHITESPACE@38..39 " "
                        UNDERSCORE@39..40 "_"
                        WHITESPACE@40..41 " "
                        EQ@41..42 "="
                        LITERAL@42..44
                          WHITESPACE@42..43 " "
                          INT_LITERAL@43..44 "1"
                        SEMICOLON@44..45 ";"
                      FOR_EACH_STMT@45..64
                        WHITESPACE@45..46 " "
                        FOR_KW@46..49 "for"
                        WHITESPACE@49..50 " "
                        LPAREN@50..51 "("
                        MODIFIERS@51..51
                        TYPE@51..54
                          VAR_KW@51..54 "var"
                        WHITESPACE@54..55 " "
                        UNDERSCORE@55..56 "_"
                        WHITESPACE@56..57 " "
                        COLON@57..58 ":"
                        NAME_REF@58..60
                          WHITESPACE@58..59 " "
                          IDENT@59..60 "a"
                        RPAREN@60..61 ")"
                        BLOCK@61..64
                          WHITESPACE@61..62 " "
                          LBRACE@62..63 "{"
                          RBRACE@63..64 "}"
                      TRY_STMT@64..118
                        WHITESPACE@64..65 " "
                        TRY_KW@65..68 "try"
                        RESOURCE_LIST@68..84
                          WHITESPACE@68..69 " "
                          LPAREN@69..70 "("
                          RESOURCE@70..83
                            MODIFIERS@70..70
                            TYPE@70..74
                              IDENT@70..74 "Lock"
                            WHITESPACE@74..75 " "
                            UNDERSCORE@75..76 "_"
                            WHITESPACE@76..77 " "
                            EQ@77..78 "="
                            LITERAL@78..83
                              WHITESPACE@78..79 " "
                              NULL_KW@79..83 "null"
                          RPAREN@83..84 ")"
                        BLOCK@84..87
                          WHITESPACE@84..85 " "
                          LBRACE@85..86 "{"
                          RBRACE@86..87 "}"
                        CATCH_CLAUSE@87..118
                          WHITESPACE@87..88 " "
                          CATCH_KW@88..93 "catch"
                          WHITESPACE@93..94 " "
                          LPAREN@94..95 "("
                          MODIFIERS@95..95
                          TYPE@95..104
                            IDENT@95..104 "Exception"
                          WHITESPACE@104..105 " "
                          PIPE@105..106 "|"
                          TYPE@106..112
                            WHITESPACE@106..107 " "
                            IDENT@107..112 "Error"
                          WHITESPACE@112..113 " "
                          UNDERSCORE@113..114 "_"
                          RPAREN@114..115 ")"
                          BLOCK@115..118
                            WHITESPACE@115..116 " "
                            LBRACE@116..117 "{"
                            RBRACE@117..118 "}"
                      WHITESPACE@118..119 " "
                      RBRACE@119..120 "}"
                  WHITESPACE@120..121 " "
                  RBRACE@121..122 "}"
        "#]],
    );
}

#[test]
fn unnamed_lambda_parameters() {
    check(
        "class C { void m() { f((_, _) -> {}); g((var _, var _) -> {}); h((int _, int b) -> {}); } }",
        expect![[r#"
            SOURCE_FILE@0..91
              CLASS_DECL@0..91
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..91
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..89
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..89
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      EXPR_STMT@20..37
                        CALL_EXPR@20..36
                          NAME_REF@20..22
                            WHITESPACE@20..21 " "
                            IDENT@21..22 "f"
                          ARG_LIST@22..36
                            LPAREN@22..23 "("
                            LAMBDA_EXPR@23..35
                              LAMBDA_PARAMS@23..29
                                LPAREN@23..24 "("
                                PARAM@24..25
                                  UNDERSCORE@24..25 "_"
                                COMMA@25..26 ","
                                PARAM@26..28
                                  WHITESPACE@26..27 " "
                                  UNDERSCORE@27..28 "_"
                                RPAREN@28..29 ")"
                              WHITESPACE@29..30 " "
                              ARROW@30..32 "->"
                              BLOCK@32..35
                                WHITESPACE@32..33 " "
                                LBRACE@33..34 "{"
                                RBRACE@34..35 "}"
                            RPAREN@35..36 ")"
                        SEMICOLON@36..37 ";"
                      EXPR_STMT@37..62
                        CALL_EXPR@37..61
                          NAME_REF@37..39
                            WHITESPACE@37..38 " "
                            IDENT@38..39 "g"
                          ARG_LIST@39..61
                            LPAREN@39..40 "("
                            LAMBDA_EXPR@40..60
                              LAMBDA_PARAMS@40..54
                                LPAREN@40..41 "("
                                PARAM@41..46
                                  MODIFIERS@41..41
                                  TYPE@41..44
                                    VAR_KW@41..44 "var"
                                  WHITESPACE@44..45 " "
                                  UNDERSCORE@45..46 "_"
                                COMMA@46..47 ","
                                PARAM@47..53
                                  MODIFIERS@47..47
                                  TYPE@47..51
                                    WHITESPACE@47..48 " "
                                    VAR_KW@48..51 "var"
                                  WHITESPACE@51..52 " "
                                  UNDERSCORE@52..53 "_"
                                RPAREN@53..54 ")"
                              WHITESPACE@54..55 " "
                              ARROW@55..57 "->"
                              BLOCK@57..60
                                WHITESPACE@57..58 " "
                                LBRACE@58..59 "{"
                                RBRACE@59..60 "}"
                            RPAREN@60..61 ")"
                        SEMICOLON@61..62 ";"
                      EXPR_STMT@62..87
                        CALL_EXPR@62..86
                          NAME_REF@62..64
                            WHITESPACE@62..63 " "
                            IDENT@63..64 "h"
                          ARG_LIST@64..86
                            LPAREN@64..65 "("
                            LAMBDA_EXPR@65..85
                              LAMBDA_PARAMS@65..79
                                LPAREN@65..66 "("
                                PARAM@66..71
                                  MODIFIERS@66..66
                                  TYPE@66..69
                                    INT_KW@66..69 "int"
                                  WHITESPACE@69..70 " "
                                  UNDERSCORE@70..71 "_"
                                COMMA@71..72 ","
                                PARAM@72..78
                                  MODIFIERS@72..72
                                  TYPE@72..76
                                    WHITESPACE@72..73 " "
                                    INT_KW@73..76 "int"
                                  WHITESPACE@76..77 " "
                                  IDENT@77..78 "b"
                                RPAREN@78..79 ")"
                              WHITESPACE@79..80 " "
                              ARROW@80..82 "->"
                              BLOCK@82..85
                                WHITESPACE@82..83 " "
                                LBRACE@83..84 "{"
                                RBRACE@84..85 "}"
                            RPAREN@85..86 ")"
                        SEMICOLON@86..87 ";"
                      WHITESPACE@87..88 " "
                      RBRACE@88..89 "}"
                  WHITESPACE@89..90 " "
                  RBRACE@90..91 "}"
        "#]],
    );
}

#[test]
fn unnamed_and_record_patterns_in_instanceof() {
    check(
        "class C { void m(Object o) { if (o instanceof R _) {} if (o instanceof R(_)) {} } }",
        expect![[r#"
            SOURCE_FILE@0..83
              CLASS_DECL@0..83
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..83
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..81
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..26
                      LPAREN@16..17 "("
                      PARAM@17..25
                        MODIFIERS@17..17
                        TYPE@17..23
                          IDENT@17..23 "Object"
                        WHITESPACE@23..24 " "
                        IDENT@24..25 "o"
                      RPAREN@25..26 ")"
                    BLOCK@26..81
                      WHITESPACE@26..27 " "
                      LBRACE@27..28 "{"
                      IF_STMT@28..53
                        WHITESPACE@28..29 " "
                        IF_KW@29..31 "if"
                        WHITESPACE@31..32 " "
                        LPAREN@32..33 "("
                        BINARY_EXPR@33..49
                          NAME_REF@33..34
                            IDENT@33..34 "o"
                          WHITESPACE@34..35 " "
                          INSTANCEOF_KW@35..45 "instanceof"
                          TYPE_PATTERN@45..49
                            TYPE@45..47
                              WHITESPACE@45..46 " "
                              IDENT@46..47 "R"
                            WHITESPACE@47..48 " "
                            UNDERSCORE@48..49 "_"
                        RPAREN@49..50 ")"
                        BLOCK@50..53
                          WHITESPACE@50..51 " "
                          LBRACE@51..52 "{"
                          RBRACE@52..53 "}"
                      IF_STMT@53..79
                        WHITESPACE@53..54 " "
                        IF_KW@54..56 "if"
                        WHITESPACE@56..57 " "
                        LPAREN@57..58 "("
                        BINARY_EXPR@58..75
                          NAME_REF@58..59
                            IDENT@58..59 "o"
                          WHITESPACE@59..60 " "
                          INSTANCEOF_KW@60..70 "instanceof"
                          RECORD_PATTERN@70..75
                            TYPE@70..72
                              WHITESPACE@70..71 " "
                              IDENT@71..72 "R"
                            LPAREN@72..73 "("
                            UNNAMED_PATTERN@73..74
                              UNDERSCORE@73..74 "_"
                            RPAREN@74..75 ")"
                        RPAREN@75..76 ")"
                        BLOCK@76..79
                          WHITESPACE@76..77 " "
                          LBRACE@77..78 "{"
                          RBRACE@78..79 "}"
                      WHITESPACE@79..80 " "
                      RBRACE@80..81 "}"
                  WHITESPACE@81..82 " "
                  RBRACE@82..83 "}"
        "#]],
    );
}

#[test]
fn switch_patterns_unnamed_and_modifiers() {
    check(
        "class C { int m(Object o) { return switch (o) { case Float _ -> 1; case R1 _, R2 _ -> 2; case ARecord(final String s) -> 3; case R(@A var x) -> 4; default -> 0; }; } }",
        expect![[r#"
            SOURCE_FILE@0..167
              CLASS_DECL@0..167
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..167
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..165
                    MODIFIERS@9..9
                    TYPE@9..13
                      WHITESPACE@9..10 " "
                      INT_KW@10..13 "int"
                    WHITESPACE@13..14 " "
                    IDENT@14..15 "m"
                    PARAM_LIST@15..25
                      LPAREN@15..16 "("
                      PARAM@16..24
                        MODIFIERS@16..16
                        TYPE@16..22
                          IDENT@16..22 "Object"
                        WHITESPACE@22..23 " "
                        IDENT@23..24 "o"
                      RPAREN@24..25 ")"
                    BLOCK@25..165
                      WHITESPACE@25..26 " "
                      LBRACE@26..27 "{"
                      RETURN_STMT@27..163
                        WHITESPACE@27..28 " "
                        RETURN_KW@28..34 "return"
                        SWITCH_EXPR@34..162
                          WHITESPACE@34..35 " "
                          SWITCH_KW@35..41 "switch"
                          WHITESPACE@41..42 " "
                          LPAREN@42..43 "("
                          NAME_REF@43..44
                            IDENT@43..44 "o"
                          RPAREN@44..45 ")"
                          SWITCH_BLOCK@45..162
                            WHITESPACE@45..46 " "
                            LBRACE@46..47 "{"
                            SWITCH_RULE@47..66
                              SWITCH_LABEL@47..60
                                WHITESPACE@47..48 " "
                                CASE_KW@48..52 "case"
                                TYPE_PATTERN@52..60
                                  TYPE@52..58
                                    WHITESPACE@52..53 " "
                                    IDENT@53..58 "Float"
                                  WHITESPACE@58..59 " "
                                  UNDERSCORE@59..60 "_"
                              WHITESPACE@60..61 " "
                              ARROW@61..63 "->"
                              LITERAL@63..65
                                WHITESPACE@63..64 " "
                                INT_LITERAL@64..65 "1"
                              SEMICOLON@65..66 ";"
                            SWITCH_RULE@66..88
                              SWITCH_LABEL@66..82
                                WHITESPACE@66..67 " "
                                CASE_KW@67..71 "case"
                                TYPE_PATTERN@71..76
                                  TYPE@71..74
                                    WHITESPACE@71..72 " "
                                    IDENT@72..74 "R1"
                                  WHITESPACE@74..75 " "
                                  UNDERSCORE@75..76 "_"
                                COMMA@76..77 ","
                                TYPE_PATTERN@77..82
                                  TYPE@77..80
                                    WHITESPACE@77..78 " "
                                    IDENT@78..80 "R2"
                                  WHITESPACE@80..81 " "
                                  UNDERSCORE@81..82 "_"
                              WHITESPACE@82..83 " "
                              ARROW@83..85 "->"
                              LITERAL@85..87
                                WHITESPACE@85..86 " "
                                INT_LITERAL@86..87 "2"
                              SEMICOLON@87..88 ";"
                            SWITCH_RULE@88..123
                              SWITCH_LABEL@88..117
                                WHITESPACE@88..89 " "
                                CASE_KW@89..93 "case"
                                RECORD_PATTERN@93..117
                                  TYPE@93..101
                                    WHITESPACE@93..94 " "
                                    IDENT@94..101 "ARecord"
                                  LPAREN@101..102 "("
                                  TYPE_PATTERN@102..116
                                    MODIFIERS@102..107
                                      FINAL_KW@102..107 "final"
                                    TYPE@107..114
                                      WHITESPACE@107..108 " "
                                      IDENT@108..114 "String"
                                    WHITESPACE@114..115 " "
                                    IDENT@115..116 "s"
                                  RPAREN@116..117 ")"
                              WHITESPACE@117..118 " "
                              ARROW@118..120 "->"
                              LITERAL@120..122
                                WHITESPACE@120..121 " "
                                INT_LITERAL@121..122 "3"
                              SEMICOLON@122..123 ";"
                            SWITCH_RULE@123..146
                              SWITCH_LABEL@123..140
                                WHITESPACE@123..124 " "
                                CASE_KW@124..128 "case"
                                RECORD_PATTERN@128..140
                                  TYPE@128..130
                                    WHITESPACE@128..129 " "
                                    IDENT@129..130 "R"
                                  LPAREN@130..131 "("
                                  TYPE_PATTERN@131..139
                                    TYPE@131..137
                                      ANNOTATION@131..133
                                        AT@131..132 "@"
                                        QUALIFIED_NAME@132..133
                                          IDENT@132..133 "A"
                                      WHITESPACE@133..134 " "
                                      VAR_KW@134..137 "var"
                                    WHITESPACE@137..138 " "
                                    IDENT@138..139 "x"
                                  RPAREN@139..140 ")"
                              WHITESPACE@140..141 " "
                              ARROW@141..143 "->"
                              LITERAL@143..145
                                WHITESPACE@143..144 " "
                                INT_LITERAL@144..145 "4"
                              SEMICOLON@145..146 ";"
                            SWITCH_RULE@146..160
                              SWITCH_LABEL@146..154
                                WHITESPACE@146..147 " "
                                DEFAULT_KW@147..154 "default"
                              WHITESPACE@154..155 " "
                              ARROW@155..157 "->"
                              LITERAL@157..159
                                WHITESPACE@157..158 " "
                                INT_LITERAL@158..159 "0"
                              SEMICOLON@159..160 ";"
                            WHITESPACE@160..161 " "
                            RBRACE@161..162 "}"
                        SEMICOLON@162..163 ";"
                      WHITESPACE@163..164 " "
                      RBRACE@164..165 "}"
                  WHITESPACE@165..166 " "
                  RBRACE@166..167 "}"
        "#]],
    );
}

#[test]
fn instanceof_annotated_type_without_binding_is_a_type() {
    // Regression: a leading type-use annotation with no binding stays a plain type
    // (`TYPE`, no `TYPE_PATTERN`/`MODIFIERS`), not a pattern.
    check(
        "class C { void m(Object o) { if (o instanceof @DA String) {} } }",
        expect![[r#"
            SOURCE_FILE@0..64
              CLASS_DECL@0..64
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..64
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..62
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..26
                      LPAREN@16..17 "("
                      PARAM@17..25
                        MODIFIERS@17..17
                        TYPE@17..23
                          IDENT@17..23 "Object"
                        WHITESPACE@23..24 " "
                        IDENT@24..25 "o"
                      RPAREN@25..26 ")"
                    BLOCK@26..62
                      WHITESPACE@26..27 " "
                      LBRACE@27..28 "{"
                      IF_STMT@28..60
                        WHITESPACE@28..29 " "
                        IF_KW@29..31 "if"
                        WHITESPACE@31..32 " "
                        LPAREN@32..33 "("
                        BINARY_EXPR@33..56
                          NAME_REF@33..34
                            IDENT@33..34 "o"
                          WHITESPACE@34..35 " "
                          INSTANCEOF_KW@35..45 "instanceof"
                          TYPE@45..56
                            ANNOTATION@45..49
                              WHITESPACE@45..46 " "
                              AT@46..47 "@"
                              QUALIFIED_NAME@47..49
                                IDENT@47..49 "DA"
                            WHITESPACE@49..50 " "
                            IDENT@50..56 "String"
                        RPAREN@56..57 ")"
                        BLOCK@57..60
                          WHITESPACE@57..58 " "
                          LBRACE@58..59 "{"
                          RBRACE@59..60 "}"
                      WHITESPACE@60..61 " "
                      RBRACE@61..62 "}"
                  WHITESPACE@62..63 " "
                  RBRACE@63..64 "}"
        "#]],
    );
}

#[test]
fn lambda_forms() {
    check(
        "class C { void m() { f(x -> x + 1); g((a, b) -> a * b); h(() -> { return 0; }); i((int z) -> z); } }",
        expect![[r#"
            SOURCE_FILE@0..100
              CLASS_DECL@0..100
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..100
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..98
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..98
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      EXPR_STMT@20..35
                        CALL_EXPR@20..34
                          NAME_REF@20..22
                            WHITESPACE@20..21 " "
                            IDENT@21..22 "f"
                          ARG_LIST@22..34
                            LPAREN@22..23 "("
                            LAMBDA_EXPR@23..33
                              LAMBDA_PARAMS@23..24
                                PARAM@23..24
                                  IDENT@23..24 "x"
                              WHITESPACE@24..25 " "
                              ARROW@25..27 "->"
                              BINARY_EXPR@27..33
                                NAME_REF@27..29
                                  WHITESPACE@27..28 " "
                                  IDENT@28..29 "x"
                                WHITESPACE@29..30 " "
                                PLUS@30..31 "+"
                                LITERAL@31..33
                                  WHITESPACE@31..32 " "
                                  INT_LITERAL@32..33 "1"
                            RPAREN@33..34 ")"
                        SEMICOLON@34..35 ";"
                      EXPR_STMT@35..55
                        CALL_EXPR@35..54
                          NAME_REF@35..37
                            WHITESPACE@35..36 " "
                            IDENT@36..37 "g"
                          ARG_LIST@37..54
                            LPAREN@37..38 "("
                            LAMBDA_EXPR@38..53
                              LAMBDA_PARAMS@38..44
                                LPAREN@38..39 "("
                                PARAM@39..40
                                  IDENT@39..40 "a"
                                COMMA@40..41 ","
                                PARAM@41..43
                                  WHITESPACE@41..42 " "
                                  IDENT@42..43 "b"
                                RPAREN@43..44 ")"
                              WHITESPACE@44..45 " "
                              ARROW@45..47 "->"
                              BINARY_EXPR@47..53
                                NAME_REF@47..49
                                  WHITESPACE@47..48 " "
                                  IDENT@48..49 "a"
                                WHITESPACE@49..50 " "
                                STAR@50..51 "*"
                                NAME_REF@51..53
                                  WHITESPACE@51..52 " "
                                  IDENT@52..53 "b"
                            RPAREN@53..54 ")"
                        SEMICOLON@54..55 ";"
                      EXPR_STMT@55..79
                        CALL_EXPR@55..78
                          NAME_REF@55..57
                            WHITESPACE@55..56 " "
                            IDENT@56..57 "h"
                          ARG_LIST@57..78
                            LPAREN@57..58 "("
                            LAMBDA_EXPR@58..77
                              LAMBDA_PARAMS@58..60
                                LPAREN@58..59 "("
                                RPAREN@59..60 ")"
                              WHITESPACE@60..61 " "
                              ARROW@61..63 "->"
                              BLOCK@63..77
                                WHITESPACE@63..64 " "
                                LBRACE@64..65 "{"
                                RETURN_STMT@65..75
                                  WHITESPACE@65..66 " "
                                  RETURN_KW@66..72 "return"
                                  LITERAL@72..74
                                    WHITESPACE@72..73 " "
                                    INT_LITERAL@73..74 "0"
                                  SEMICOLON@74..75 ";"
                                WHITESPACE@75..76 " "
                                RBRACE@76..77 "}"
                            RPAREN@77..78 ")"
                        SEMICOLON@78..79 ";"
                      EXPR_STMT@79..96
                        CALL_EXPR@79..95
                          NAME_REF@79..81
                            WHITESPACE@79..80 " "
                            IDENT@80..81 "i"
                          ARG_LIST@81..95
                            LPAREN@81..82 "("
                            LAMBDA_EXPR@82..94
                              LAMBDA_PARAMS@82..89
                                LPAREN@82..83 "("
                                PARAM@83..88
                                  MODIFIERS@83..83
                                  TYPE@83..86
                                    INT_KW@83..86 "int"
                                  WHITESPACE@86..87 " "
                                  IDENT@87..88 "z"
                                RPAREN@88..89 ")"
                              WHITESPACE@89..90 " "
                              ARROW@90..92 "->"
                              NAME_REF@92..94
                                WHITESPACE@92..93 " "
                                IDENT@93..94 "z"
                            RPAREN@94..95 ")"
                        SEMICOLON@95..96 ";"
                      WHITESPACE@96..97 " "
                      RBRACE@97..98 "}"
                  WHITESPACE@98..99 " "
                  RBRACE@99..100 "}"
        "#]],
    );
}

#[test]
fn method_reference_and_class_literal() {
    check(
        "class C { void m() { f(String::valueOf); g(C::new); var k = String.class; } }",
        expect![[r#"
            SOURCE_FILE@0..77
              CLASS_DECL@0..77
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..77
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..75
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..75
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      EXPR_STMT@20..40
                        CALL_EXPR@20..39
                          NAME_REF@20..22
                            WHITESPACE@20..21 " "
                            IDENT@21..22 "f"
                          ARG_LIST@22..39
                            LPAREN@22..23 "("
                            METHOD_REF_EXPR@23..38
                              NAME_REF@23..29
                                IDENT@23..29 "String"
                              COLON_COLON@29..31 "::"
                              IDENT@31..38 "valueOf"
                            RPAREN@38..39 ")"
                        SEMICOLON@39..40 ";"
                      EXPR_STMT@40..51
                        CALL_EXPR@40..50
                          NAME_REF@40..42
                            WHITESPACE@40..41 " "
                            IDENT@41..42 "g"
                          ARG_LIST@42..50
                            LPAREN@42..43 "("
                            METHOD_REF_EXPR@43..49
                              NAME_REF@43..44
                                IDENT@43..44 "C"
                              COLON_COLON@44..46 "::"
                              NEW_KW@46..49 "new"
                            RPAREN@49..50 ")"
                        SEMICOLON@50..51 ";"
                      LOCAL_VAR_DECL@51..73
                        MODIFIERS@51..51
                        TYPE@51..55
                          WHITESPACE@51..52 " "
                          VAR_KW@52..55 "var"
                        WHITESPACE@55..56 " "
                        IDENT@56..57 "k"
                        WHITESPACE@57..58 " "
                        EQ@58..59 "="
                        CLASS_LITERAL@59..72
                          NAME_REF@59..66
                            WHITESPACE@59..60 " "
                            IDENT@60..66 "String"
                          DOT@66..67 "."
                          CLASS_KW@67..72 "class"
                        SEMICOLON@72..73 ";"
                      WHITESPACE@73..74 " "
                      RBRACE@74..75 "}"
                  WHITESPACE@75..76 " "
                  RBRACE@76..77 "}"
        "#]],
    );
}

#[test]
fn primitive_class_literals() {
    check(
        "class C { void m() { f(int.class); g(void.class); h(boolean.class); } }",
        expect![[r#"
            SOURCE_FILE@0..71
              CLASS_DECL@0..71
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..71
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..69
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..69
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      EXPR_STMT@20..34
                        CALL_EXPR@20..33
                          NAME_REF@20..22
                            WHITESPACE@20..21 " "
                            IDENT@21..22 "f"
                          ARG_LIST@22..33
                            LPAREN@22..23 "("
                            CLASS_LITERAL@23..32
                              TYPE@23..26
                                INT_KW@23..26 "int"
                              DOT@26..27 "."
                              CLASS_KW@27..32 "class"
                            RPAREN@32..33 ")"
                        SEMICOLON@33..34 ";"
                      EXPR_STMT@34..49
                        CALL_EXPR@34..48
                          NAME_REF@34..36
                            WHITESPACE@34..35 " "
                            IDENT@35..36 "g"
                          ARG_LIST@36..48
                            LPAREN@36..37 "("
                            CLASS_LITERAL@37..47
                              TYPE@37..41
                                VOID_KW@37..41 "void"
                              DOT@41..42 "."
                              CLASS_KW@42..47 "class"
                            RPAREN@47..48 ")"
                        SEMICOLON@48..49 ";"
                      EXPR_STMT@49..67
                        CALL_EXPR@49..66
                          NAME_REF@49..51
                            WHITESPACE@49..50 " "
                            IDENT@50..51 "h"
                          ARG_LIST@51..66
                            LPAREN@51..52 "("
                            CLASS_LITERAL@52..65
                              TYPE@52..59
                                BOOLEAN_KW@52..59 "boolean"
                              DOT@59..60 "."
                              CLASS_KW@60..65 "class"
                            RPAREN@65..66 ")"
                        SEMICOLON@66..67 ";"
                      WHITESPACE@67..68 " "
                      RBRACE@68..69 "}"
                  WHITESPACE@69..70 " "
                  RBRACE@70..71 "}"
        "#]],
    );
}

#[test]
fn array_class_literals() {
    check(
        "class C { void m() { f(int[].class); g(long[][][].class); h(String[].class); i(java.lang.String[][].class); } }",
        expect![[r#"
            SOURCE_FILE@0..111
              CLASS_DECL@0..111
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..111
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..109
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..109
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      EXPR_STMT@20..36
                        CALL_EXPR@20..35
                          NAME_REF@20..22
                            WHITESPACE@20..21 " "
                            IDENT@21..22 "f"
                          ARG_LIST@22..35
                            LPAREN@22..23 "("
                            CLASS_LITERAL@23..34
                              TYPE@23..28
                                INT_KW@23..26 "int"
                                LBRACK@26..27 "["
                                RBRACK@27..28 "]"
                              DOT@28..29 "."
                              CLASS_KW@29..34 "class"
                            RPAREN@34..35 ")"
                        SEMICOLON@35..36 ";"
                      EXPR_STMT@36..57
                        CALL_EXPR@36..56
                          NAME_REF@36..38
                            WHITESPACE@36..37 " "
                            IDENT@37..38 "g"
                          ARG_LIST@38..56
                            LPAREN@38..39 "("
                            CLASS_LITERAL@39..55
                              TYPE@39..49
                                LONG_KW@39..43 "long"
                                LBRACK@43..44 "["
                                RBRACK@44..45 "]"
                                LBRACK@45..46 "["
                                RBRACK@46..47 "]"
                                LBRACK@47..48 "["
                                RBRACK@48..49 "]"
                              DOT@49..50 "."
                              CLASS_KW@50..55 "class"
                            RPAREN@55..56 ")"
                        SEMICOLON@56..57 ";"
                      EXPR_STMT@57..76
                        CALL_EXPR@57..75
                          NAME_REF@57..59
                            WHITESPACE@57..58 " "
                            IDENT@58..59 "h"
                          ARG_LIST@59..75
                            LPAREN@59..60 "("
                            CLASS_LITERAL@60..74
                              NAME_REF@60..66
                                IDENT@60..66 "String"
                              LBRACK@66..67 "["
                              RBRACK@67..68 "]"
                              DOT@68..69 "."
                              CLASS_KW@69..74 "class"
                            RPAREN@74..75 ")"
                        SEMICOLON@75..76 ";"
                      EXPR_STMT@76..107
                        CALL_EXPR@76..106
                          NAME_REF@76..78
                            WHITESPACE@76..77 " "
                            IDENT@77..78 "i"
                          ARG_LIST@78..106
                            LPAREN@78..79 "("
                            CLASS_LITERAL@79..105
                              FIELD_ACCESS@79..95
                                FIELD_ACCESS@79..88
                                  NAME_REF@79..83
                                    IDENT@79..83 "java"
                                  DOT@83..84 "."
                                  IDENT@84..88 "lang"
                                DOT@88..89 "."
                                IDENT@89..95 "String"
                              LBRACK@95..96 "["
                              RBRACK@96..97 "]"
                              LBRACK@97..98 "["
                              RBRACK@98..99 "]"
                              DOT@99..100 "."
                              CLASS_KW@100..105 "class"
                            RPAREN@105..106 ")"
                        SEMICOLON@106..107 ";"
                      WHITESPACE@107..108 " "
                      RBRACE@108..109 "}"
                  WHITESPACE@109..110 " "
                  RBRACE@110..111 "}"
        "#]],
    );
}

#[test]
fn array_method_refs() {
    check(
        "class C { void m() { f(String[]::new); g(int[]::new); h(int[][]::new); i(java.lang.String[][]::new); j(Map.Entry[]::new); k(a[0]::toString); } }",
        expect![[r#"
            SOURCE_FILE@0..144
              CLASS_DECL@0..144
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..144
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..142
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..142
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      EXPR_STMT@20..38
                        CALL_EXPR@20..37
                          NAME_REF@20..22
                            WHITESPACE@20..21 " "
                            IDENT@21..22 "f"
                          ARG_LIST@22..37
                            LPAREN@22..23 "("
                            METHOD_REF_EXPR@23..36
                              NAME_REF@23..29
                                IDENT@23..29 "String"
                              LBRACK@29..30 "["
                              RBRACK@30..31 "]"
                              COLON_COLON@31..33 "::"
                              NEW_KW@33..36 "new"
                            RPAREN@36..37 ")"
                        SEMICOLON@37..38 ";"
                      EXPR_STMT@38..53
                        CALL_EXPR@38..52
                          NAME_REF@38..40
                            WHITESPACE@38..39 " "
                            IDENT@39..40 "g"
                          ARG_LIST@40..52
                            LPAREN@40..41 "("
                            METHOD_REF_EXPR@41..51
                              TYPE@41..46
                                INT_KW@41..44 "int"
                                LBRACK@44..45 "["
                                RBRACK@45..46 "]"
                              COLON_COLON@46..48 "::"
                              NEW_KW@48..51 "new"
                            RPAREN@51..52 ")"
                        SEMICOLON@52..53 ";"
                      EXPR_STMT@53..70
                        CALL_EXPR@53..69
                          NAME_REF@53..55
                            WHITESPACE@53..54 " "
                            IDENT@54..55 "h"
                          ARG_LIST@55..69
                            LPAREN@55..56 "("
                            METHOD_REF_EXPR@56..68
                              TYPE@56..63
                                INT_KW@56..59 "int"
                                LBRACK@59..60 "["
                                RBRACK@60..61 "]"
                                LBRACK@61..62 "["
                                RBRACK@62..63 "]"
                              COLON_COLON@63..65 "::"
                              NEW_KW@65..68 "new"
                            RPAREN@68..69 ")"
                        SEMICOLON@69..70 ";"
                      EXPR_STMT@70..100
                        CALL_EXPR@70..99
                          NAME_REF@70..72
                            WHITESPACE@70..71 " "
                            IDENT@71..72 "i"
                          ARG_LIST@72..99
                            LPAREN@72..73 "("
                            METHOD_REF_EXPR@73..98
                              FIELD_ACCESS@73..89
                                FIELD_ACCESS@73..82
                                  NAME_REF@73..77
                                    IDENT@73..77 "java"
                                  DOT@77..78 "."
                                  IDENT@78..82 "lang"
                                DOT@82..83 "."
                                IDENT@83..89 "String"
                              LBRACK@89..90 "["
                              RBRACK@90..91 "]"
                              LBRACK@91..92 "["
                              RBRACK@92..93 "]"
                              COLON_COLON@93..95 "::"
                              NEW_KW@95..98 "new"
                            RPAREN@98..99 ")"
                        SEMICOLON@99..100 ";"
                      EXPR_STMT@100..121
                        CALL_EXPR@100..120
                          NAME_REF@100..102
                            WHITESPACE@100..101 " "
                            IDENT@101..102 "j"
                          ARG_LIST@102..120
                            LPAREN@102..103 "("
                            METHOD_REF_EXPR@103..119
                              FIELD_ACCESS@103..112
                                NAME_REF@103..106
                                  IDENT@103..106 "Map"
                                DOT@106..107 "."
                                IDENT@107..112 "Entry"
                              LBRACK@112..113 "["
                              RBRACK@113..114 "]"
                              COLON_COLON@114..116 "::"
                              NEW_KW@116..119 "new"
                            RPAREN@119..120 ")"
                        SEMICOLON@120..121 ";"
                      EXPR_STMT@121..140
                        CALL_EXPR@121..139
                          NAME_REF@121..123
                            WHITESPACE@121..122 " "
                            IDENT@122..123 "k"
                          ARG_LIST@123..139
                            LPAREN@123..124 "("
                            METHOD_REF_EXPR@124..138
                              INDEX_EXPR@124..128
                                NAME_REF@124..125
                                  IDENT@124..125 "a"
                                LBRACK@125..126 "["
                                LITERAL@126..127
                                  INT_LITERAL@126..127 "0"
                                RBRACK@127..128 "]"
                              COLON_COLON@128..130 "::"
                              IDENT@130..138 "toString"
                            RPAREN@138..139 ")"
                        SEMICOLON@139..140 ";"
                      WHITESPACE@140..141 " "
                      RBRACE@141..142 "}"
                  WHITESPACE@142..143 " "
                  RBRACE@143..144 "}"
        "#]],
    );
}

#[test]
fn class_literal_statement_position() {
    // `int.class` at statement start must parse as an expression statement, not be
    // mistaken for the start of a local variable declaration (javac rejects the
    // assignment semantically, but it is syntactically well-formed).
    check(
        "class C { void f() { int.class = null; } }",
        expect![[r#"
        SOURCE_FILE@0..42
          CLASS_DECL@0..42
            MODIFIERS@0..0
            CLASS_KW@0..5 "class"
            WHITESPACE@5..6 " "
            IDENT@6..7 "C"
            CLASS_BODY@7..42
              WHITESPACE@7..8 " "
              LBRACE@8..9 "{"
              METHOD_DECL@9..40
                MODIFIERS@9..9
                TYPE@9..14
                  WHITESPACE@9..10 " "
                  VOID_KW@10..14 "void"
                WHITESPACE@14..15 " "
                IDENT@15..16 "f"
                PARAM_LIST@16..18
                  LPAREN@16..17 "("
                  RPAREN@17..18 ")"
                BLOCK@18..40
                  WHITESPACE@18..19 " "
                  LBRACE@19..20 "{"
                  EXPR_STMT@20..38
                    ASSIGNMENT_EXPR@20..37
                      CLASS_LITERAL@20..30
                        TYPE@20..24
                          WHITESPACE@20..21 " "
                          INT_KW@21..24 "int"
                        DOT@24..25 "."
                        CLASS_KW@25..30 "class"
                      WHITESPACE@30..31 " "
                      EQ@31..32 "="
                      LITERAL@32..37
                        WHITESPACE@32..33 " "
                        NULL_KW@33..37 "null"
                    SEMICOLON@37..38 ";"
                  WHITESPACE@38..39 " "
                  RBRACE@39..40 "}"
              WHITESPACE@40..41 " "
              RBRACE@41..42 "}"
    "#]],
    );
}

#[test]
fn class_literal_error_recovery() {
    // `f(int)` and `a[]` keep their pre-existing error recovery; `int.foo` and
    // `int[].foo` recover as a CLASS_LITERAL missing its `class` keyword.
    check(
        "class C { void m() { f(int); g(int.foo); h(int[].foo); var x = a[]; } }",
        expect![[r#"
            SOURCE_FILE@0..71
              CLASS_DECL@0..71
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..71
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..69
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..69
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      EXPR_STMT@20..28
                        CALL_EXPR@20..27
                          NAME_REF@20..22
                            WHITESPACE@20..21 " "
                            IDENT@21..22 "f"
                          ARG_LIST@22..27
                            LPAREN@22..23 "("
                            ERROR@23..26
                              INT_KW@23..26 "int"
                            RPAREN@26..27 ")"
                        SEMICOLON@27..28 ";"
                      EXPR_STMT@28..35
                        CALL_EXPR@28..35
                          NAME_REF@28..30
                            WHITESPACE@28..29 " "
                            IDENT@29..30 "g"
                          ARG_LIST@30..35
                            LPAREN@30..31 "("
                            CLASS_LITERAL@31..35
                              TYPE@31..34
                                INT_KW@31..34 "int"
                              DOT@34..35 "."
                      EXPR_STMT@35..38
                        NAME_REF@35..38
                          IDENT@35..38 "foo"
                      ERROR@38..39
                        RPAREN@38..39 ")"
                      EMPTY_STMT@39..40
                        SEMICOLON@39..40 ";"
                      EXPR_STMT@40..49
                        CALL_EXPR@40..49
                          NAME_REF@40..42
                            WHITESPACE@40..41 " "
                            IDENT@41..42 "h"
                          ARG_LIST@42..49
                            LPAREN@42..43 "("
                            CLASS_LITERAL@43..49
                              TYPE@43..48
                                INT_KW@43..46 "int"
                                LBRACK@46..47 "["
                                RBRACK@47..48 "]"
                              DOT@48..49 "."
                      EXPR_STMT@49..52
                        NAME_REF@49..52
                          IDENT@49..52 "foo"
                      ERROR@52..53
                        RPAREN@52..53 ")"
                      EMPTY_STMT@53..54
                        SEMICOLON@53..54 ";"
                      LOCAL_VAR_DECL@54..67
                        MODIFIERS@54..54
                        TYPE@54..58
                          WHITESPACE@54..55 " "
                          VAR_KW@55..58 "var"
                        WHITESPACE@58..59 " "
                        IDENT@59..60 "x"
                        WHITESPACE@60..61 " "
                        EQ@61..62 "="
                        INDEX_EXPR@62..66
                          NAME_REF@62..64
                            WHITESPACE@62..63 " "
                            IDENT@63..64 "a"
                          LBRACK@64..65 "["
                          ERROR@65..66
                            RBRACK@65..66 "]"
                        SEMICOLON@66..67 ";"
                      WHITESPACE@67..68 " "
                      RBRACE@68..69 "}"
                  WHITESPACE@69..70 " "
                  RBRACE@70..71 "}"
            error 23..23: expected an expression
            error 35..35: expected CLASS_KW
            error 35..35: expected RPAREN
            error 35..35: expected SEMICOLON
            error 38..38: expected SEMICOLON
            error 38..38: expected a statement
            error 49..49: expected CLASS_KW
            error 49..49: expected RPAREN
            error 49..49: expected SEMICOLON
            error 52..52: expected SEMICOLON
            error 52..52: expected a statement
            error 65..65: expected an expression
            error 66..66: expected RBRACK
        "#]],
    );
}

#[test]
fn explicit_type_witness() {
    check(
        "class C { void m() { List.<String>of(); obj.<Map<K, V>>build().run(); } }",
        expect![[r#"
            SOURCE_FILE@0..73
              CLASS_DECL@0..73
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..73
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..71
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..71
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      EXPR_STMT@20..39
                        CALL_EXPR@20..38
                          FIELD_ACCESS@20..36
                            NAME_REF@20..25
                              WHITESPACE@20..21 " "
                              IDENT@21..25 "List"
                            DOT@25..26 "."
                            TYPE_ARGS@26..34
                              LT@26..27 "<"
                              TYPE@27..33
                                IDENT@27..33 "String"
                              GT@33..34 ">"
                            IDENT@34..36 "of"
                          ARG_LIST@36..38
                            LPAREN@36..37 "("
                            RPAREN@37..38 ")"
                        SEMICOLON@38..39 ";"
                      EXPR_STMT@39..69
                        CALL_EXPR@39..68
                          FIELD_ACCESS@39..66
                            CALL_EXPR@39..62
                              FIELD_ACCESS@39..60
                                NAME_REF@39..43
                                  WHITESPACE@39..40 " "
                                  IDENT@40..43 "obj"
                                DOT@43..44 "."
                                TYPE_ARGS@44..55
                                  LT@44..45 "<"
                                  TYPE@45..54
                                    IDENT@45..48 "Map"
                                    TYPE_ARGS@48..54
                                      LT@48..49 "<"
                                      TYPE@49..50
                                        IDENT@49..50 "K"
                                      COMMA@50..51 ","
                                      TYPE@51..53
                                        WHITESPACE@51..52 " "
                                        IDENT@52..53 "V"
                                      GT@53..54 ">"
                                  GT@54..55 ">"
                                IDENT@55..60 "build"
                              ARG_LIST@60..62
                                LPAREN@60..61 "("
                                RPAREN@61..62 ")"
                            DOT@62..63 "."
                            IDENT@63..66 "run"
                          ARG_LIST@66..68
                            LPAREN@66..67 "("
                            RPAREN@67..68 ")"
                        SEMICOLON@68..69 ";"
                      WHITESPACE@69..70 " "
                      RBRACE@70..71 "}"
                  WHITESPACE@71..72 " "
                  RBRACE@72..73 "}"
        "#]],
    );
}

#[test]
fn ternary_and_assignment() {
    check(
        "class C { void m() { x = a ? b : c; y += 1; z >>= 2; } }",
        expect![[r#"
            SOURCE_FILE@0..56
              CLASS_DECL@0..56
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..56
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..54
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..54
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      EXPR_STMT@20..35
                        ASSIGNMENT_EXPR@20..34
                          NAME_REF@20..22
                            WHITESPACE@20..21 " "
                            IDENT@21..22 "x"
                          WHITESPACE@22..23 " "
                          EQ@23..24 "="
                          TERNARY_EXPR@24..34
                            NAME_REF@24..26
                              WHITESPACE@24..25 " "
                              IDENT@25..26 "a"
                            WHITESPACE@26..27 " "
                            QUESTION@27..28 "?"
                            NAME_REF@28..30
                              WHITESPACE@28..29 " "
                              IDENT@29..30 "b"
                            WHITESPACE@30..31 " "
                            COLON@31..32 ":"
                            NAME_REF@32..34
                              WHITESPACE@32..33 " "
                              IDENT@33..34 "c"
                        SEMICOLON@34..35 ";"
                      EXPR_STMT@35..43
                        ASSIGNMENT_EXPR@35..42
                          NAME_REF@35..37
                            WHITESPACE@35..36 " "
                            IDENT@36..37 "y"
                          WHITESPACE@37..38 " "
                          PLUS_EQ@38..40 "+="
                          LITERAL@40..42
                            WHITESPACE@40..41 " "
                            INT_LITERAL@41..42 "1"
                        SEMICOLON@42..43 ";"
                      EXPR_STMT@43..52
                        ASSIGNMENT_EXPR@43..51
                          NAME_REF@43..45
                            WHITESPACE@43..44 " "
                            IDENT@44..45 "z"
                          WHITESPACE@45..46 " "
                          GT@46..47 ">"
                          GT@47..48 ">"
                          EQ@48..49 "="
                          LITERAL@49..51
                            WHITESPACE@49..50 " "
                            INT_LITERAL@50..51 "2"
                        SEMICOLON@51..52 ";"
                      WHITESPACE@52..53 " "
                      RBRACE@53..54 "}"
                  WHITESPACE@54..55 " "
                  RBRACE@55..56 "}"
        "#]],
    );
}

#[test]
fn cast_primitive_and_reference() {
    check(
        "class C { void m() { int a = (int) d; Object o = (String) s; long e = (long) -f; } }",
        expect![[r#"
            SOURCE_FILE@0..84
              CLASS_DECL@0..84
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..84
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..82
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..82
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      LOCAL_VAR_DECL@20..37
                        MODIFIERS@20..20
                        TYPE@20..24
                          WHITESPACE@20..21 " "
                          INT_KW@21..24 "int"
                        WHITESPACE@24..25 " "
                        IDENT@25..26 "a"
                        WHITESPACE@26..27 " "
                        EQ@27..28 "="
                        CAST_EXPR@28..36
                          WHITESPACE@28..29 " "
                          LPAREN@29..30 "("
                          TYPE@30..33
                            INT_KW@30..33 "int"
                          RPAREN@33..34 ")"
                          NAME_REF@34..36
                            WHITESPACE@34..35 " "
                            IDENT@35..36 "d"
                        SEMICOLON@36..37 ";"
                      LOCAL_VAR_DECL@37..60
                        MODIFIERS@37..37
                        TYPE@37..44
                          WHITESPACE@37..38 " "
                          IDENT@38..44 "Object"
                        WHITESPACE@44..45 " "
                        IDENT@45..46 "o"
                        WHITESPACE@46..47 " "
                        EQ@47..48 "="
                        CAST_EXPR@48..59
                          WHITESPACE@48..49 " "
                          LPAREN@49..50 "("
                          TYPE@50..56
                            IDENT@50..56 "String"
                          RPAREN@56..57 ")"
                          NAME_REF@57..59
                            WHITESPACE@57..58 " "
                            IDENT@58..59 "s"
                        SEMICOLON@59..60 ";"
                      LOCAL_VAR_DECL@60..80
                        MODIFIERS@60..60
                        TYPE@60..65
                          WHITESPACE@60..61 " "
                          LONG_KW@61..65 "long"
                        WHITESPACE@65..66 " "
                        IDENT@66..67 "e"
                        WHITESPACE@67..68 " "
                        EQ@68..69 "="
                        CAST_EXPR@69..79
                          WHITESPACE@69..70 " "
                          LPAREN@70..71 "("
                          TYPE@71..75
                            LONG_KW@71..75 "long"
                          RPAREN@75..76 ")"
                          UNARY_EXPR@76..79
                            WHITESPACE@76..77 " "
                            MINUS@77..78 "-"
                            NAME_REF@78..79
                              IDENT@78..79 "f"
                        SEMICOLON@79..80 ";"
                      WHITESPACE@80..81 " "
                      RBRACE@81..82 "}"
                  WHITESPACE@82..83 " "
                  RBRACE@83..84 "}"
        "#]],
    );
}

#[test]
fn cast_to_lambda() {
    check(
        "class C { void m() { Runnable r = (Runnable) () -> {}; Comparator<String> c = (Comparator<String>) (a, b) -> a.compareTo(b); Object o = (Runnable & Serializable) () -> {}; Runnable s = (Runnable) x -> x; } }",
        expect![[r#"
            SOURCE_FILE@0..207
              CLASS_DECL@0..207
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..207
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..205
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..205
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      LOCAL_VAR_DECL@20..54
                        MODIFIERS@20..20
                        TYPE@20..29
                          WHITESPACE@20..21 " "
                          IDENT@21..29 "Runnable"
                        WHITESPACE@29..30 " "
                        IDENT@30..31 "r"
                        WHITESPACE@31..32 " "
                        EQ@32..33 "="
                        CAST_EXPR@33..53
                          WHITESPACE@33..34 " "
                          LPAREN@34..35 "("
                          TYPE@35..43
                            IDENT@35..43 "Runnable"
                          RPAREN@43..44 ")"
                          LAMBDA_EXPR@44..53
                            LAMBDA_PARAMS@44..47
                              WHITESPACE@44..45 " "
                              LPAREN@45..46 "("
                              RPAREN@46..47 ")"
                            WHITESPACE@47..48 " "
                            ARROW@48..50 "->"
                            BLOCK@50..53
                              WHITESPACE@50..51 " "
                              LBRACE@51..52 "{"
                              RBRACE@52..53 "}"
                        SEMICOLON@53..54 ";"
                      LOCAL_VAR_DECL@54..124
                        MODIFIERS@54..54
                        TYPE@54..73
                          WHITESPACE@54..55 " "
                          IDENT@55..65 "Comparator"
                          TYPE_ARGS@65..73
                            LT@65..66 "<"
                            TYPE@66..72
                              IDENT@66..72 "String"
                            GT@72..73 ">"
                        WHITESPACE@73..74 " "
                        IDENT@74..75 "c"
                        WHITESPACE@75..76 " "
                        EQ@76..77 "="
                        CAST_EXPR@77..123
                          WHITESPACE@77..78 " "
                          LPAREN@78..79 "("
                          TYPE@79..97
                            IDENT@79..89 "Comparator"
                            TYPE_ARGS@89..97
                              LT@89..90 "<"
                              TYPE@90..96
                                IDENT@90..96 "String"
                              GT@96..97 ">"
                          RPAREN@97..98 ")"
                          LAMBDA_EXPR@98..123
                            LAMBDA_PARAMS@98..105
                              WHITESPACE@98..99 " "
                              LPAREN@99..100 "("
                              PARAM@100..101
                                IDENT@100..101 "a"
                              COMMA@101..102 ","
                              PARAM@102..104
                                WHITESPACE@102..103 " "
                                IDENT@103..104 "b"
                              RPAREN@104..105 ")"
                            WHITESPACE@105..106 " "
                            ARROW@106..108 "->"
                            CALL_EXPR@108..123
                              FIELD_ACCESS@108..120
                                NAME_REF@108..110
                                  WHITESPACE@108..109 " "
                                  IDENT@109..110 "a"
                                DOT@110..111 "."
                                IDENT@111..120 "compareTo"
                              ARG_LIST@120..123
                                LPAREN@120..121 "("
                                NAME_REF@121..122
                                  IDENT@121..122 "b"
                                RPAREN@122..123 ")"
                        SEMICOLON@123..124 ";"
                      LOCAL_VAR_DECL@124..171
                        MODIFIERS@124..124
                        TYPE@124..131
                          WHITESPACE@124..125 " "
                          IDENT@125..131 "Object"
                        WHITESPACE@131..132 " "
                        IDENT@132..133 "o"
                        WHITESPACE@133..134 " "
                        EQ@134..135 "="
                        CAST_EXPR@135..170
                          WHITESPACE@135..136 " "
                          LPAREN@136..137 "("
                          TYPE@137..145
                            IDENT@137..145 "Runnable"
                          WHITESPACE@145..146 " "
                          AMP@146..147 "&"
                          TYPE@147..160
                            WHITESPACE@147..148 " "
                            IDENT@148..160 "Serializable"
                          RPAREN@160..161 ")"
                          LAMBDA_EXPR@161..170
                            LAMBDA_PARAMS@161..164
                              WHITESPACE@161..162 " "
                              LPAREN@162..163 "("
                              RPAREN@163..164 ")"
                            WHITESPACE@164..165 " "
                            ARROW@165..167 "->"
                            BLOCK@167..170
                              WHITESPACE@167..168 " "
                              LBRACE@168..169 "{"
                              RBRACE@169..170 "}"
                        SEMICOLON@170..171 ";"
                      LOCAL_VAR_DECL@171..203
                        MODIFIERS@171..171
                        TYPE@171..180
                          WHITESPACE@171..172 " "
                          IDENT@172..180 "Runnable"
                        WHITESPACE@180..181 " "
                        IDENT@181..182 "s"
                        WHITESPACE@182..183 " "
                        EQ@183..184 "="
                        CAST_EXPR@184..202
                          WHITESPACE@184..185 " "
                          LPAREN@185..186 "("
                          TYPE@186..194
                            IDENT@186..194 "Runnable"
                          RPAREN@194..195 ")"
                          LAMBDA_EXPR@195..202
                            LAMBDA_PARAMS@195..197
                              PARAM@195..197
                                WHITESPACE@195..196 " "
                                IDENT@196..197 "x"
                            WHITESPACE@197..198 " "
                            ARROW@198..200 "->"
                            NAME_REF@200..202
                              WHITESPACE@200..201 " "
                              IDENT@201..202 "x"
                        SEMICOLON@202..203 ";"
                      WHITESPACE@203..204 " "
                      RBRACE@204..205 "}"
                  WHITESPACE@205..206 " "
                  RBRACE@206..207 "}"
        "#]],
    );
}

#[test]
fn primitive_cast_rejects_lambda() {
    check(
        "class C { void m() { Object o = (int) () -> {}; } }",
        expect![[r#"
            SOURCE_FILE@0..51
              CLASS_DECL@0..51
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..51
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..49
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..49
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      LOCAL_VAR_DECL@20..40
                        MODIFIERS@20..20
                        TYPE@20..27
                          WHITESPACE@20..21 " "
                          IDENT@21..27 "Object"
                        WHITESPACE@27..28 " "
                        IDENT@28..29 "o"
                        WHITESPACE@29..30 " "
                        EQ@30..31 "="
                        CAST_EXPR@31..40
                          WHITESPACE@31..32 " "
                          LPAREN@32..33 "("
                          TYPE@33..36
                            INT_KW@33..36 "int"
                          RPAREN@36..37 ")"
                          PAREN_EXPR@37..40
                            WHITESPACE@37..38 " "
                            LPAREN@38..39 "("
                            ERROR@39..40
                              RPAREN@39..40 ")"
                      ERROR@40..43
                        WHITESPACE@40..41 " "
                        ARROW@41..43 "->"
                      BLOCK@43..46
                        WHITESPACE@43..44 " "
                        LBRACE@44..45 "{"
                        RBRACE@45..46 "}"
                      EMPTY_STMT@46..47
                        SEMICOLON@46..47 ";"
                      WHITESPACE@47..48 " "
                      RBRACE@48..49 "}"
                  WHITESPACE@49..50 " "
                  RBRACE@50..51 "}"
            error 39..39: expected an expression
            error 40..40: expected RPAREN
            error 40..40: expected SEMICOLON
            error 40..40: expected a statement
        "#]],
    );
}

#[test]
fn qualified_new_inner_class() {
    check(
        "class C { void m() { var a = outer.new Inner(); var b = x.new B().new C(); } }",
        expect![[r#"
            SOURCE_FILE@0..78
              CLASS_DECL@0..78
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..78
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..76
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..76
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      LOCAL_VAR_DECL@20..47
                        MODIFIERS@20..20
                        TYPE@20..24
                          WHITESPACE@20..21 " "
                          VAR_KW@21..24 "var"
                        WHITESPACE@24..25 " "
                        IDENT@25..26 "a"
                        WHITESPACE@26..27 " "
                        EQ@27..28 "="
                        NEW_EXPR@28..46
                          NAME_REF@28..34
                            WHITESPACE@28..29 " "
                            IDENT@29..34 "outer"
                          DOT@34..35 "."
                          NEW_KW@35..38 "new"
                          TYPE@38..44
                            WHITESPACE@38..39 " "
                            IDENT@39..44 "Inner"
                          ARG_LIST@44..46
                            LPAREN@44..45 "("
                            RPAREN@45..46 ")"
                        SEMICOLON@46..47 ";"
                      LOCAL_VAR_DECL@47..74
                        MODIFIERS@47..47
                        TYPE@47..51
                          WHITESPACE@47..48 " "
                          VAR_KW@48..51 "var"
                        WHITESPACE@51..52 " "
                        IDENT@52..53 "b"
                        WHITESPACE@53..54 " "
                        EQ@54..55 "="
                        NEW_EXPR@55..73
                          NEW_EXPR@55..65
                            NAME_REF@55..57
                              WHITESPACE@55..56 " "
                              IDENT@56..57 "x"
                            DOT@57..58 "."
                            NEW_KW@58..61 "new"
                            TYPE@61..63
                              WHITESPACE@61..62 " "
                              IDENT@62..63 "B"
                            ARG_LIST@63..65
                              LPAREN@63..64 "("
                              RPAREN@64..65 ")"
                          DOT@65..66 "."
                          NEW_KW@66..69 "new"
                          TYPE@69..71
                            WHITESPACE@69..70 " "
                            IDENT@70..71 "C"
                          ARG_LIST@71..73
                            LPAREN@71..72 "("
                            RPAREN@72..73 ")"
                        SEMICOLON@73..74 ";"
                      WHITESPACE@74..75 " "
                      RBRACE@75..76 "}"
                  WHITESPACE@76..77 " "
                  RBRACE@77..78 "}"
        "#]],
    );
}

#[test]
fn new_array_and_anonymous() {
    check(
        "class C { void m() { var a = new int[]{1, 2}; var b = new Runnable() { public void run() {} }; var c = new ArrayList<>(); } }",
        expect![[r#"
            SOURCE_FILE@0..125
              CLASS_DECL@0..125
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..125
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..123
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..123
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      LOCAL_VAR_DECL@20..45
                        MODIFIERS@20..20
                        TYPE@20..24
                          WHITESPACE@20..21 " "
                          VAR_KW@21..24 "var"
                        WHITESPACE@24..25 " "
                        IDENT@25..26 "a"
                        WHITESPACE@26..27 " "
                        EQ@27..28 "="
                        NEW_EXPR@28..44
                          WHITESPACE@28..29 " "
                          NEW_KW@29..32 "new"
                          TYPE@32..38
                            WHITESPACE@32..33 " "
                            INT_KW@33..36 "int"
                            LBRACK@36..37 "["
                            RBRACK@37..38 "]"
                          ARRAY_INIT@38..44
                            LBRACE@38..39 "{"
                            LITERAL@39..40
                              INT_LITERAL@39..40 "1"
                            COMMA@40..41 ","
                            LITERAL@41..43
                              WHITESPACE@41..42 " "
                              INT_LITERAL@42..43 "2"
                            RBRACE@43..44 "}"
                        SEMICOLON@44..45 ";"
                      LOCAL_VAR_DECL@45..94
                        MODIFIERS@45..45
                        TYPE@45..49
                          WHITESPACE@45..46 " "
                          VAR_KW@46..49 "var"
                        WHITESPACE@49..50 " "
                        IDENT@50..51 "b"
                        WHITESPACE@51..52 " "
                        EQ@52..53 "="
                        NEW_EXPR@53..93
                          WHITESPACE@53..54 " "
                          NEW_KW@54..57 "new"
                          TYPE@57..66
                            WHITESPACE@57..58 " "
                            IDENT@58..66 "Runnable"
                          ARG_LIST@66..68
                            LPAREN@66..67 "("
                            RPAREN@67..68 ")"
                          CLASS_BODY@68..93
                            WHITESPACE@68..69 " "
                            LBRACE@69..70 "{"
                            METHOD_DECL@70..91
                              MODIFIERS@70..77
                                WHITESPACE@70..71 " "
                                PUBLIC_KW@71..77 "public"
                              TYPE@77..82
                                WHITESPACE@77..78 " "
                                VOID_KW@78..82 "void"
                              WHITESPACE@82..83 " "
                              IDENT@83..86 "run"
                              PARAM_LIST@86..88
                                LPAREN@86..87 "("
                                RPAREN@87..88 ")"
                              BLOCK@88..91
                                WHITESPACE@88..89 " "
                                LBRACE@89..90 "{"
                                RBRACE@90..91 "}"
                            WHITESPACE@91..92 " "
                            RBRACE@92..93 "}"
                        SEMICOLON@93..94 ";"
                      LOCAL_VAR_DECL@94..121
                        MODIFIERS@94..94
                        TYPE@94..98
                          WHITESPACE@94..95 " "
                          VAR_KW@95..98 "var"
                        WHITESPACE@98..99 " "
                        IDENT@99..100 "c"
                        WHITESPACE@100..101 " "
                        EQ@101..102 "="
                        NEW_EXPR@102..120
                          WHITESPACE@102..103 " "
                          NEW_KW@103..106 "new"
                          TYPE@106..118
                            WHITESPACE@106..107 " "
                            IDENT@107..116 "ArrayList"
                            TYPE_ARGS@116..118
                              LT@116..117 "<"
                              GT@117..118 ">"
                          ARG_LIST@118..120
                            LPAREN@118..119 "("
                            RPAREN@119..120 ")"
                        SEMICOLON@120..121 ";"
                      WHITESPACE@121..122 " "
                      RBRACE@122..123 "}"
                  WHITESPACE@123..124 " "
                  RBRACE@124..125 "}"
        "#]],
    );
}

#[test]
fn annotated_array_dims() {
    // Standard JLS type annotations on array dimensions: `Type @A []`.
    check(
        "class C { Document @Readonly [] docs1; Document[] @Readonly [] docs2; }",
        expect![[r#"
            SOURCE_FILE@0..71
              CLASS_DECL@0..71
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..71
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  FIELD_DECL@9..38
                    MODIFIERS@9..9
                    TYPE@9..31
                      WHITESPACE@9..10 " "
                      IDENT@10..18 "Document"
                      ANNOTATION@18..28
                        WHITESPACE@18..19 " "
                        AT@19..20 "@"
                        QUALIFIED_NAME@20..28
                          IDENT@20..28 "Readonly"
                      WHITESPACE@28..29 " "
                      LBRACK@29..30 "["
                      RBRACK@30..31 "]"
                    WHITESPACE@31..32 " "
                    IDENT@32..37 "docs1"
                    SEMICOLON@37..38 ";"
                  FIELD_DECL@38..69
                    MODIFIERS@38..38
                    TYPE@38..62
                      WHITESPACE@38..39 " "
                      IDENT@39..47 "Document"
                      LBRACK@47..48 "["
                      RBRACK@48..49 "]"
                      ANNOTATION@49..59
                        WHITESPACE@49..50 " "
                        AT@50..51 "@"
                        QUALIFIED_NAME@51..59
                          IDENT@51..59 "Readonly"
                      WHITESPACE@59..60 " "
                      LBRACK@60..61 "["
                      RBRACK@61..62 "]"
                    WHITESPACE@62..63 " "
                    IDENT@63..68 "docs2"
                    SEMICOLON@68..69 ";"
                  WHITESPACE@69..70 " "
                  RBRACE@70..71 "}"
        "#]],
    );
}

#[test]
fn annotated_array_dims_variants() {
    // Multiple annotations on one dimension, annotation arguments, and an old-style
    // annotated return-type dimension (`m() @A []`).
    check(
        "class C { int @A @B [] f; String m() @Size(max = 3) [] { return null; } }",
        expect![[r#"
            SOURCE_FILE@0..73
              CLASS_DECL@0..73
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..73
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  FIELD_DECL@9..25
                    MODIFIERS@9..9
                    TYPE@9..22
                      WHITESPACE@9..10 " "
                      INT_KW@10..13 "int"
                      ANNOTATION@13..16
                        WHITESPACE@13..14 " "
                        AT@14..15 "@"
                        QUALIFIED_NAME@15..16
                          IDENT@15..16 "A"
                      ANNOTATION@16..19
                        WHITESPACE@16..17 " "
                        AT@17..18 "@"
                        QUALIFIED_NAME@18..19
                          IDENT@18..19 "B"
                      WHITESPACE@19..20 " "
                      LBRACK@20..21 "["
                      RBRACK@21..22 "]"
                    WHITESPACE@22..23 " "
                    IDENT@23..24 "f"
                    SEMICOLON@24..25 ";"
                  METHOD_DECL@25..71
                    MODIFIERS@25..25
                    TYPE@25..32
                      WHITESPACE@25..26 " "
                      IDENT@26..32 "String"
                    WHITESPACE@32..33 " "
                    IDENT@33..34 "m"
                    PARAM_LIST@34..36
                      LPAREN@34..35 "("
                      RPAREN@35..36 ")"
                    ANNOTATION@36..51
                      WHITESPACE@36..37 " "
                      AT@37..38 "@"
                      QUALIFIED_NAME@38..42
                        IDENT@38..42 "Size"
                      ANNOTATION_ARG_LIST@42..51
                        LPAREN@42..43 "("
                        ANNOTATION_PAIR@43..50
                          IDENT@43..46 "max"
                          WHITESPACE@46..47 " "
                          EQ@47..48 "="
                          LITERAL@48..50
                            WHITESPACE@48..49 " "
                            INT_LITERAL@49..50 "3"
                        RPAREN@50..51 ")"
                    WHITESPACE@51..52 " "
                    LBRACK@52..53 "["
                    RBRACK@53..54 "]"
                    BLOCK@54..71
                      WHITESPACE@54..55 " "
                      LBRACE@55..56 "{"
                      RETURN_STMT@56..69
                        WHITESPACE@56..57 " "
                        RETURN_KW@57..63 "return"
                        LITERAL@63..68
                          WHITESPACE@63..64 " "
                          NULL_KW@64..68 "null"
                        SEMICOLON@68..69 ";"
                      WHITESPACE@69..70 " "
                      RBRACE@70..71 "}"
                  WHITESPACE@71..72 " "
                  RBRACE@72..73 "}"
        "#]],
    );
}

#[test]
fn new_array_with_annotated_dim() {
    // Array creation with an annotated dimension expression: `new T[n] @A [m]`.
    check(
        "class C { void m() { var x = new Document[2] @Readonly [12]; } }",
        expect![[r#"
            SOURCE_FILE@0..64
              CLASS_DECL@0..64
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..64
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..62
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..62
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      LOCAL_VAR_DECL@20..60
                        MODIFIERS@20..20
                        TYPE@20..24
                          WHITESPACE@20..21 " "
                          VAR_KW@21..24 "var"
                        WHITESPACE@24..25 " "
                        IDENT@25..26 "x"
                        WHITESPACE@26..27 " "
                        EQ@27..28 "="
                        NEW_EXPR@28..59
                          WHITESPACE@28..29 " "
                          NEW_KW@29..32 "new"
                          TYPE@32..41
                            WHITESPACE@32..33 " "
                            IDENT@33..41 "Document"
                          LBRACK@41..42 "["
                          LITERAL@42..43
                            INT_LITERAL@42..43 "2"
                          RBRACK@43..44 "]"
                          ANNOTATION@44..54
                            WHITESPACE@44..45 " "
                            AT@45..46 "@"
                            QUALIFIED_NAME@46..54
                              IDENT@46..54 "Readonly"
                          WHITESPACE@54..55 " "
                          LBRACK@55..56 "["
                          LITERAL@56..58
                            INT_LITERAL@56..58 "12"
                          RBRACK@58..59 "]"
                        SEMICOLON@59..60 ";"
                      WHITESPACE@60..61 " "
                      RBRACE@61..62 "}"
                  WHITESPACE@62..63 " "
                  RBRACE@63..64 "}"
        "#]],
    );
}

#[test]
fn misc_statements() {
    check(
        "class C { void m() { outer: do { if (x) continue; break outer; } while (c); assert x : \"m\"; synchronized (lock) { } throw e; } }",
        expect![[r#"
            SOURCE_FILE@0..128
              CLASS_DECL@0..128
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..128
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..126
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..18
                      LPAREN@16..17 "("
                      RPAREN@17..18 ")"
                    BLOCK@18..126
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      LABELED_STMT@20..75
                        WHITESPACE@20..21 " "
                        IDENT@21..26 "outer"
                        COLON@26..27 ":"
                        DO_WHILE_STMT@27..75
                          WHITESPACE@27..28 " "
                          DO_KW@28..30 "do"
                          BLOCK@30..64
                            WHITESPACE@30..31 " "
                            LBRACE@31..32 "{"
                            IF_STMT@32..49
                              WHITESPACE@32..33 " "
                              IF_KW@33..35 "if"
                              WHITESPACE@35..36 " "
                              LPAREN@36..37 "("
                              NAME_REF@37..38
                                IDENT@37..38 "x"
                              RPAREN@38..39 ")"
                              CONTINUE_STMT@39..49
                                WHITESPACE@39..40 " "
                                CONTINUE_KW@40..48 "continue"
                                SEMICOLON@48..49 ";"
                            BREAK_STMT@49..62
                              WHITESPACE@49..50 " "
                              BREAK_KW@50..55 "break"
                              WHITESPACE@55..56 " "
                              IDENT@56..61 "outer"
                              SEMICOLON@61..62 ";"
                            WHITESPACE@62..63 " "
                            RBRACE@63..64 "}"
                          WHITESPACE@64..65 " "
                          WHILE_KW@65..70 "while"
                          WHITESPACE@70..71 " "
                          LPAREN@71..72 "("
                          NAME_REF@72..73
                            IDENT@72..73 "c"
                          RPAREN@73..74 ")"
                          SEMICOLON@74..75 ";"
                      ASSERT_STMT@75..91
                        WHITESPACE@75..76 " "
                        ASSERT_KW@76..82 "assert"
                        NAME_REF@82..84
                          WHITESPACE@82..83 " "
                          IDENT@83..84 "x"
                        WHITESPACE@84..85 " "
                        COLON@85..86 ":"
                        LITERAL@86..90
                          WHITESPACE@86..87 " "
                          STRING_LITERAL@87..90 "\"m\""
                        SEMICOLON@90..91 ";"
                      SYNCHRONIZED_STMT@91..115
                        WHITESPACE@91..92 " "
                        SYNCHRONIZED_KW@92..104 "synchronized"
                        WHITESPACE@104..105 " "
                        LPAREN@105..106 "("
                        NAME_REF@106..110
                          IDENT@106..110 "lock"
                        RPAREN@110..111 ")"
                        BLOCK@111..115
                          WHITESPACE@111..112 " "
                          LBRACE@112..113 "{"
                          WHITESPACE@113..114 " "
                          RBRACE@114..115 "}"
                      THROW_STMT@115..124
                        WHITESPACE@115..116 " "
                        THROW_KW@116..121 "throw"
                        NAME_REF@121..123
                          WHITESPACE@121..122 " "
                          IDENT@122..123 "e"
                        SEMICOLON@123..124 ";"
                      WHITESPACE@124..125 " "
                      RBRACE@125..126 "}"
                  WHITESPACE@126..127 " "
                  RBRACE@127..128 "}"
        "#]],
    );
}

#[test]
fn error_recovery_in_switch_and_record() {
    // Returns a tree even for a broken switch (missing arrow body) and an unclosed record header.
    check(
        "class C { void m(Object o) { switch (o) { case 1 -> ; default } record R(int x }",
        expect![[r#"
            SOURCE_FILE@0..80
              CLASS_DECL@0..80
                MODIFIERS@0..0
                CLASS_KW@0..5 "class"
                WHITESPACE@5..6 " "
                IDENT@6..7 "C"
                CLASS_BODY@7..80
                  WHITESPACE@7..8 " "
                  LBRACE@8..9 "{"
                  METHOD_DECL@9..80
                    MODIFIERS@9..9
                    TYPE@9..14
                      WHITESPACE@9..10 " "
                      VOID_KW@10..14 "void"
                    WHITESPACE@14..15 " "
                    IDENT@15..16 "m"
                    PARAM_LIST@16..26
                      LPAREN@16..17 "("
                      PARAM@17..25
                        MODIFIERS@17..17
                        TYPE@17..23
                          IDENT@17..23 "Object"
                        WHITESPACE@23..24 " "
                        IDENT@24..25 "o"
                      RPAREN@25..26 ")"
                    BLOCK@26..80
                      WHITESPACE@26..27 " "
                      LBRACE@27..28 "{"
                      SWITCH_STMT@28..63
                        WHITESPACE@28..29 " "
                        SWITCH_KW@29..35 "switch"
                        WHITESPACE@35..36 " "
                        LPAREN@36..37 "("
                        NAME_REF@37..38
                          IDENT@37..38 "o"
                        RPAREN@38..39 ")"
                        SWITCH_BLOCK@39..63
                          WHITESPACE@39..40 " "
                          LBRACE@40..41 "{"
                          SWITCH_RULE@41..53
                            SWITCH_LABEL@41..48
                              WHITESPACE@41..42 " "
                              CASE_KW@42..46 "case"
                              LITERAL@46..48
                                WHITESPACE@46..47 " "
                                INT_LITERAL@47..48 "1"
                            WHITESPACE@48..49 " "
                            ARROW@49..51 "->"
                            ERROR@51..53
                              WHITESPACE@51..52 " "
                              SEMICOLON@52..53 ";"
                          SWITCH_GROUP@53..61
                            SWITCH_LABEL@53..61
                              WHITESPACE@53..54 " "
                              DEFAULT_KW@54..61 "default"
                          WHITESPACE@61..62 " "
                          RBRACE@62..63 "}"
                      RECORD_DECL@63..78
                        MODIFIERS@63..63
                        WHITESPACE@63..64 " "
                        RECORD_KW@64..70 "record"
                        WHITESPACE@70..71 " "
                        IDENT@71..72 "R"
                        RECORD_HEADER@72..78
                          LPAREN@72..73 "("
                          RECORD_COMPONENT@73..78
                            MODIFIERS@73..73
                            TYPE@73..76
                              INT_KW@73..76 "int"
                            WHITESPACE@76..77 " "
                            IDENT@77..78 "x"
                        CLASS_BODY@78..78
                      WHITESPACE@78..79 " "
                      RBRACE@79..80 "}"
            error 51..51: expected an expression
            error 53..53: expected SEMICOLON
            error 61..61: expected COLON
            error 78..78: expected RPAREN
            error 78..78: expected LBRACE
            error 80..80: expected RBRACE
        "#]],
    );
}

#[test]
fn lossless_on_various_inputs() {
    for src in [
        "",
        "   ",
        "\n\n",
        "// only a comment",
        "package a.b.c;",
        "int x = 0xFF; /* c */ var y = \"\"\"\nt\n\"\"\";",
        "@#$%^",
        "class\u{00A0}name { }",
        "class C{int a=b>>>c;var d=e<<2;}",
        "class C { void m() throws E, F { x[0].y(1, 2); } }",
        "class C { C() { this.x = new int[3]; } }",
    ] {
        assert_lossless(src);
    }
}
