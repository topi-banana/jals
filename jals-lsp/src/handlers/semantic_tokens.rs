//! Builds LSP semantic tokens from the lossless CST, refined by name resolution.
//!
//! Identifiers are classified first from `jals-hir`'s file-local resolution ([`jals_hir::Resolved::resolve_node`]): a
//! resolved reference takes the kind of the binding it names (a field is `property`, a parameter
//! `parameter`, a sibling type `class`/`enum`/...), and a declaring name takes its own kind plus the
//! `declaration` modifier. A type name the file-local pass could not place — an imported or
//! same-package sibling declared in another file — is resolved against the project index when one is
//! supplied, so it too is classified by its declared kind rather than the generic `type`. Everything
//! still unplaced — an external (JDK) type, an inherited member, a member-access right-hand name, a
//! qualified/annotation name — falls back to a purely syntactic classification from the token's
//! [`SyntaxKind`] and its parent (sometimes grandparent) node. The fallback is what classified
//! *every* identifier before resolution was wired in, so it never regresses; resolution only sharpens
//! what it can place.

use std::collections::HashMap;

use async_lsp::lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensEdit,
    SemanticTokensLegend,
};
use jals_hir::{DefKind, FileId, Namespace, ProjectIndex, TypeResolution};
use jals_syntax::{Parse, SyntaxElement, SyntaxKind, SyntaxToken};

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

/// Semantic tokens (`textDocument/semanticTokens`).
pub(crate) struct SemanticTokensBuilder;

impl SemanticTokensBuilder {
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
    pub(crate) fn semantic_tokens(
        parse: &Parse,
        text: &str,
        line_index: &LineIndex,
        project: Option<(&ProjectIndex, FileId)>,
    ) -> SemanticTokens {
        let root = parse.syntax();
        let by_start = Self::resolution_classes(&root, project);
        let mut data: Vec<SemanticToken> = Vec::new();
        // Anchor for delta encoding: the line/start of the previously emitted token.
        let (mut prev_line, mut prev_start) = (0u32, 0u32);

        for token in root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            let Some((token_type, token_modifiers_bitset)) = Self::classify(&token, &by_start)
            else {
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

    /// The LSP semantic-tokens *delta* (edit script) turning `prev` into `next`, as a single splice of
    /// the one differing middle range. `prev` and `next` are the delta-encoded token arrays of two
    /// consecutive [`semantic_tokens`] results for the same document (the client's last copy and the
    /// current one).
    ///
    /// A `SemanticTokensEdit`'s `start` / `delete_count` count entries of the *flattened* integer array
    /// the tokens encode to — 5 ints per token — so the token indices are multiplied by 5. Returns an
    /// empty vector when the arrays are identical (no edits to apply).
    pub(crate) fn tokens_delta(
        prev: &[SemanticToken],
        next: &[SemanticToken],
    ) -> Vec<SemanticTokensEdit> {
        let max = prev.len().min(next.len());
        // The longest common prefix, then the longest common suffix that does not overlap it.
        let mut prefix = 0;
        while prefix < max && prev[prefix] == next[prefix] {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < max - prefix
            && prev[prev.len() - 1 - suffix] == next[next.len() - 1 - suffix]
        {
            suffix += 1;
        }
        let deleted = prev.len() - prefix - suffix;
        let inserted = &next[prefix..next.len() - suffix];
        if deleted == 0 && inserted.is_empty() {
            return Vec::new();
        }
        vec![SemanticTokensEdit {
            start: (prefix * 5) as u32,
            delete_count: (deleted * 5) as u32,
            data: Some(inserted.to_vec()),
        }]
    }

    /// A map from a token's start byte offset to its resolution-derived `(token_type, modifier_bits)`,
    /// built once per request from `jals-hir`'s file-local name resolution.
    ///
    /// Equivalent to calling [`jals_hir::Resolved::symbol_at`] for every identifier, but precomputed in
    /// one pass (O(references + defs)) so each token is an O(1) lookup instead of a linear scan. A
    /// resolved reference maps to the kind of the binding it names (no `declaration` modifier); a
    /// declaring name maps to its own kind plus [`MOD_DECLARATION`]. References are inserted first and
    /// declarations only fill gaps (`symbol_at` treats a covering reference as authoritative), though in
    /// practice a reference and a declaring name never share a start offset. A reference the file-local
    /// pass left unresolved is placed only when `project` binds it to a cross-file type (by its declared
    /// kind); any other unresolved reference is omitted, leaving its token to the syntactic fallback.
    fn resolution_classes(
        root: &jals_syntax::SyntaxNode,
        project: Option<(&ProjectIndex, FileId)>,
    ) -> HashMap<usize, (u32, u32)> {
        let resolved = jals_hir::Resolved::resolve_node(root);
        let mut by_start: HashMap<usize, (u32, u32)> = HashMap::new();
        for reference in &resolved.references {
            if let Some(id) = reference.resolution.def_id() {
                by_start.insert(
                    reference.range.start,
                    (Self::token_type_for(resolved.def(id).kind), 0),
                );
            } else if let Some((index, file)) = project
                && reference.namespace == Namespace::Type
                && let TypeResolution::Project(item) = index.resolve_reference(file, reference)
            {
                // A cross-file type the file-local pass could not place: classify by the indexed
                // declaration's kind, sharper than the syntactic fallback's generic `type`.
                by_start.insert(
                    reference.range.start,
                    (Self::token_type_for(index.item(item).kind), 0),
                );
            }
        }
        for def in &resolved.defs {
            by_start
                .entry(def.name_range.start)
                .or_insert_with(|| (Self::token_type_for(def.kind), MOD_DECLARATION));
        }
        by_start
    }

    /// The legend token-type index for a resolved binding's [`DefKind`]. Mirrors the declaration-site
    /// mapping in [`Self::classify_ident_syntactic`], so a declaration classifies the same whether it is
    /// placed by resolution or syntax.
    const fn token_type_for(kind: DefKind) -> u32 {
        match kind {
            DefKind::Class | DefKind::Record => ty::CLASS,
            DefKind::Interface | DefKind::AnnotationType => ty::INTERFACE,
            DefKind::Enum => ty::ENUM,
            DefKind::TypeParam => ty::TYPE_PARAMETER,
            DefKind::Param | DefKind::LambdaParam => ty::PARAMETER,
            DefKind::Field => ty::PROPERTY,
            DefKind::EnumConstant => ty::ENUM_MEMBER,
            DefKind::Method | DefKind::Constructor => ty::METHOD,
            DefKind::Local | DefKind::CatchParam | DefKind::Resource | DefKind::PatternVar => {
                ty::VARIABLE
            }
        }
    }

    /// Classify a single token into a `(token_type, modifier_bits)` pair, or `None` to skip it
    /// (whitespace/newlines, operators, delimiters, and unclassifiable identifiers).
    ///
    /// An identifier is taken from `by_start` (name resolution) when present, otherwise classified
    /// syntactically.
    fn classify(token: &SyntaxToken, by_start: &HashMap<usize, (u32, u32)>) -> Option<(u32, u32)> {
        use SyntaxKind::{
            BLOCK_COMMENT, CHAR_LITERAL, DOC_COMMENT, FLOAT_LITERAL, IDENT, INT_LITERAL,
            LINE_COMMENT, NEWLINE, STRING_LITERAL, TEXT_BLOCK, UNDERSCORE, WHITESPACE,
        };
        match token.kind() {
            WHITESPACE | NEWLINE => None,
            LINE_COMMENT | BLOCK_COMMENT | DOC_COMMENT => Some((ty::COMMENT, 0)),
            STRING_LITERAL | TEXT_BLOCK | CHAR_LITERAL => Some((ty::STRING, 0)),
            INT_LITERAL | FLOAT_LITERAL => Some((ty::NUMBER, 0)),
            IDENT | UNDERSCORE => {
                let start = usize::from(token.text_range().start());
                by_start
                    .get(&start)
                    .copied()
                    .or_else(|| Self::classify_ident_syntactic(token))
            }
            k if Self::is_keyword(k) => Some((ty::KEYWORD, 0)),
            _ => None,
        }
    }

    /// Classify an identifier from the kind of its parent node, falling back to grandparent
    /// context to distinguish a method call from a plain name/field access. The syntactic fallback for
    /// identifiers name resolution cannot place.
    fn classify_ident_syntactic(token: &SyntaxToken) -> Option<(u32, u32)> {
        use SyntaxKind::{
            ANNOTATION, ANNOTATION_PAIR, ANNOTATION_TYPE_DECL, CALL_EXPR, CATCH_CLAUSE, CLASS_DECL,
            CONSTRUCTOR_DECL, ENUM_CONSTANT, ENUM_DECL, FIELD_ACCESS, FIELD_DECL, INTERFACE_DECL,
            LOCAL_VAR_DECL, METHOD_DECL, METHOD_REF_EXPR, NAME_REF, NON_SEALED_KW, PARAM,
            QUALIFIED_NAME, RECORD_COMPONENT, RECORD_DECL, RESOURCE, TYPE, TYPE_PARAM,
            TYPE_PATTERN,
        };
        let parent = token.parent()?;
        let grandparent = || parent.parent().map(|n| n.kind());
        // Each arm names a distinct syntactic context; keeping the equal-bodied arms separate (rather
        // than merging them) is what documents which context maps to which token type.
        #[allow(clippy::match_same_arms)]
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
            LOCAL_VAR_DECL | RESOURCE | CATCH_CLAUSE | TYPE_PATTERN => {
                (ty::VARIABLE, MOD_DECLARATION)
            }
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
    const fn is_keyword(kind: SyntaxKind) -> bool {
        use SyntaxKind::{
            ABSTRACT_KW, ASSERT_KW, BOOLEAN_KW, BREAK_KW, BYTE_KW, CASE_KW, CATCH_KW, CHAR_KW,
            CLASS_KW, CONST_KW, CONTINUE_KW, DEFAULT_KW, DO_KW, DOUBLE_KW, ELSE_KW, ENUM_KW,
            EXPORTS_KW, EXTENDS_KW, FALSE_KW, FINAL_KW, FINALLY_KW, FLOAT_KW, FOR_KW, GOTO_KW,
            IF_KW, IMPLEMENTS_KW, IMPORT_KW, INSTANCEOF_KW, INT_KW, INTERFACE_KW, LONG_KW,
            MODULE_KW, NATIVE_KW, NEW_KW, NULL_KW, OPEN_KW, OPENS_KW, PACKAGE_KW, PERMITS_KW,
            PRIVATE_KW, PROTECTED_KW, PROVIDES_KW, PUBLIC_KW, RECORD_KW, REQUIRES_KW, RETURN_KW,
            SEALED_KW, SHORT_KW, STATIC_KW, STRICTFP_KW, SUPER_KW, SWITCH_KW, SYNCHRONIZED_KW,
            THIS_KW, THROW_KW, THROWS_KW, TO_KW, TRANSIENT_KW, TRANSITIVE_KW, TRUE_KW, TRY_KW,
            USES_KW, VAR_KW, VOID_KW, VOLATILE_KW, WHEN_KW, WHILE_KW, WITH_KW, YIELD_KW,
        };
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

    /// Decode an already-built token stream back to absolute positions and type names, so tests
    /// can assert on what a client would actually render.
    fn decode_tokens(toks: &SemanticTokens) -> Vec<Tok> {
        let legend = SemanticTokensBuilder::legend();
        let (mut line, mut start) = (0u32, 0u32);
        let mut out = Vec::new();
        for t in &toks.data {
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
                    .to_owned(),
                mods: t.token_modifiers_bitset,
            });
        }
        out
    }

    /// Decode the tokens produced for `text` (with no project index).
    fn decode(text: &str) -> Vec<Tok> {
        decode_tokens(&SemanticTokensBuilder::semantic_tokens(
            &jals_syntax::Parse::parse(text),
            text,
            &LineIndex::new(text),
            None,
        ))
    }

    /// Find the single token covering `needle`'s first occurrence (by column on line 0).
    fn at(text: &str, needle: &str) -> Tok {
        let col = text.find(needle).expect("needle present") as u32;
        decode(text)
            .into_iter()
            .find(|t| t.line == 0 && t.start == col)
            .unwrap_or_else(|| panic!("no token at column {col} for {needle:?}"))
    }

    /// Find the single token covering `needle`'s *last* occurrence (by column on line 0). Used to
    /// target a *use* of a name when its declaration appears earlier on the same line.
    fn at_last(text: &str, needle: &str) -> Tok {
        let col = text.rfind(needle).expect("needle present") as u32;
        decode(text)
            .into_iter()
            .find(|t| t.line == 0 && t.start == col)
            .unwrap_or_else(|| panic!("no token at column {col} for {needle:?}"))
    }

    #[test]
    fn legend_indices_match() {
        let legend = SemanticTokensBuilder::legend();
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
    fn name_references_resolve_to_their_kind() {
        // A bare name reference is classified by what it resolves to, not a flat `variable`.
        // Each `_last` targets the *use*, not the earlier declaration of the same name.
        let field = at_last("class C { int f; int g() { return f; } }", "f");
        assert_eq!(field.ty, "property");
        assert_eq!(
            field.mods, 0,
            "a use must not carry the declaration modifier"
        );

        assert_eq!(
            at_last("class C { int m(int p) { return p; } }", "p").ty,
            "parameter"
        );
        assert_eq!(
            at_last("class C { int g() { int x = 0; return x; } }", "x").ty,
            "variable"
        );
    }

    #[test]
    fn type_reference_to_a_sibling_is_precise() {
        // A type reference to a file-local sibling resolves to its declared kind (`enum`), where the
        // syntactic fallback could only say `type`.
        assert_eq!(at_last("enum E {} class C { E e; }", "E").ty, "enum");
    }

    #[test]
    fn unresolved_names_keep_the_syntactic_fallback() {
        // An external/unresolved type and a member access have no file-local binding, so they fall
        // back to syntax: `List` stays `type`, the member `field` stays `property`.
        assert_eq!(at("class C { List x; }", "List").ty, "type");
        assert_eq!(
            at("class C { void m() { var v = obj.field; } }", "field").ty,
            "property"
        );
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
    fn cross_file_type_is_classified_by_its_kind() {
        use jals_hir::{FileId, ProjectIndex};

        // `Color` is declared as an `enum` in another file and imported here. The file-local pass
        // leaves the reference unresolved (a generic `type`); the project index sharpens it to `enum`.
        let other = "package a; enum Color { RED }";
        let main = "package a; class C { Color c; }";
        let nodes = [
            (FileId(0), jals_syntax::Parse::parse(other).syntax()),
            (FileId(1), jals_syntax::Parse::parse(main).syntax()),
        ];
        let index = ProjectIndex::builder(&nodes).build();

        // Without the index: the syntactic fallback can only say `type`.
        let local = SemanticTokensBuilder::semantic_tokens(
            &jals_syntax::Parse::parse(main),
            main,
            &LineIndex::new(main),
            None,
        );
        let col = main.find("Color").unwrap() as u32;
        let kind_at = |toks: &SemanticTokens| {
            decode_tokens(toks)
                .into_iter()
                .find(|t| t.line == 0 && t.start == col)
                .map(|t| t.ty)
                .expect("no token at the `Color` reference")
        };
        assert_eq!(kind_at(&local), "type");

        // With the index: the cross-file `enum` is recognized.
        let indexed = SemanticTokensBuilder::semantic_tokens(
            &jals_syntax::Parse::parse(main),
            main,
            &LineIndex::new(main),
            Some((&index, FileId(1))),
        );
        assert_eq!(kind_at(&indexed), "enum");
    }

    /// A token with the given fields; modifiers default to none.
    fn tok(delta_line: u32, delta_start: u32, length: u32, token_type: u32) -> SemanticToken {
        SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        }
    }

    #[test]
    fn tokens_delta_splices_only_the_changed_middle() {
        let a = vec![tok(0, 0, 3, 1), tok(0, 4, 2, 2), tok(1, 0, 5, 3)];
        // Change only the middle token: the edit deletes 1 token at index 1 and inserts its
        // replacement, in flattened-int units (×5).
        let mut b = a.clone();
        b[1] = tok(0, 4, 2, 7);
        let edits = SemanticTokensBuilder::tokens_delta(&a, &b);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start, 5);
        assert_eq!(edits[0].delete_count, 5);
        assert_eq!(edits[0].data.as_deref(), Some(&[tok(0, 4, 2, 7)][..]));
    }

    #[test]
    fn tokens_delta_identical_is_empty() {
        let a = vec![tok(0, 0, 3, 1), tok(0, 4, 2, 2)];
        assert!(SemanticTokensBuilder::tokens_delta(&a, &a).is_empty());
    }

    #[test]
    fn tokens_delta_pure_append_deletes_nothing() {
        let a = vec![tok(0, 0, 3, 1), tok(0, 4, 2, 2)];
        let mut b = a.clone();
        b.push(tok(1, 0, 1, 4));
        let edits = SemanticTokensBuilder::tokens_delta(&a, &b);
        assert_eq!(edits.len(), 1);
        // Prefix of 2 tokens (10 ints), nothing deleted, one token inserted.
        assert_eq!(edits[0].start, 10);
        assert_eq!(edits[0].delete_count, 0);
        assert_eq!(edits[0].data.as_deref(), Some(&[tok(1, 0, 1, 4)][..]));
    }

    #[test]
    fn tokens_delta_pure_delete_inserts_nothing() {
        let a = vec![tok(0, 0, 3, 1), tok(0, 4, 2, 2), tok(1, 0, 5, 3)];
        let mut b = a.clone();
        b.remove(1); // drop the middle token
        let edits = SemanticTokensBuilder::tokens_delta(&a, &b);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start, 5);
        assert_eq!(edits[0].delete_count, 5);
        assert_eq!(edits[0].data.as_deref(), Some(&[][..]));
    }

    #[test]
    fn does_not_panic_on_garbage_or_empty() {
        // Invariant: handlers never panic, even on broken / arbitrary input.
        let panics = |text: &str| {
            SemanticTokensBuilder::semantic_tokens(
                &jals_syntax::Parse::parse(text),
                text,
                &LineIndex::new(text),
                None,
            )
        };
        let _ = panics("");
        let _ = panics("class");
        let _ = panics("@#$%^ <<< class {");
        let _ = panics("класс 类 😀 \0 /*");
    }
}
