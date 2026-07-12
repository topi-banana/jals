//! Property tests: the parser never panics and stays lossless on arbitrary input.

use super::*;
use proptest::prelude::*;

proptest! {
    /// Never panics on any UTF-8 string, and the syntax tree's text equals the input (lossless).
    #[test]
    fn parse_is_lossless_and_never_panics(src in any::<String>()) {
        let parse = Parse::parse(&src);
        prop_assert_eq!(parse.syntax().text().to_string(), src);
    }

    /// Input made of Java-like tokens is also lossless and never panics.
    /// (Mixing ASCII symbols with identifiers and keywords, exercises the new grammar paths
    /// broadly — switch / try / lambda / method references / ternary / patterns / sealed etc.)
    #[test]
    fn parse_is_lossless_on_javaish(
        src in proptest::collection::vec(
            prop_oneof![
                Just("class"), Just("interface"), Just("enum"), Just("record"),
                Just("void"), Just("int"), Just("return"), Just("if"), Just("else"),
                Just("var"), Just("new"), Just("instanceof"), Just("non-sealed"),
                Just("sealed"), Just("permits"), Just("extends"), Just("implements"),
                Just("switch"), Just("case"), Just("default"), Just("when"), Just("yield"),
                Just("try"), Just("catch"), Just("finally"), Just("throw"), Just("for"),
                Just("this"), Just("super"), Just("true"), Just("null"),
                Just("x"), Just("Foo"), Just("0"), Just("\"s\""), Just("@Ann"),
                Just("{"), Just("}"), Just("("), Just(")"), Just("["), Just("]"),
                Just("<"), Just(">"), Just(";"), Just(","), Just("."), Just(":"),
                Just("="), Just("+"), Just("-"), Just("*"), Just("&"), Just("|"),
                Just("?"), Just("->"), Just("::"), Just(">>"), Just(">>="), Just("+="),
                Just(" "), Just("\n"),
            ],
            0..48,
        ).prop_map(|parts| parts.concat())
    ) {
        let parse = Parse::parse(&src);
        prop_assert_eq!(parse.syntax().text().to_string(), src);
    }
}
