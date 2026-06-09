//! Builds LSP semantic tokens from the lossless CST.
//!
//! Every significant token is classified purely from the syntax tree: the token's own
//! [`SyntaxKind`] plus, for identifiers, the kind of its parent (and sometimes grandparent)
//! node. There is no name resolution, so classification is best-effort — e.g. a bare name
//! reference is always `variable`, since without types we cannot tell a field from a local.
//! The lossless tree still pins down most tokens (declarations, type positions, call
//! callees, annotations) unambiguously.

use async_lsp::lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensLegend,
};
use jals_syntax::{SyntaxKind, SyntaxToken};

use crate::line_index::LineIndex;

/// Token-type indices into the legend's `token_types`. Kept in sync with [`legend`] by the
/// `legend_indices_match` test.
mod ty {
    pub(super) const NAMESPACE: u32 = 0;
    pub(super) const TYPE: u32 = 1;
    pub(super) const CLASS: u32 = 2;
    pub(super) const ENUM: u32 = 3;
    pub(super) const INTERFACE: u32 = 4;
    pub(super) const TYPE_PARAMETER: u32 = 5;
    pub(super) const PARAMETER: u32 = 6;
    pub(super) const VARIABLE: u32 = 7;
    pub(super) const PROPERTY: u32 = 8;
    pub(super) const ENUM_MEMBER: u32 = 9;
    pub(super) const METHOD: u32 = 10;
    pub(super) const KEYWORD: u32 = 11;
    pub(super) const COMMENT: u32 = 12;
    pub(super) const STRING: u32 = 13;
    pub(super) const NUMBER: u32 = 14;
    pub(super) const DECORATOR: u32 = 15;
}

/// The `declaration` modifier (bit 0, the only entry in the legend's `token_modifiers`).
const MOD_DECLARATION: u32 = 1 << 0;

/// The legend advertised on `initialize`. The order of `token_types` defines the indices in
/// [`ty`]; the order of `token_modifiers` defines the [`MOD_DECLARATION`] bit.
pub(crate) fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::NAMESPACE,
            SemanticTokenType::TYPE,
            SemanticTokenType::CLASS,
            SemanticTokenType::ENUM,
            SemanticTokenType::INTERFACE,
            SemanticTokenType::TYPE_PARAMETER,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PROPERTY,
            SemanticTokenType::ENUM_MEMBER,
            SemanticTokenType::METHOD,
            SemanticTokenType::KEYWORD,
            SemanticTokenType::COMMENT,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::DECORATOR,
        ],
        token_modifiers: vec![SemanticTokenModifier::DECLARATION],
    }
}

/// Classify every significant token in `text` and emit LSP semantic tokens (delta-encoded,
/// one per line — multi-line tokens are split, as the protocol requires).
pub(crate) fn semantic_tokens(text: &str, line_index: &LineIndex) -> SemanticTokens {
    let root = jals_syntax::parse(text).syntax();
    let mut data: Vec<SemanticToken> = Vec::new();
    // Anchor for delta encoding: the line/start of the previously emitted token.
    let (mut prev_line, mut prev_start) = (0u32, 0u32);

    for token in root
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
    {
        let Some((token_type, token_modifiers_bitset)) = classify(&token) else {
            continue;
        };
        let start = line_index.position(text, token.text_range().start());
        // A semantic token may not span lines (LSP spec), so split on '\n'. Trivia tokens
        // are dropped, but comments are kept and a block comment / text block can be
        // multi-line; each line becomes its own token starting at column 0 (after line 0).
        for (line, (i, segment)) in (start.line..).zip(token.text().split('\n').enumerate()) {
            let segment = segment.strip_suffix('\r').unwrap_or(segment);
            let length: u32 = segment.chars().map(|c| c.len_utf16() as u32).sum();
            let char_start = if i == 0 { start.character } else { 0 };
            if length != 0 {
                let delta_line = line.saturating_sub(prev_line);
                let delta_start = if delta_line == 0 {
                    // Same line as the previous token; never underflows for in-order,
                    // non-overlapping tokens, but saturate defensively to never panic.
                    char_start.saturating_sub(prev_start)
                } else {
                    char_start
                };
                data.push(SemanticToken {
                    delta_line,
                    delta_start,
                    length,
                    token_type,
                    token_modifiers_bitset,
                });
                (prev_line, prev_start) = (line, char_start);
            }
        }
    }

    SemanticTokens {
        result_id: None,
        data,
    }
}

/// Classify a single token into a `(token_type, modifier_bits)` pair, or `None` to skip it
/// (whitespace/newlines, operators, delimiters, and unclassifiable identifiers).
fn classify(token: &SyntaxToken) -> Option<(u32, u32)> {
    use SyntaxKind::*;
    match token.kind() {
        WHITESPACE | NEWLINE => None,
        LINE_COMMENT | BLOCK_COMMENT | DOC_COMMENT => Some((ty::COMMENT, 0)),
        STRING_LITERAL | TEXT_BLOCK | CHAR_LITERAL => Some((ty::STRING, 0)),
        INT_LITERAL | FLOAT_LITERAL => Some((ty::NUMBER, 0)),
        IDENT | UNDERSCORE => classify_ident(token),
        k if is_keyword(k) => Some((ty::KEYWORD, 0)),
        _ => None,
    }
}

/// Classify an identifier from the kind of its parent node, falling back to grandparent
/// context to distinguish a method call from a plain name/field access.
fn classify_ident(token: &SyntaxToken) -> Option<(u32, u32)> {
    use SyntaxKind::*;
    let parent = token.parent()?;
    let grandparent = || parent.parent().map(|n| n.kind());
    let classified = match parent.kind() {
        // Declaration sites: the identifier names the entity being declared.
        CLASS_DECL | RECORD_DECL => (ty::CLASS, MOD_DECLARATION),
        INTERFACE_DECL | ANNOTATION_TYPE_DECL => (ty::INTERFACE, MOD_DECLARATION),
        ENUM_DECL => (ty::ENUM, MOD_DECLARATION),
        METHOD_DECL | CONSTRUCTOR_DECL => (ty::METHOD, MOD_DECLARATION),
        TYPE_PARAM => (ty::TYPE_PARAMETER, MOD_DECLARATION),
        PARAM | RECORD_COMPONENT => (ty::PARAMETER, MOD_DECLARATION),
        ENUM_CONSTANT => (ty::ENUM_MEMBER, MOD_DECLARATION),
        FIELD_DECL => (ty::PROPERTY, MOD_DECLARATION),
        // Binding sites that introduce a local: `T x`, `var x = ..`, `catch (E e)`,
        // `o instanceof T p`, `case T p ->`.
        LOCAL_VAR_DECL | RESOURCE | CATCH_CLAUSE | TYPE_PATTERN => (ty::VARIABLE, MOD_DECLARATION),
        // Reference sites.
        TYPE => (ty::TYPE, 0),
        METHOD_REF_EXPR => (ty::METHOD, 0),
        ANNOTATION_PAIR => (ty::PROPERTY, 0),
        // `non-sealed` is re-joined into a node wrapping `non` `-` `sealed`.
        NON_SEALED_KW => (ty::KEYWORD, 0),
        // A name / field is a method when it is the callee of a call expression.
        NAME_REF if grandparent() == Some(CALL_EXPR) => (ty::METHOD, 0),
        NAME_REF => (ty::VARIABLE, 0),
        FIELD_ACCESS if grandparent() == Some(CALL_EXPR) => (ty::METHOD, 0),
        FIELD_ACCESS => (ty::PROPERTY, 0),
        // A dotted name: the annotation type after `@`, otherwise a package/module name.
        QUALIFIED_NAME if grandparent() == Some(ANNOTATION) => (ty::DECORATOR, 0),
        QUALIFIED_NAME => (ty::NAMESPACE, 0),
        _ => return None,
    };
    Some(classified)
}

/// Whether `kind` is a keyword token — the reserved 50, the literal keywords
/// (`true`/`false`/`null`), and the contextual keywords the parser promotes from `IDENT`.
fn is_keyword(kind: SyntaxKind) -> bool {
    use SyntaxKind::*;
    matches!(
        kind,
        ABSTRACT_KW
            | ASSERT_KW
            | BOOLEAN_KW
            | BREAK_KW
            | BYTE_KW
            | CASE_KW
            | CATCH_KW
            | CHAR_KW
            | CLASS_KW
            | CONST_KW
            | CONTINUE_KW
            | DEFAULT_KW
            | DO_KW
            | DOUBLE_KW
            | ELSE_KW
            | ENUM_KW
            | EXTENDS_KW
            | FINAL_KW
            | FINALLY_KW
            | FLOAT_KW
            | FOR_KW
            | GOTO_KW
            | IF_KW
            | IMPLEMENTS_KW
            | IMPORT_KW
            | INSTANCEOF_KW
            | INT_KW
            | INTERFACE_KW
            | LONG_KW
            | NATIVE_KW
            | NEW_KW
            | PACKAGE_KW
            | PRIVATE_KW
            | PROTECTED_KW
            | PUBLIC_KW
            | RETURN_KW
            | SHORT_KW
            | STATIC_KW
            | STRICTFP_KW
            | SUPER_KW
            | SWITCH_KW
            | SYNCHRONIZED_KW
            | THIS_KW
            | THROW_KW
            | THROWS_KW
            | TRANSIENT_KW
            | TRY_KW
            | VOID_KW
            | VOLATILE_KW
            | WHILE_KW
            | TRUE_KW
            | FALSE_KW
            | NULL_KW
            | VAR_KW
            | YIELD_KW
            | RECORD_KW
            | SEALED_KW
            | PERMITS_KW
            | WHEN_KW
            | MODULE_KW
            | OPEN_KW
            | OPENS_KW
            | REQUIRES_KW
            | TRANSITIVE_KW
            | EXPORTS_KW
            | TO_KW
            | PROVIDES_KW
            | USES_KW
            | WITH_KW
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One decoded token: absolute `(line, start, length, type_name, modifier_bits)`.
    #[derive(Debug, PartialEq, Eq)]
    struct Tok {
        line: u32,
        start: u32,
        len: u32,
        ty: String,
        mods: u32,
    }

    /// Decode the delta-encoded tokens back to absolute positions and type names, so tests
    /// can assert on what a client would actually render.
    fn decode(text: &str) -> Vec<Tok> {
        let toks = semantic_tokens(text, &LineIndex::new(text));
        let legend = legend();
        let (mut line, mut start) = (0u32, 0u32);
        let mut out = Vec::new();
        for t in toks.data {
            if t.delta_line == 0 {
                start += t.delta_start;
            } else {
                line += t.delta_line;
                start = t.delta_start;
            }
            out.push(Tok {
                line,
                start,
                len: t.length,
                ty: legend.token_types[t.token_type as usize]
                    .as_str()
                    .to_string(),
                mods: t.token_modifiers_bitset,
            });
        }
        out
    }

    /// Find the single token covering `needle`'s first occurrence (by column on line 0).
    fn at(text: &str, needle: &str) -> Tok {
        let col = text.find(needle).expect("needle present") as u32;
        decode(text)
            .into_iter()
            .find(|t| t.line == 0 && t.start == col)
            .unwrap_or_else(|| panic!("no token at column {col} for {needle:?}"))
    }

    #[test]
    fn legend_indices_match() {
        let legend = legend();
        assert_eq!(legend.token_types.len(), 16);
        assert_eq!(legend.token_modifiers.len(), 1);
        // Spot-check that the index constants line up with the legend order.
        assert_eq!(legend.token_types[ty::CLASS as usize].as_str(), "class");
        assert_eq!(legend.token_types[ty::METHOD as usize].as_str(), "method");
        assert_eq!(
            legend.token_types[ty::DECORATOR as usize].as_str(),
            "decorator"
        );
        assert_eq!(
            legend.token_modifiers[0],
            SemanticTokenModifier::DECLARATION
        );
    }

    #[test]
    fn declarations_are_classified() {
        let src = "class C<T> { int field; void m(int p) {} }";
        assert_eq!(at(src, "C").ty, "class");
        assert_eq!(at(src, "C").mods, MOD_DECLARATION);
        assert_eq!(at(src, "T").ty, "typeParameter");
        assert_eq!(at(src, "field").ty, "property");
        assert_eq!(at(src, "m").ty, "method");
        assert_eq!(at(src, "p").ty, "parameter");
    }

    #[test]
    fn type_and_keyword_positions() {
        let src = "class C { List x; }";
        assert_eq!(at(src, "class").ty, "keyword");
        assert_eq!(at(src, "List").ty, "type");
        // `int` is a primitive keyword, not a `type`.
        assert_eq!(at("class C { int x; }", "int").ty, "keyword");
    }

    #[test]
    fn calls_vs_references_and_fields() {
        let src = "class C { void m() { foo(); obj.bar(); var x = obj.field; } }";
        assert_eq!(at(src, "foo").ty, "method");
        assert_eq!(at(src, "obj").ty, "variable");
        assert_eq!(at(src, "bar").ty, "method");
        assert_eq!(at(src, "field").ty, "property");
    }

    #[test]
    fn annotations_and_enum_members() {
        let ann = "@Override class D {}";
        assert_eq!(at(ann, "Override").ty, "decorator");
        let en = "enum E { A, B }";
        assert_eq!(at(en, "A").ty, "enumMember");
        assert_eq!(at(en, "B").ty, "enumMember");
    }

    #[test]
    fn imports_are_namespaces() {
        let src = "import java.util.List;";
        assert_eq!(at(src, "java").ty, "namespace");
        assert_eq!(at(src, "util").ty, "namespace");
    }

    #[test]
    fn literals_and_strings() {
        let src = "class C { int a = 42; String s = \"hi\"; char c = 'x'; }";
        assert_eq!(at(src, "42").ty, "number");
        assert_eq!(at(src, "\"hi\"").ty, "string");
        assert_eq!(at(src, "'x'").ty, "string");
    }

    #[test]
    fn block_comment_splits_per_line() {
        let src = "/* a\n   b */ class C {}";
        let toks = decode(src);
        // Two comment tokens, one per physical line, both before `class` on line 1.
        let comments: Vec<&Tok> = toks.iter().filter(|t| t.ty == "comment").collect();
        assert_eq!(comments.len(), 2);
        assert_eq!((comments[0].line, comments[0].start), (0, 0));
        assert_eq!(comments[1].line, 1);
    }

    #[test]
    fn text_block_splits_per_line_as_string() {
        let src = "class C { String s = \"\"\"\n  hi\n  \"\"\"; }";
        let strings: Vec<Tok> = decode(src)
            .into_iter()
            .filter(|t| t.ty == "string")
            .collect();
        // The text block spans three lines; each contributes a string token.
        assert_eq!(strings.len(), 3);
        assert!(strings.iter().all(|t| t.len > 0));
    }

    #[test]
    fn utf16_columns_are_not_byte_offsets() {
        // '😀' is 4 UTF-8 bytes but 2 UTF-16 code units; both token lengths and the columns
        // of everything after it must be counted in UTF-16, not bytes.
        let src = "class C { String s = \"😀\"; int x; }";
        let toks = decode(src);
        // "😀" = quote + astral(2 units) + quote = 4 UTF-16 units (would be 6 in bytes).
        let lit = toks.iter().find(|t| t.ty == "string").unwrap();
        assert_eq!(lit.len, 4);
        // Fields `s` and `x` are properties; `x` sits at UTF-16 column 31, not byte 33.
        let props: Vec<&Tok> = toks.iter().filter(|t| t.ty == "property").collect();
        assert_eq!(props.len(), 2);
        assert_eq!((props[1].line, props[1].start), (0, 31));
    }

    #[test]
    fn does_not_panic_on_garbage_or_empty() {
        // Invariant: handlers never panic, even on broken / arbitrary input.
        let _ = semantic_tokens("", &LineIndex::new(""));
        let _ = semantic_tokens("class", &LineIndex::new("class"));
        let _ = semantic_tokens("@#$%^ <<< class {", &LineIndex::new("@#$%^ <<< class {"));
        let weird = "класс 类 😀 \0 /*";
        let _ = semantic_tokens(weird, &LineIndex::new(weird));
    }
}
