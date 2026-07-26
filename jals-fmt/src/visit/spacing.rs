//! `[spacing]` — whether a single space separates two adjacent significant tokens.
//!
//! Eclipse spells this family as 219 `insert_space_*` settings (context × before/after) and
//! IntelliJ as 45 `SPACE_*` booleans. jals bundles them **by token role**, so one decision
//! function serves every syntactic position: the pair `(previous token, next token)` plus each
//! one's parent node kind is enough context to pick the rule (`MAPPING.md` §5.5).
//!
//! Centralizing it here is what lets the visitors stay about *structure*. A visitor decides where
//! a level opens and where a break may fall; it never writes "emit a space" for an ordinary token
//! sequence, so a spacing rule cannot be honored in one construct and forgotten in another.
//!
//! # Two details that are easy to get wrong
//!
//! - **`>` is not one token.** The lexer emits `>` singly and the parser fuses runs of them, so
//!   `>>`, `>=`, and `>>>=` arrive as several adjacent tokens. They have to be emitted tight or
//!   the operator changes meaning; [`Spacing::fused`] catches exactly that case by requiring
//!   source adjacency and a shared parent.
//! - **`<` and `>` wear two hats.** Inside `TYPE_ARGS` / `TYPE_PARAMS` they are delimiters and
//!   obey `within-angle-brackets`; inside an expression they are relational operators. The parent
//!   node kind is what tells them apart — there is no lexical difference.

use jals_config::fmt::Spacing as SpacingRules;
use jals_syntax::{SyntaxKind as S, SyntaxToken};

use crate::style::Style;

/// The inter-token spacing decision.
pub(crate) struct Spacing;

impl Spacing {
    /// Whether a space separates `prev` from `next`.
    pub(crate) fn between(prev: &SyntaxToken, next: &SyntaxToken, style: &Style) -> bool {
        let rules = &style.cfg.spacing;
        if Self::fused(prev, next) {
            return false;
        }
        let (pk, nk) = (prev.kind(), next.kind());
        let (pp, np) = (Self::parent(prev), Self::parent(next));

        // Selectors and the annotation sigil bind tighter than anything configurable.
        if pk == S::DOT || nk == S::DOT || pk == S::AT || nk == S::ELLIPSIS {
            return false;
        }
        if pk == S::COLON_COLON || nk == S::COLON_COLON {
            return rules.around_method_ref_double_colon;
        }

        if let Some(space) = Self::delimiters(pk, nk, pp, np, rules) {
            return space;
        }
        // An annotation opening after a word or a closing delimiter needs separating —
        // `public @interface`, `@Foo(1) @Bar`. Checked after the delimiter rules so
        // `(@NonNull String x)` still hugs its parenthesis.
        if nk == S::AT {
            return Self::is_word(pk) || matches!(pk, S::RPAREN | S::RBRACK | S::GT);
        }
        if let Some(space) = Self::separators(prev, next, pp, np, rules) {
            return space;
        }
        if let Some(space) = Self::angles(prev, next, pp, np, rules) {
            return space;
        }
        if let Some(space) = Self::operators(prev, next, pp, np, rules) {
            return space;
        }

        // Everything left is a pair of words — two identifiers, a keyword and a name, a type and
        // a variable — which always need separating.
        Self::is_word(pk) && Self::is_word(nk)
    }

    /// The node kind a token hangs off, or `SOURCE_FILE` for the impossible orphan case.
    fn parent(tok: &SyntaxToken) -> S {
        tok.parent().map_or(S::SOURCE_FILE, |node| node.kind())
    }

    /// Whether the two tokens spell one fused `>`-family operator (`>>`, `>=`, `>>>=`).
    ///
    /// Requires source adjacency *and* a shared parent, so the two `>` closing `Map<K, List<V>>`
    /// — which belong to different `TYPE_ARGS` nodes — are not mistaken for a shift.
    #[allow(
        clippy::suspicious_operation_groupings,
        reason = "`prev.end() == next.start()` is source adjacency, not a mismatched pair"
    )]
    fn fused(prev: &SyntaxToken, next: &SyntaxToken) -> bool {
        prev.kind() == S::GT
            && matches!(next.kind(), S::GT | S::EQ)
            && prev.text_range().end() == next.text_range().start()
            && prev.parent() == next.parent()
    }

    /// Whether a token is word-like, so that two of them in a row must be separated.
    fn is_word(kind: S) -> bool {
        matches!(
            kind,
            S::IDENT
                | S::UNDERSCORE
                | S::INT_LITERAL
                | S::FLOAT_LITERAL
                | S::CHAR_LITERAL
                | S::STRING_LITERAL
                | S::TEXT_BLOCK
        ) || Self::is_keyword(kind)
    }

    /// Whether a token is a keyword — everything between the first and last keyword kind, plus
    /// the three literal keywords and the context-sensitive ones the parser promotes.
    fn is_keyword(kind: S) -> bool {
        (S::ABSTRACT_KW..=S::WHILE_KW).contains(&kind)
            || matches!(kind, S::TRUE_KW | S::FALSE_KW | S::NULL_KW)
            || (S::VAR_KW..=S::WITH_KW).contains(&kind)
            || kind == S::NON_SEALED_KW
    }

    // ===== Bracketing =====

    /// Parentheses, brackets, and braces.
    fn delimiters(pk: S, nk: S, pp: S, np: S, rules: &SpacingRules) -> Option<bool> {
        match (pk, nk) {
            // An empty pair gets its own rule, since `f()` and `f( )` are a different decision
            // from `f(a)` and `f( a )`.
            (S::LPAREN, S::RPAREN) => Some(rules.within_empty_parentheses),
            (S::LBRACE, S::RBRACE) => Some(rules.within_empty_braces),
            (S::LBRACK, S::RBRACK) => Some(false),

            (S::LPAREN, _) => Some(Self::within_parens(pp, rules)),
            (_, S::RPAREN) => Some(Self::within_parens(np, rules)),
            (S::LBRACK, _) => Some(pp == S::INDEX_EXPR && rules.within_brackets),
            (_, S::RBRACK) => Some(np == S::INDEX_EXPR && rules.within_brackets),
            (_, S::LBRACK) => Some(false),
            // `String[][] xs` — an array type's `]` is followed by the name it declares. Only a
            // word is separated, so `a[0] = 1` still reaches the assignment rule and `a[0].b`
            // still hugs its selector.
            (S::RBRACK, _) if Self::is_word(nk) => Some(true),

            (_, S::LPAREN) => Some(Self::before_parens(np, rules)),
            (_, S::LBRACE) => Some(Self::before_brace(pk, np, rules)),
            (S::LBRACE, _) => Some(pp == S::ARRAY_INIT && rules.within_array_initializer_braces),
            (_, S::RBRACE) => Some(np == S::ARRAY_INIT && rules.within_array_initializer_braces),

            // `} else`, `} catch`, `} finally`, `} while`
            (S::RBRACE, S::ELSE_KW | S::CATCH_KW | S::FINALLY_KW | S::WHILE_KW) => {
                Some(rules.before_continuation_keyword)
            }
            // A cast's `)` is followed by the value it converts.
            (S::RPAREN, _) if pp == S::CAST_EXPR => Some(rules.after_type_cast),
            _ => None,
        }
    }

    /// The `within-*-parentheses` rule for a parenthesis owned by `parent`.
    const fn within_parens(parent: S, rules: &SpacingRules) -> bool {
        match parent {
            S::ARG_LIST => rules.within_method_call_parentheses,
            S::PARAM_LIST | S::LAMBDA_PARAMS => rules.within_method_parentheses,
            S::RECORD_HEADER => rules.within_record_header,
            S::ANNOTATION_ARG_LIST | S::ATTR_ARG_LIST => rules.within_annotation_parentheses,
            S::CAST_EXPR => rules.within_cast_parentheses,
            S::IF_STMT
            | S::WHILE_STMT
            | S::DO_WHILE_STMT
            | S::FOR_STMT
            | S::FOR_EACH_STMT
            | S::SWITCH_STMT
            | S::SWITCH_EXPR
            | S::CATCH_CLAUSE
            | S::SYNCHRONIZED_STMT
            | S::RESOURCE_LIST => rules.within_keyword_parentheses,
            _ => false,
        }
    }

    /// The `before-*-parentheses` rule for an opening parenthesis owned by `parent`.
    const fn before_parens(parent: S, rules: &SpacingRules) -> bool {
        match parent {
            S::ARG_LIST => rules.before_method_call_parentheses,
            S::PARAM_LIST => rules.before_method_parentheses,
            S::ANNOTATION_ARG_LIST | S::ATTR_ARG_LIST => rules.before_annotation_parentheses,
            S::IF_STMT
            | S::WHILE_STMT
            | S::DO_WHILE_STMT
            | S::FOR_STMT
            | S::FOR_EACH_STMT
            | S::SWITCH_STMT
            | S::SWITCH_EXPR
            | S::CATCH_CLAUSE
            | S::SYNCHRONIZED_STMT
            | S::TRY_STMT
            | S::RESOURCE_LIST => rules.before_keyword_parentheses,
            // A record header and a lambda parameter list hug their name; a parenthesized
            // expression after a keyword or an operator was already spaced by that token's own
            // rule. Neither adds a space of its own.
            _ => false,
        }
    }

    /// The `before-*-brace` rule for an opening brace owned by `parent`.
    ///
    /// An initializer that follows `=` is the one place two rules meet: the brace's own
    /// `before-array-initializer-left-brace` and the assignment operator's spacing. Either asking
    /// for a space is enough — `int[] xs ={1}` is not what
    /// `around-assignment-operators = true` means.
    fn before_brace(previous: S, parent: S, rules: &SpacingRules) -> bool {
        if parent != S::ARRAY_INIT {
            return rules.before_left_brace;
        }
        rules.before_array_initializer_left_brace
            || (previous == S::EQ && rules.around_assignment_operators)
    }

    // ===== Punctuation =====

    /// Commas, semicolons, colons, and the ternary `?`.
    fn separators(
        prev: &SyntaxToken,
        next: &SyntaxToken,
        pp: S,
        np: S,
        rules: &SpacingRules,
    ) -> Option<bool> {
        match (prev.kind(), next.kind()) {
            (_, S::COMMA) => Some(rules.before_comma),
            (S::COMMA, _) => Some(rules.after_comma),
            // Only a basic-`for` header's semicolons are separators; a statement terminator
            // never takes a space before it.
            (_, S::SEMICOLON) => Some(np == S::FOR_STMT && rules.before_semicolon),
            (S::SEMICOLON, _) => Some(pp == S::FOR_STMT && rules.after_semicolon),
            (_, S::QUESTION) => Some(rules.before_ternary_question),
            (S::QUESTION, _) if pp == S::TERNARY_EXPR => Some(rules.after_ternary_question),
            (_, S::COLON) => Some(Self::before_colon(np, rules)),
            (S::COLON, _) => Some(Self::after_colon(pp, rules)),
            _ => None,
        }
    }

    /// Java's five colon contexts genuinely disagree across vendors, so each keeps its own pair.
    const fn before_colon(parent: S, rules: &SpacingRules) -> bool {
        match parent {
            S::TERNARY_EXPR => rules.before_ternary_colon,
            S::FOR_EACH_STMT => rules.before_foreach_colon,
            S::LABELED_STMT => rules.before_label_colon,
            // The `:` of a colon-form case is a child of the *group*, not of the label.
            S::SWITCH_LABEL | S::SWITCH_GROUP => rules.before_case_colon,
            S::ASSERT_STMT => rules.before_assert_colon,
            _ => false,
        }
    }

    /// The `after-*-colon` half of the same five.
    const fn after_colon(parent: S, rules: &SpacingRules) -> bool {
        match parent {
            S::TERNARY_EXPR => rules.after_ternary_colon,
            S::FOR_EACH_STMT => rules.after_foreach_colon,
            S::LABELED_STMT => rules.after_label_colon,
            S::SWITCH_LABEL | S::SWITCH_GROUP => rules.after_case_colon,
            S::ASSERT_STMT => rules.after_assert_colon,
            _ => true,
        }
    }

    // ===== Angle brackets =====

    /// `<` and `>` as type-list delimiters, which is decided by the parent, not the token.
    fn angles(
        prev: &SyntaxToken,
        next: &SyntaxToken,
        pp: S,
        np: S,
        rules: &SpacingRules,
    ) -> Option<bool> {
        let (pk, nk) = (prev.kind(), next.kind());
        let delimiter = |kind: S, parent: S| {
            matches!(kind, S::LT | S::GT)
                && matches!(
                    parent,
                    S::TYPE_ARGS | S::TYPE_PARAMS | S::TYPE | S::RECORD_PATTERN
                )
        };
        match (delimiter(pk, pp), delimiter(nk, np)) {
            (true, true) => Some(rules.within_angle_brackets),
            (true, false) if pk == S::LT => Some(rules.within_angle_brackets),
            // A closing `>` followed by an operator — `Comparable<T> & Cloneable` — is not an
            // angle-bracket decision at all; let the operator's own rule answer it.
            (true, false) if Self::operator_rule(nk, np, rules).is_some() => None,
            (true, false) => Some(Self::is_word(nk)),
            (false, true) if nk == S::GT => Some(rules.within_angle_brackets),
            (false, true) => Some(pp != S::IDENT && Self::before_angle(np, rules)),
            (false, false) => None,
        }
    }

    /// Whether a `<` opening a type-parameter list takes a space before it.
    ///
    /// Only a declaration's own list has the rule; a type *use* (`Map<K, V>`) always hugs.
    const fn before_angle(parent: S, rules: &SpacingRules) -> bool {
        matches!(parent, S::TYPE_PARAMS) && rules.before_type_parameter_list
    }

    // ===== Operators =====

    /// Binary, unary, assignment, and arrow operators.
    fn operators(
        prev: &SyntaxToken,
        next: &SyntaxToken,
        pp: S,
        np: S,
        rules: &SpacingRules,
    ) -> Option<bool> {
        if let Some(space) = Self::operator_rule(next.kind(), np, rules) {
            return Some(space);
        }
        if let Some(space) = Self::operator_rule(prev.kind(), pp, rules) {
            return Some(space);
        }
        None
    }

    /// The spacing rule an operator token asks for, by class.
    fn operator_rule(kind: S, parent: S, rules: &SpacingRules) -> Option<bool> {
        // A prefix or postfix operator hugs its operand under `around-unary-operator`.
        if matches!(parent, S::UNARY_EXPR | S::POSTFIX_EXPR) {
            return matches!(
                kind,
                S::BANG | S::TILDE | S::PLUS | S::MINUS | S::PLUS_PLUS | S::MINUS_MINUS
            )
            .then_some(rules.around_unary_operator);
        }
        // The `&` of a type bound and the `|` of a multi-`catch` are type punctuation, not
        // bitwise operators, so they follow `around-type-bounds`.
        if matches!(parent, S::TYPE_PARAM | S::TYPE | S::CATCH_CLAUSE | S::PARAM)
            && matches!(kind, S::AMP | S::PIPE)
        {
            return Some(rules.around_type_bounds);
        }
        if parent == S::ANNOTATION_PAIR && kind == S::EQ {
            return Some(rules.around_annotation_eq);
        }
        Some(match kind {
            S::EQ
            | S::PLUS_EQ
            | S::MINUS_EQ
            | S::STAR_EQ
            | S::SLASH_EQ
            | S::PERCENT_EQ
            | S::AMP_EQ
            | S::PIPE_EQ
            | S::CARET_EQ
            | S::LSHIFT_EQ => rules.around_assignment_operators,
            S::AMP_AMP | S::PIPE_PIPE => rules.around_logical_operators,
            S::EQ_EQ | S::BANG_EQ => rules.around_equality_operators,
            S::LT | S::GT | S::LT_EQ | S::INSTANCEOF_KW => rules.around_relational_operators,
            S::AMP | S::PIPE | S::CARET => rules.around_bitwise_operators,
            S::PLUS | S::MINUS => rules.around_additive_operators,
            S::STAR | S::SLASH | S::PERCENT => rules.around_multiplicative_operators,
            S::LSHIFT => rules.around_shift_operators,
            S::ARROW => rules.around_lambda_arrow,
            _ => return None,
        })
    }
}
