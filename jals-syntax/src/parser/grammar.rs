//! Java grammar (recursive descent). The Milestone B extension.
//!
//! Coverage: package / import / type declaration (class / interface / enum / record / `@interface`,
//! modifier, type argument, extends, implements, sealed/permits/non-sealed) / member (field,
//! method, constructor, initializer, nested type, annotation element) / annotation argument /
//! statement (block, local variable, local type, return, if, while, do-while, for / for-each, break, continue,
//! throw, yield, assert, synchronized, try/catch/finally, switch (pattern), labeled, expression statement) /
//! expression (assignment, ternary, lambda, method reference, cast, instanceof pattern, switch expression, new, array initializer,
//! class literal, binary/unary/postfix via precedence climbing) / type (`List<Map<K, V>>`, array).
//! `>` family tokens are fused via adjacency checks.
//!
//! Returns a tree without panicking even on broken input. Recovery sets are placed throughout,
//! and `err_and_bump` guarantees progress.
//! lambda / cast / for-each / switch / pattern disambiguation uses bounded lookahead that consumes no fuel
//! ([`Parser::nth_nofuel`]) and always terminates within the input length.

use super::Parser;
use super::marker::{CompletedMarker, Marker};
use super::token_set::TokenSet;
use crate::syntax_kind::SyntaxKind;
use crate::syntax_kind::SyntaxKind::*;

/// Tokens that can begin a class body member (used for recovery).
const MEMBER_RECOVERY: TokenSet = TokenSet::new(&[
    AT,
    PUBLIC_KW,
    PROTECTED_KW,
    PRIVATE_KW,
    STATIC_KW,
    FINAL_KW,
    ABSTRACT_KW,
    NATIVE_KW,
    SYNCHRONIZED_KW,
    TRANSIENT_KW,
    VOLATILE_KW,
    STRICTFP_KW,
    DEFAULT_KW,
    CLASS_KW,
    INTERFACE_KW,
    ENUM_KW,
    RBRACE,
]);

/// Tokens that can begin a statement (used for recovery).
const STMT_RECOVERY: TokenSet = TokenSet::new(&[
    LBRACE,
    RBRACE,
    SEMICOLON,
    IF_KW,
    WHILE_KW,
    FOR_KW,
    RETURN_KW,
    DO_KW,
    SWITCH_KW,
    TRY_KW,
    THROW_KW,
    BREAK_KW,
    CONTINUE_KW,
    ASSERT_KW,
    SYNCHRONIZED_KW,
]);

/// Primitive type keywords.
const PRIMITIVE_TYPE: TokenSet = TokenSet::new(&[
    BOOLEAN_KW, BYTE_KW, SHORT_KW, INT_KW, LONG_KW, CHAR_KW, FLOAT_KW, DOUBLE_KW, VOID_KW,
]);

/// Modifier keywords (`non-sealed` is handled separately).
const MODIFIER_KW: TokenSet = TokenSet::new(&[
    PUBLIC_KW,
    PROTECTED_KW,
    PRIVATE_KW,
    STATIC_KW,
    FINAL_KW,
    ABSTRACT_KW,
    NATIVE_KW,
    SYNCHRONIZED_KW,
    TRANSIENT_KW,
    VOLATILE_KW,
    STRICTFP_KW,
    DEFAULT_KW,
]);

/// Literal tokens.
const LITERAL_TOKEN: TokenSet = TokenSet::new(&[
    INT_LITERAL,
    FLOAT_LITERAL,
    CHAR_LITERAL,
    STRING_LITERAL,
    TEXT_BLOCK,
    TRUE_KW,
    FALSE_KW,
    NULL_KW,
]);

/// Tokens that can start a unary expression following a primitive scalar cast `(int) x` (including `+`/`-`).
const CAST_FOLLOW_PRIMITIVE: TokenSet = TokenSet::new(&[
    IDENT,
    INT_LITERAL,
    FLOAT_LITERAL,
    CHAR_LITERAL,
    STRING_LITERAL,
    TEXT_BLOCK,
    TRUE_KW,
    FALSE_KW,
    NULL_KW,
    LPAREN,
    BANG,
    TILDE,
    PLUS,
    MINUS,
    PLUS_PLUS,
    MINUS_MINUS,
    NEW_KW,
    THIS_KW,
    SUPER_KW,
    SWITCH_KW,
]);

/// Tokens that can start a unary expression following a reference type cast `(Foo) x` (excluding `+`/`-`/`++`/`--`).
/// This constraint ensures `(a) - b` is treated as subtraction, not a cast.
const CAST_FOLLOW_REF: TokenSet = TokenSet::new(&[
    IDENT,
    INT_LITERAL,
    FLOAT_LITERAL,
    CHAR_LITERAL,
    STRING_LITERAL,
    TEXT_BLOCK,
    TRUE_KW,
    FALSE_KW,
    NULL_KW,
    LPAREN,
    BANG,
    TILDE,
    NEW_KW,
    THIS_KW,
    SUPER_KW,
    SWITCH_KW,
]);

/// Token set that can begin an expression (for lookahead only).
const EXPR_START: TokenSet = TokenSet::new(&[
    INT_LITERAL,
    FLOAT_LITERAL,
    CHAR_LITERAL,
    STRING_LITERAL,
    TEXT_BLOCK,
    TRUE_KW,
    FALSE_KW,
    NULL_KW,
    IDENT,
    LPAREN,
    THIS_KW,
    SUPER_KW,
    NEW_KW,
    SWITCH_KW,
    BANG,
    TILDE,
    PLUS,
    MINUS,
    PLUS_PLUS,
    MINUS_MINUS,
]);

/// Entry point. Parses a compilation unit.
pub(super) fn root(p: &mut Parser) {
    let m = p.start();
    if p.at(PACKAGE_KW) {
        package_decl(p);
    }
    while p.at(IMPORT_KW) {
        import_decl(p);
    }
    while !p.at_eof() {
        let before = p.pos();
        type_decl(p);
        // Progress guarantee (last-resort safeguard).
        if p.pos() == before {
            p.err_and_bump("unexpected token");
        }
    }
    m.complete(p, SOURCE_FILE);
}

fn package_decl(p: &mut Parser) {
    let m = p.start();
    p.bump(PACKAGE_KW);
    qualified_name(p, false);
    p.expect(SEMICOLON);
    m.complete(p, PACKAGE_DECL);
}

fn import_decl(p: &mut Parser) {
    let m = p.start();
    p.bump(IMPORT_KW);
    p.eat(STATIC_KW);
    qualified_name(p, true);
    p.expect(SEMICOLON);
    m.complete(p, IMPORT_DECL);
}

/// Dotted name. If `allow_star` is true, allows a trailing `.*` (for imports).
fn qualified_name(p: &mut Parser, allow_star: bool) {
    let m = p.start();
    p.expect(IDENT);
    while p.at(DOT) {
        if allow_star && p.nth_at(1, STAR) {
            p.bump(DOT);
            p.bump(STAR);
            break;
        }
        if !p.nth_at(1, IDENT) {
            break;
        }
        p.bump(DOT);
        p.bump(IDENT);
    }
    m.complete(p, QUALIFIED_NAME);
}

// ===== Type declarations =====

/// Whether this is the start of `record Foo(...)` / `record Foo<T>(...)` (`record` is a contextual keyword).
/// Requires `(` or `<` after the name to distinguish from variable declarations like `record r = 1;`.
fn at_record_decl(p: &Parser) -> bool {
    p.at_contextual_kw("record") && p.nth_at(1, IDENT) && (p.nth_at(2, LPAREN) || p.nth_at(2, LT))
}

/// Type declaration (class / interface / enum / record / `@interface`).
fn type_decl(p: &mut Parser) {
    let m = p.start();
    modifiers(p);
    match p.current() {
        CLASS_KW => class_rest(p, m),
        INTERFACE_KW => interface_rest(p, m),
        ENUM_KW => enum_rest(p, m),
        AT if p.nth_at(1, INTERFACE_KW) => annotation_type_rest(p, m),
        _ if at_record_decl(p) => record_rest(p, m),
        _ => {
            m.abandon(p);
            p.err_and_bump("expected a type declaration");
        }
    }
}

/// Modifier sequence (annotations, modifier keywords, `sealed`, `non-sealed`). Always creates a node.
fn modifiers(p: &mut Parser) {
    let m = p.start();
    loop {
        if p.at(AT) && !p.nth_at(1, INTERFACE_KW) {
            annotation(p);
        } else if p.at_ts(MODIFIER_KW) {
            p.bump_any();
        } else if at_non_sealed(p) {
            non_sealed(p);
        } else if p.at_contextual_kw("sealed") {
            p.bump_remap(SEALED_KW);
        } else {
            break;
        }
    }
    m.complete(p, MODIFIERS);
}

fn annotation(p: &mut Parser) {
    let m = p.start();
    p.bump(AT);
    qualified_name(p, false);
    if p.at(LPAREN) {
        annotation_arg_list(p);
    }
    m.complete(p, ANNOTATION);
}

/// Annotation argument list (`(value)` / `(name = value, ...)`).
fn annotation_arg_list(p: &mut Parser) {
    let m = p.start();
    p.bump(LPAREN);
    while !p.at(RPAREN) && !p.at_eof() {
        let before = p.pos();
        if p.at(IDENT) && p.nth_at(1, EQ) {
            let pair = p.start();
            p.bump(IDENT);
            p.bump(EQ);
            element_value(p);
            pair.complete(p, ANNOTATION_PAIR);
        } else {
            element_value(p);
        }
        if p.pos() == before {
            p.err_and_bump("unexpected argument");
        }
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(RPAREN);
    m.complete(p, ANNOTATION_ARG_LIST);
}

/// Annotation element value or array initializer element (expression / nested annotation / array).
fn element_value(p: &mut Parser) {
    if p.at(LBRACE) {
        array_init(p);
    } else if p.at(AT) && !p.nth_at(1, INTERFACE_KW) {
        annotation(p);
    } else {
        expr(p);
    }
}

/// Array initializer `{ a, b, c }` (nested, trailing comma allowed).
fn array_init(p: &mut Parser) {
    let m = p.start();
    p.bump(LBRACE);
    while !p.at(RBRACE) && !p.at_eof() {
        let before = p.pos();
        element_value(p);
        if p.pos() == before {
            p.err_and_bump("unexpected element");
        }
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(RBRACE);
    m.complete(p, ARRAY_INIT);
}

/// Detects `non-sealed` (`IDENT("non") MINUS IDENT("sealed")` adjacent).
fn at_non_sealed(p: &Parser) -> bool {
    p.at_contextual_kw("non")
        && p.nth_at(1, MINUS)
        && p.nth_adjacent(0)
        && p.nth_adjacent(1)
        && p.nth_at(2, IDENT)
        && p.nth_text(2) == "sealed"
}

/// Re-combines `non-sealed` into a single `NON_SEALED_KW` node.
fn non_sealed(p: &mut Parser) {
    let m = p.start();
    p.bump_any(); // non
    p.bump_any(); // -
    p.bump_any(); // sealed
    m.complete(p, NON_SEALED_KW);
}

/// After `class` (modifiers already consumed by the caller, `m` is the enclosing start marker).
fn class_rest(p: &mut Parser, m: Marker) {
    p.bump(CLASS_KW);
    p.expect(IDENT);
    if p.at(LT) {
        type_params(p);
    }
    if p.at(EXTENDS_KW) {
        extends_clause(p, false);
    }
    if p.at(IMPLEMENTS_KW) {
        implements_clause(p);
    }
    if p.at_contextual_kw("permits") {
        permits_clause(p);
    }
    class_body(p);
    m.complete(p, CLASS_DECL);
}

/// After `interface`.
fn interface_rest(p: &mut Parser, m: Marker) {
    p.bump(INTERFACE_KW);
    p.expect(IDENT);
    if p.at(LT) {
        type_params(p);
    }
    if p.at(EXTENDS_KW) {
        // interfaces can extend multiple types.
        extends_clause(p, true);
    }
    if p.at_contextual_kw("permits") {
        permits_clause(p);
    }
    class_body(p);
    m.complete(p, INTERFACE_DECL);
}

/// After `enum`.
fn enum_rest(p: &mut Parser, m: Marker) {
    p.bump(ENUM_KW);
    p.expect(IDENT);
    if p.at(IMPLEMENTS_KW) {
        implements_clause(p);
    }
    enum_body(p);
    m.complete(p, ENUM_DECL);
}

fn enum_body(p: &mut Parser) {
    let m = p.start();
    if !p.expect(LBRACE) {
        m.complete(p, ENUM_BODY);
        return;
    }
    // Constant list (up to `;` or `}`).
    while !p.at(RBRACE) && !p.at(SEMICOLON) && !p.at_eof() {
        let before = p.pos();
        if p.at(IDENT) || p.at(AT) {
            enum_constant(p);
        } else {
            p.err_and_bump("expected an enum constant");
        }
        if p.pos() == before {
            p.err_and_bump("unexpected token");
        }
        if !p.eat(COMMA) {
            break;
        }
    }
    // Optional `;` followed by members.
    if p.eat(SEMICOLON) {
        while !p.at(RBRACE) && !p.at_eof() {
            let before = p.pos();
            member(p);
            if p.pos() == before {
                p.err_and_bump("unexpected token");
            }
        }
    }
    p.expect(RBRACE);
    m.complete(p, ENUM_BODY);
}

fn enum_constant(p: &mut Parser) {
    let m = p.start();
    while p.at(AT) && !p.nth_at(1, INTERFACE_KW) {
        annotation(p);
    }
    p.expect(IDENT);
    if p.at(LPAREN) {
        arg_list(p);
    }
    if p.at(LBRACE) {
        // Class body specific to this constant.
        class_body(p);
    }
    m.complete(p, ENUM_CONSTANT);
}

/// After `record`.
fn record_rest(p: &mut Parser, m: Marker) {
    p.bump_remap(RECORD_KW);
    p.expect(IDENT);
    if p.at(LT) {
        type_params(p);
    }
    record_header(p);
    if p.at(IMPLEMENTS_KW) {
        implements_clause(p);
    }
    class_body(p);
    m.complete(p, RECORD_DECL);
}

fn record_header(p: &mut Parser) {
    let m = p.start();
    if !p.expect(LPAREN) {
        m.complete(p, RECORD_HEADER);
        return;
    }
    while !p.at(RPAREN) && !p.at_eof() {
        let comp = p.start();
        modifiers(p);
        type_(p);
        p.eat(ELLIPSIS);
        p.expect(IDENT);
        comp.complete(p, RECORD_COMPONENT);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(RPAREN);
    m.complete(p, RECORD_HEADER);
}

/// After `@interface` (annotation type declaration).
fn annotation_type_rest(p: &mut Parser, m: Marker) {
    p.bump(AT);
    p.bump(INTERFACE_KW);
    p.expect(IDENT);
    class_body(p);
    m.complete(p, ANNOTATION_TYPE_DECL);
}

fn extends_clause(p: &mut Parser, multi: bool) {
    let c = p.start();
    p.bump(EXTENDS_KW);
    type_(p);
    if multi {
        while p.eat(COMMA) {
            type_(p);
        }
    }
    c.complete(p, EXTENDS_CLAUSE);
}

fn implements_clause(p: &mut Parser) {
    let c = p.start();
    p.bump(IMPLEMENTS_KW);
    type_(p);
    while p.eat(COMMA) {
        type_(p);
    }
    c.complete(p, IMPLEMENTS_CLAUSE);
}

fn permits_clause(p: &mut Parser) {
    let c = p.start();
    p.bump_remap(PERMITS_KW);
    type_(p);
    while p.eat(COMMA) {
        type_(p);
    }
    c.complete(p, PERMITS_CLAUSE);
}

fn type_params(p: &mut Parser) {
    let m = p.start();
    p.bump(LT);
    while !p.at(GT) && !p.at_eof() {
        let tp = p.start();
        // Type parameters may also carry annotations.
        while p.at(AT) && !p.nth_at(1, INTERFACE_KW) {
            annotation(p);
        }
        p.expect(IDENT);
        if p.at(EXTENDS_KW) {
            p.bump(EXTENDS_KW);
            type_(p);
            while p.at(AMP) {
                p.bump(AMP);
                type_(p);
            }
        }
        tp.complete(p, TYPE_PARAM);
        if !p.eat(COMMA) {
            break;
        }
    }
    expect_gt(p);
    m.complete(p, TYPE_PARAMS);
}

fn class_body(p: &mut Parser) {
    let m = p.start();
    if !p.expect(LBRACE) {
        m.complete(p, CLASS_BODY);
        return;
    }
    while !p.at(RBRACE) && !p.at_eof() {
        let before = p.pos();
        member(p);
        // Progress guarantee: if member consumed no tokens, force-wrap one token as ERROR.
        if p.pos() == before {
            p.err_and_bump("unexpected token");
        }
    }
    p.expect(RBRACE);
    m.complete(p, CLASS_BODY);
}

/// Member of class / interface / enum / `@interface`.
fn member(p: &mut Parser) {
    if p.at(SEMICOLON) {
        // Empty member.
        p.bump(SEMICOLON);
        return;
    }
    let m = p.start();
    modifiers(p);

    // Nested type declaration.
    match p.current() {
        CLASS_KW => return class_rest(p, m),
        INTERFACE_KW => return interface_rest(p, m),
        ENUM_KW => return enum_rest(p, m),
        AT if p.nth_at(1, INTERFACE_KW) => return annotation_type_rest(p, m),
        _ => {}
    }
    if at_record_decl(p) {
        return record_rest(p, m);
    }

    // Initializer block (`{ ... }` / `static { ... }`).
    if p.at(LBRACE) {
        block(p);
        m.complete(p, INITIALIZER);
        return;
    }

    // Type arguments for generic methods/constructors.
    if p.at(LT) {
        type_params(p);
    }

    // Compact canonical constructor (record): `Name { ... }`.
    if p.at(IDENT) && p.nth_at(1, LBRACE) {
        p.bump(IDENT);
        block(p);
        m.complete(p, CONSTRUCTOR_DECL);
        return;
    }

    // Constructor: `Name ( ... )`.
    if p.at(IDENT) && p.nth_at(1, LPAREN) {
        p.bump(IDENT);
        param_list(p);
        if p.at(THROWS_KW) {
            throws_clause(p);
        }
        if p.at(LBRACE) {
            block(p);
        } else {
            p.expect(SEMICOLON);
        }
        m.complete(p, CONSTRUCTOR_DECL);
        return;
    }

    // Otherwise starts with a type (field or method).
    if !at_type_start(p) {
        m.abandon(p);
        p.err_recover("expected a member declaration", MEMBER_RECOVERY);
        return;
    }
    type_(p);
    p.expect(IDENT);
    if p.at(LPAREN) {
        // Method (including annotation elements).
        param_list(p);
        // Old-style return-type array dimensions `m()[]` (each optionally annotated).
        dims(p);
        if p.at(THROWS_KW) {
            throws_clause(p);
        }
        if p.at(DEFAULT_KW) {
            // Default value for annotation element.
            let d = p.start();
            p.bump(DEFAULT_KW);
            element_value(p);
            d.complete(p, ANNOTATION_DEFAULT);
        }
        if p.at(LBRACE) {
            block(p);
        } else {
            p.expect(SEMICOLON);
        }
        m.complete(p, METHOD_DECL);
    } else {
        // Field (supports multiple declarators, array dimensions, and array initializers).
        field_tail(p);
        p.expect(SEMICOLON);
        m.complete(p, FIELD_DECL);
    }
}

/// Remainder of a field/local variable declarator (the first name is already consumed).
fn field_tail(p: &mut Parser) {
    dims(p);
    if p.eat(EQ) {
        var_init(p);
    }
    while p.eat(COMMA) {
        p.expect(IDENT);
        dims(p);
        if p.eat(EQ) {
            var_init(p);
        }
    }
}

/// Skips a sequence of array dimensions (`[]`), each optionally annotated (`String @A []`).
fn dims(p: &mut Parser) {
    loop {
        // An annotation here belongs to a dimension only if `[]` follows it
        // (`String @A []`); otherwise leave it for whatever comes next.
        if p.at(AT) && !p.nth_at(1, INTERFACE_KW) {
            let i = skip_annotations_lookahead(p, 0);
            if p.nth_nofuel(i) == LBRACK && p.nth_nofuel(i + 1) == RBRACK {
                while p.at(AT) && !p.nth_at(1, INTERFACE_KW) {
                    annotation(p);
                }
                p.bump(LBRACK);
                p.bump(RBRACK);
                continue;
            }
            break;
        }
        if p.at(LBRACK) && p.nth_at(1, RBRACK) {
            p.bump(LBRACK);
            p.bump(RBRACK);
            continue;
        }
        break;
    }
}

/// Variable initializer (array initializer `{...}` or an expression).
fn var_init(p: &mut Parser) {
    if p.at(LBRACE) {
        array_init(p);
    } else {
        expr(p);
    }
}

fn throws_clause(p: &mut Parser) {
    let m = p.start();
    p.bump(THROWS_KW);
    type_(p);
    while p.eat(COMMA) {
        type_(p);
    }
    m.complete(p, THROWS_CLAUSE);
}

fn param_list(p: &mut Parser) {
    let m = p.start();
    p.bump(LPAREN);
    while !p.at(RPAREN) && !p.at_eof() {
        let param = p.start();
        modifiers(p);
        type_(p);
        p.eat(ELLIPSIS); // varargs.
        // Also allows a `this` receiver parameter (`Foo this`).
        if p.at(THIS_KW) {
            p.bump(THIS_KW);
        } else {
            p.expect(IDENT);
            dims(p);
        }
        param.complete(p, PARAM);
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(RPAREN);
    m.complete(p, PARAM_LIST);
}

// ===== Types =====

/// Whether this can begin a type.
fn at_type_start(p: &Parser) -> bool {
    p.at_ts(PRIMITIVE_TYPE) || p.at(IDENT) || p.at_contextual_kw("var")
}

fn type_(p: &mut Parser) {
    let m = p.start();
    // Annotations on types.
    while p.at(AT) && !p.nth_at(1, INTERFACE_KW) {
        annotation(p);
    }
    if p.at_contextual_kw("var") {
        p.bump_remap(VAR_KW);
    } else if p.at_ts(PRIMITIVE_TYPE) {
        p.bump_any();
    } else {
        // Reference type: name + optional type arguments + dotted continuation.
        p.expect(IDENT);
        if p.at(LT) {
            type_args(p);
        }
        while p.at(DOT) && p.nth_at(1, IDENT) {
            p.bump(DOT);
            p.bump(IDENT);
            if p.at(LT) {
                type_args(p);
            }
        }
    }
    dims(p);
    m.complete(p, TYPE);
}

fn type_args(p: &mut Parser) {
    let m = p.start();
    p.bump(LT);
    if !p.at(GT) {
        type_arg(p);
        while p.eat(COMMA) {
            type_arg(p);
        }
    }
    expect_gt(p);
    m.complete(p, TYPE_ARGS);
}

fn type_arg(p: &mut Parser) {
    if p.at(QUESTION) {
        // Wildcard `? extends T` / `? super T`.
        p.bump(QUESTION);
        if p.at(EXTENDS_KW) || p.at(SUPER_KW) {
            p.bump_any();
            type_(p);
        }
    } else {
        type_(p);
    }
}

/// Consumes one `>` that closes a type argument/type parameter. `>>` and similar are
/// represented as adjacent `GT` tokens, so this always consumes only a single `GT` (the rest is consumed by the outer caller).
fn expect_gt(p: &mut Parser) {
    if !p.eat(GT) {
        p.error("expected `>`");
    }
}

/// Skips one type starting at `start` using fuel-free lookahead, returning the offset immediately after it.
/// Returns `None` if the tokens cannot be interpreted as a type. Used for lambda/cast/pattern/local variable disambiguation.
fn skip_type(p: &Parser, start: usize) -> Option<usize> {
    let mut i = start;
    if PRIMITIVE_TYPE.contains(p.nth_nofuel(i)) {
        i += 1;
    } else if p.nth_nofuel(i) == IDENT {
        i += 1;
        loop {
            if p.nth_nofuel(i) == LT {
                // Skip a balanced `<...>` (`>` is a single GT, `>>` is two GTs).
                let mut depth = 0i32;
                loop {
                    match p.nth_nofuel(i) {
                        LT => {
                            depth += 1;
                            i += 1;
                        }
                        GT => {
                            depth -= 1;
                            i += 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        EOF | SEMICOLON | LBRACE | RBRACE => return None,
                        _ => i += 1,
                    }
                }
            }
            if p.nth_nofuel(i) == DOT && p.nth_nofuel(i + 1) == IDENT {
                i += 2;
                continue;
            }
            break;
        }
    } else {
        return None;
    }
    while p.nth_nofuel(i) == LBRACK && p.nth_nofuel(i + 1) == RBRACK {
        i += 2;
    }
    Some(i)
}

// ===== Statements =====

fn block(p: &mut Parser) {
    let m = p.start();
    p.bump(LBRACE);
    while !p.at(RBRACE) && !p.at_eof() {
        let before = p.pos();
        stmt(p);
        // Progress guarantee (last-resort safeguard).
        if p.pos() == before {
            p.err_and_bump("unexpected token");
        }
    }
    p.expect(RBRACE);
    m.complete(p, BLOCK);
}

fn stmt(p: &mut Parser) {
    match p.current() {
        LBRACE => block(p),
        SEMICOLON => {
            let m = p.start();
            p.bump(SEMICOLON);
            m.complete(p, EMPTY_STMT);
        }
        IF_KW => if_stmt(p),
        WHILE_KW => while_stmt(p),
        DO_KW => do_while_stmt(p),
        FOR_KW => for_stmt(p),
        RETURN_KW => return_stmt(p),
        THROW_KW => throw_stmt(p),
        BREAK_KW => break_or_continue(p, BREAK_KW, BREAK_STMT),
        CONTINUE_KW => break_or_continue(p, CONTINUE_KW, CONTINUE_STMT),
        ASSERT_KW => assert_stmt(p),
        SYNCHRONIZED_KW => synchronized_stmt(p),
        TRY_KW => try_stmt(p),
        SWITCH_KW => switch_stmt(p),
        CLASS_KW | INTERFACE_KW | ENUM_KW => type_decl(p),
        AT if p.nth_at(1, INTERFACE_KW) => type_decl(p),
        _ => {
            // Labeled statement (`label:`). Distinguishable from ternary `?:` by the absence of `?`.
            if p.at(IDENT) && p.nth_at(1, COLON) {
                return labeled_stmt(p);
            }
            // Local record declaration.
            if at_record_decl(p) {
                return type_decl(p);
            }
            // yield statement (inside a switch expression).
            if at_yield_stmt(p) {
                return yield_stmt(p);
            }
            // Local type declaration with modifiers/annotations.
            if (p.at_ts(MODIFIER_KW) || p.at(AT)) && at_local_type_decl(p) {
                return type_decl(p);
            }
            if at_local_var_decl(p) {
                local_var_decl(p);
            } else if at_expr_start(p) {
                let m = p.start();
                expr(p);
                p.expect(SEMICOLON);
                m.complete(p, EXPR_STMT);
            } else {
                p.err_recover("expected a statement", STMT_RECOVERY);
            }
        }
    }
}

fn labeled_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(IDENT);
    p.bump(COLON);
    stmt(p);
    m.complete(p, LABELED_STMT);
}

fn return_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(RETURN_KW);
    if !p.at(SEMICOLON) {
        expr(p);
    }
    p.expect(SEMICOLON);
    m.complete(p, RETURN_STMT);
}

fn throw_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(THROW_KW);
    expr(p);
    p.expect(SEMICOLON);
    m.complete(p, THROW_STMT);
}

fn break_or_continue(p: &mut Parser, kw: SyntaxKind, node: SyntaxKind) {
    let m = p.start();
    p.bump(kw);
    if p.at(IDENT) {
        p.bump(IDENT); // label.
    }
    p.expect(SEMICOLON);
    m.complete(p, node);
}

fn assert_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(ASSERT_KW);
    expr(p);
    if p.eat(COLON) {
        expr(p);
    }
    p.expect(SEMICOLON);
    m.complete(p, ASSERT_STMT);
}

fn synchronized_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(SYNCHRONIZED_KW);
    p.expect(LPAREN);
    expr(p);
    p.expect(RPAREN);
    if p.at(LBRACE) {
        block(p);
    }
    m.complete(p, SYNCHRONIZED_STMT);
}

fn yield_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump_remap(YIELD_KW);
    expr(p);
    p.expect(SEMICOLON);
    m.complete(p, YIELD_STMT);
}

fn try_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(TRY_KW);
    if p.at(LPAREN) {
        resource_list(p);
    }
    if p.at(LBRACE) {
        block(p);
    }
    while p.at(CATCH_KW) {
        catch_clause(p);
    }
    if p.at(FINALLY_KW) {
        finally_clause(p);
    }
    m.complete(p, TRY_STMT);
}

fn resource_list(p: &mut Parser) {
    let m = p.start();
    p.bump(LPAREN);
    while !p.at(RPAREN) && !p.at_eof() {
        let before = p.pos();
        resource(p);
        if p.pos() == before {
            p.err_and_bump("unexpected token");
        }
        if !p.eat(SEMICOLON) {
            break;
        }
    }
    p.expect(RPAREN);
    m.complete(p, RESOURCE_LIST);
}

fn resource(p: &mut Parser) {
    let m = p.start();
    if at_local_var_decl(p) {
        // Resource variable declaration: {modifiers} Type id = expr
        modifiers(p);
        type_(p);
        p.expect(IDENT);
        p.expect(EQ);
        expr(p);
    } else {
        // Reference to an existing variable (Java 9+).
        expr(p);
    }
    m.complete(p, RESOURCE);
}

fn catch_clause(p: &mut Parser) {
    let m = p.start();
    p.bump(CATCH_KW);
    p.expect(LPAREN);
    modifiers(p);
    type_(p);
    while p.at(PIPE) {
        // Multi-catch `A | B`.
        p.bump(PIPE);
        type_(p);
    }
    p.expect(IDENT);
    p.expect(RPAREN);
    if p.at(LBRACE) {
        block(p);
    }
    m.complete(p, CATCH_CLAUSE);
}

fn finally_clause(p: &mut Parser) {
    let m = p.start();
    p.bump(FINALLY_KW);
    if p.at(LBRACE) {
        block(p);
    }
    m.complete(p, FINALLY_CLAUSE);
}

/// Whether to treat `yield` as a statement (when followed by an unambiguous expression start). Cases like `yield = 3;` (variable) are sent to expression statement.
fn at_yield_stmt(p: &Parser) -> bool {
    if !p.at_contextual_kw("yield") {
        return false;
    }
    matches!(
        p.nth_nofuel(1),
        IDENT
            | INT_LITERAL
            | FLOAT_LITERAL
            | CHAR_LITERAL
            | STRING_LITERAL
            | TEXT_BLOCK
            | TRUE_KW
            | FALSE_KW
            | NULL_KW
            | BANG
            | TILDE
            | NEW_KW
            | THIS_KW
            | SUPER_KW
            | SWITCH_KW
            | LPAREN
    )
}

fn if_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(IF_KW);
    p.expect(LPAREN);
    expr(p);
    p.expect(RPAREN);
    stmt(p);
    if p.at(ELSE_KW) {
        p.bump(ELSE_KW);
        stmt(p);
    }
    m.complete(p, IF_STMT);
}

fn while_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(WHILE_KW);
    p.expect(LPAREN);
    expr(p);
    p.expect(RPAREN);
    stmt(p);
    m.complete(p, WHILE_STMT);
}

fn do_while_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(DO_KW);
    stmt(p);
    p.expect(WHILE_KW);
    p.expect(LPAREN);
    expr(p);
    p.expect(RPAREN);
    p.expect(SEMICOLON);
    m.complete(p, DO_WHILE_STMT);
}

fn for_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(FOR_KW);
    p.expect(LPAREN);
    if at_for_each(p) {
        // for-each: {modifiers} Type id : expr
        modifiers(p);
        type_(p);
        p.expect(IDENT);
        p.expect(COLON);
        expr(p);
        p.expect(RPAREN);
        stmt(p);
        m.complete(p, FOR_EACH_STMT);
    } else {
        // C-style for: init ; cond ; update
        for_init(p);
        p.expect(SEMICOLON);
        if !p.at(SEMICOLON) {
            expr(p);
        }
        p.expect(SEMICOLON);
        if !p.at(RPAREN) {
            expr(p);
            while p.eat(COMMA) {
                expr(p);
            }
        }
        p.expect(RPAREN);
        stmt(p);
        m.complete(p, FOR_STMT);
    }
}

/// Whether the for header is a for-each (`:` appears at depth 0 before `;`) — fuel-free lookahead.
fn at_for_each(p: &Parser) -> bool {
    let mut depth = 0i32;
    let mut ternary = 0i32;
    let mut i = 0usize;
    loop {
        match p.nth_nofuel(i) {
            LPAREN | LBRACK | LBRACE => depth += 1,
            RPAREN | RBRACK | RBRACE => {
                if depth == 0 {
                    return false; // End of the header: no `:` found.
                }
                depth -= 1;
            }
            SEMICOLON if depth == 0 && ternary == 0 => return false,
            QUESTION if depth == 0 => ternary += 1,
            COLON if depth == 0 => {
                if ternary > 0 {
                    ternary -= 1;
                } else {
                    return true;
                }
            }
            EOF => return false,
            _ => {}
        }
        i += 1;
    }
}

fn for_init(p: &mut Parser) {
    if p.at(SEMICOLON) {
        return; // empty.
    }
    if (p.at_ts(MODIFIER_KW) || p.at(AT)) && at_local_type_decl(p) {
        // A local type in for-init is unusual, but treat it as a declaration if it appears.
        type_decl(p);
        return;
    }
    if at_local_var_decl(p) {
        let m = p.start();
        var_decl_inner(p);
        m.complete(p, LOCAL_VAR_DECL);
    } else {
        expr(p);
        while p.eat(COMMA) {
            expr(p);
        }
    }
}

/// Whether this is the start of a local variable declaration.
fn at_local_var_decl(p: &Parser) -> bool {
    if p.at_ts(MODIFIER_KW) {
        return true; // final etc. (local types are filtered out by the caller first).
    }
    if p.at(AT) && !p.nth_at(1, INTERFACE_KW) {
        return true; // Annotated local variable.
    }
    if p.at_contextual_kw("var") {
        return true;
    }
    if p.at_ts(PRIMITIVE_TYPE) {
        return true;
    }
    if p.at(IDENT) {
        // Type + binding name (`Foo x` / `List<T> x` / `a.B c` / `int[] a`).
        if let Some(i) = skip_type(p, 0) {
            return p.nth_nofuel(i) == IDENT;
        }
    }
    false
}

/// Whether the token after skipping modifiers/annotations is a type declaration keyword — fuel-free lookahead.
fn at_local_type_decl(p: &Parser) -> bool {
    let i = skip_modifiers_lookahead(p, 0);
    if i == 0 {
        return false;
    }
    matches!(p.nth_nofuel(i), CLASS_KW | INTERFACE_KW | ENUM_KW)
        || (p.nth_nofuel(i) == AT && p.nth_nofuel(i + 1) == INTERFACE_KW)
}

/// Skips modifier keywords and annotations (including `@Name(...)`) and returns the next offset.
fn skip_modifiers_lookahead(p: &Parser, start: usize) -> usize {
    let mut i = start;
    loop {
        let k = p.nth_nofuel(i);
        if MODIFIER_KW.contains(k) {
            i += 1;
            continue;
        }
        if k == AT && p.nth_nofuel(i + 1) != INTERFACE_KW {
            i = skip_one_annotation_lookahead(p, i);
            continue;
        }
        break;
    }
    i
}

/// Skips a run of annotations (each not `@interface`) starting at `start`, returning the next offset.
fn skip_annotations_lookahead(p: &Parser, start: usize) -> usize {
    let mut i = start;
    while p.nth_nofuel(i) == AT && p.nth_nofuel(i + 1) != INTERFACE_KW {
        i = skip_one_annotation_lookahead(p, i);
    }
    i
}

/// Skips a single annotation (`@Name`, `@a.b.Name`, or `@Name(...)`) whose `@` is at offset `i`,
/// returning the offset just past it.
fn skip_one_annotation_lookahead(p: &Parser, i: usize) -> usize {
    let mut i = i + 1; // `@`
    if p.nth_nofuel(i) == IDENT {
        i += 1;
    }
    while p.nth_nofuel(i) == DOT && p.nth_nofuel(i + 1) == IDENT {
        i += 2;
    }
    if p.nth_nofuel(i) == LPAREN {
        let mut depth = 0i32;
        loop {
            match p.nth_nofuel(i) {
                LPAREN => {
                    depth += 1;
                    i += 1;
                }
                RPAREN => {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        break;
                    }
                }
                EOF => return i,
                _ => i += 1,
            }
        }
    }
    i
}

/// Whether the parser is positioned at an annotated array dimension (`@A [` …), as can begin a
/// dimension in an array creation expression (`new int @A [n]`).
fn at_annotated_dim(p: &Parser) -> bool {
    p.at(AT)
        && !p.nth_at(1, INTERFACE_KW)
        && p.nth_nofuel(skip_annotations_lookahead(p, 0)) == LBRACK
}

fn local_var_decl(p: &mut Parser) {
    let m = p.start();
    var_decl_inner(p);
    p.expect(SEMICOLON);
    m.complete(p, LOCAL_VAR_DECL);
}

/// Body of a local variable declaration (does not consume `;`). Also used in for-init.
fn var_decl_inner(p: &mut Parser) {
    modifiers(p);
    type_(p);
    p.expect(IDENT);
    field_tail(p);
}

// ===== switch (shared body for statement and expression) =====

fn switch_stmt(p: &mut Parser) {
    let m = p.start();
    p.bump(SWITCH_KW);
    p.expect(LPAREN);
    expr(p);
    p.expect(RPAREN);
    switch_block(p);
    m.complete(p, SWITCH_STMT);
}

fn switch_block(p: &mut Parser) {
    let m = p.start();
    if !p.expect(LBRACE) {
        m.complete(p, SWITCH_BLOCK);
        return;
    }
    while !p.at(RBRACE) && !p.at_eof() {
        let before = p.pos();
        switch_entry(p);
        if p.pos() == before {
            p.err_and_bump("unexpected token");
        }
    }
    p.expect(RBRACE);
    m.complete(p, SWITCH_BLOCK);
}

fn switch_entry(p: &mut Parser) {
    if !(p.at(CASE_KW) || p.at(DEFAULT_KW)) {
        p.err_and_bump("expected `case` or `default`");
        return;
    }
    if label_is_arrow(p) {
        // Arrow rule: label -> (block | throw | expr ;)
        let m = p.start();
        switch_label(p);
        p.expect(ARROW);
        if p.at(LBRACE) {
            block(p);
        } else if p.at(THROW_KW) {
            throw_stmt(p);
        } else {
            expr(p);
            p.expect(SEMICOLON);
        }
        m.complete(p, SWITCH_RULE);
    } else {
        // Colon group: label: (label:)* statements
        let m = p.start();
        switch_label(p);
        p.expect(COLON);
        while p.at(CASE_KW) || p.at(DEFAULT_KW) {
            switch_label(p);
            p.expect(COLON);
        }
        while !p.at(RBRACE) && !p.at(CASE_KW) && !p.at(DEFAULT_KW) && !p.at_eof() {
            let before = p.pos();
            stmt(p);
            if p.pos() == before {
                break;
            }
        }
        m.complete(p, SWITCH_GROUP);
    }
}

/// Whether this label is in arrow form (`->` appears at depth 0 before `:`). Fuel-free lookahead.
fn label_is_arrow(p: &Parser) -> bool {
    let mut depth = 0i32;
    let mut ternary = 0i32;
    let mut i = 0usize;
    loop {
        match p.nth_nofuel(i) {
            LPAREN | LBRACK => depth += 1,
            RPAREN | RBRACK => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            ARROW if depth == 0 => return true,
            QUESTION if depth == 0 => ternary += 1,
            COLON if depth == 0 => {
                if ternary > 0 {
                    ternary -= 1;
                } else {
                    return false;
                }
            }
            SEMICOLON if depth == 0 => return false,
            RBRACE | EOF => return false,
            _ => {}
        }
        i += 1;
    }
}

fn switch_label(p: &mut Parser) {
    let m = p.start();
    if p.at(DEFAULT_KW) {
        p.bump(DEFAULT_KW);
    } else {
        p.bump(CASE_KW);
        switch_case_item(p);
        while p.eat(COMMA) {
            switch_case_item(p);
        }
    }
    m.complete(p, SWITCH_LABEL);
}

fn switch_case_item(p: &mut Parser) {
    if p.at(DEFAULT_KW) {
        // `case null, default`.
        p.bump(DEFAULT_KW);
        return;
    }
    if at_pattern(p) {
        pattern(p);
        if p.at_contextual_kw("when") {
            guard(p);
        }
    } else {
        expr(p);
    }
}

fn guard(p: &mut Parser) {
    let m = p.start();
    p.bump_remap(WHEN_KW);
    expr(p);
    m.complete(p, GUARD);
}

// ===== Patterns (instanceof / switch) =====

/// Whether this is the start of a type pattern / record pattern (type followed by a binding name `IDENT` or `(`).
fn at_pattern(p: &Parser) -> bool {
    if !(p.at(IDENT) || p.at_ts(PRIMITIVE_TYPE)) {
        return false;
    }
    let Some(i) = skip_type(p, 0) else {
        return false;
    };
    matches!(p.nth_nofuel(i), IDENT | LPAREN)
}

fn pattern(p: &mut Parser) {
    let m = p.start();
    type_(p);
    if p.at(LPAREN) {
        // Record pattern: Type(subpatterns)
        p.bump(LPAREN);
        while !p.at(RPAREN) && !p.at_eof() {
            let before = p.pos();
            if at_pattern(p) {
                pattern(p);
            } else if p.at_contextual_kw("var") {
                // `var x` binding.
                let vm = p.start();
                type_(p);
                p.expect(IDENT);
                vm.complete(p, TYPE_PATTERN);
            } else {
                p.err_and_bump("expected a pattern");
            }
            if p.pos() == before {
                p.err_and_bump("unexpected token");
            }
            if !p.eat(COMMA) {
                break;
            }
        }
        p.expect(RPAREN);
        m.complete(p, RECORD_PATTERN);
    } else {
        // Type pattern: Type id
        p.expect(IDENT);
        m.complete(p, TYPE_PATTERN);
    }
}

// ===== Expressions (assignment -> ternary -> binary via precedence climbing -> unary -> postfix -> primary) =====

fn at_expr_start(p: &Parser) -> bool {
    p.at_ts(EXPR_START)
}

/// Parses an expression (entry point).
fn expr(p: &mut Parser) {
    let _ = expr_opt(p);
}

fn expr_opt(p: &mut Parser) -> Option<CompletedMarker> {
    if at_lambda(p) {
        return Some(lambda_expr(p));
    }
    assignment_expr(p)
}

/// Assignment expression (right-associative). Handles `=` / `+=` etc., including fused `>>=` / `>>>=`.
fn assignment_expr(p: &mut Parser) -> Option<CompletedMarker> {
    let lhs = ternary_expr(p)?;
    if let Some(len) = at_assign_op(p) {
        let m = lhs.precede(p);
        for _ in 0..len {
            p.bump_any();
        }
        expr(p); // right-associative: allows lambda/ternary/nested assignment.
        return Some(m.complete(p, ASSIGNMENT_EXPR));
    }
    Some(lhs)
}

/// Length (token count) of an assignment operator. `>>=` is GT GT EQ = 3, `>>>=` is 4.
fn at_assign_op(p: &Parser) -> Option<u8> {
    match p.current() {
        EQ | PLUS_EQ | MINUS_EQ | STAR_EQ | SLASH_EQ | PERCENT_EQ | AMP_EQ | PIPE_EQ | CARET_EQ
        | LSHIFT_EQ => Some(1),
        GT => {
            if p.nth_at(1, GT) && p.nth_adjacent(0) {
                if p.nth_at(2, GT) && p.nth_adjacent(1) {
                    if p.nth_at(3, EQ) && p.nth_adjacent(2) {
                        return Some(4); // >>>=
                    }
                    return None;
                }
                if p.nth_at(2, EQ) && p.nth_adjacent(1) {
                    return Some(3); // >>=
                }
            }
            None
        }
        _ => None,
    }
}

fn ternary_expr(p: &mut Parser) -> Option<CompletedMarker> {
    let cond = expr_bp(p, 0)?;
    if p.at(QUESTION) {
        let m = cond.precede(p);
        p.bump(QUESTION);
        expr(p); // then
        p.expect(COLON);
        expr(p); // else (right-associative)
        return Some(m.complete(p, TERNARY_EXPR));
    }
    Some(cond)
}

/// Parses a binary expression with minimum binding power `min_bp` (precedence climbing).
fn expr_bp(p: &mut Parser, min_bp: u8) -> Option<CompletedMarker> {
    let mut lhs = unary_expr(p)?;

    while let Some((op_bp, op_len, right_assoc)) = peek_bin_op(p) {
        if op_bp < min_bp {
            break;
        }
        let m = lhs.precede(p);
        if p.at(INSTANCEOF_KW) {
            // Right-hand side of `instanceof` is a type or pattern (`o instanceof String s`).
            p.bump(INSTANCEOF_KW);
            if at_pattern(p) {
                pattern(p);
            } else {
                type_(p);
            }
        } else {
            consume_bin_op(p, op_len);
            let next_min = if right_assoc { op_bp } else { op_bp + 1 };
            expr_bp(p, next_min);
        }
        lhs = m.complete(p, BINARY_EXPR);
    }
    Some(lhs)
}

/// Returns (binding power, token length, is right-associative) for the next binary operator, including fused `>` family.
/// Returns `None` for `>>=` / `>>>=` (assignment), deferring to the assignment layer.
fn peek_bin_op(p: &Parser) -> Option<(u8, u8, bool)> {
    let bp = match p.current() {
        PIPE_PIPE => return Some((1, 1, false)),
        AMP_AMP => return Some((2, 1, false)),
        PIPE => return Some((3, 1, false)),
        CARET => return Some((4, 1, false)),
        AMP => return Some((5, 1, false)),
        EQ_EQ | BANG_EQ => return Some((6, 1, false)),
        LT | LT_EQ | INSTANCEOF_KW => return Some((7, 1, false)),
        GT => {
            if p.nth_at(1, GT) && p.nth_adjacent(0) {
                if p.nth_at(2, GT) && p.nth_adjacent(1) {
                    if p.nth_at(3, EQ) && p.nth_adjacent(2) {
                        return None; // >>>= is assignment.
                    }
                    return Some((8, 3, false)); // >>>
                }
                if p.nth_at(2, EQ) && p.nth_adjacent(1) {
                    return None; // >>= is assignment.
                }
                return Some((8, 2, false)); // >>
            }
            if p.nth_at(1, EQ) && p.nth_adjacent(0) {
                return Some((7, 2, false)); // >=
            }
            return Some((7, 1, false)); // >
        }
        LSHIFT => 8,
        PLUS | MINUS => 9,
        STAR | SLASH | PERCENT => 10,
        _ => return None,
    };
    Some((bp, 1, false))
}

/// Consumes `len` binary operator tokens (for fused operators `>>`/`>>>`/`>=`).
fn consume_bin_op(p: &mut Parser, len: u8) {
    for _ in 0..len {
        p.bump_any();
    }
}

fn unary_expr(p: &mut Parser) -> Option<CompletedMarker> {
    if at_cast(p) {
        return Some(cast_expr(p));
    }
    match p.current() {
        BANG | TILDE | PLUS | MINUS | PLUS_PLUS | MINUS_MINUS => {
            let m = p.start();
            p.bump_any();
            unary_expr(p);
            Some(m.complete(p, UNARY_EXPR))
        }
        _ => postfix_expr(p),
    }
}

/// Whether `( Type ) operand` is a cast — fuel-free lookahead. Lambda is already disambiguated before this call.
fn at_cast(p: &Parser) -> bool {
    if !p.at(LPAREN) {
        return false;
    }
    let prim_first = PRIMITIVE_TYPE.contains(p.nth_nofuel(1));
    let Some(mut i) = skip_type(p, 1) else {
        return false;
    };
    // Intersection type `(A & B)`.
    while p.nth_nofuel(i) == AMP {
        match skip_type(p, i + 1) {
            Some(j) => i = j,
            None => return false,
        }
    }
    if p.nth_nofuel(i) != RPAREN {
        return false;
    }
    let after = p.nth_nofuel(i + 1);
    let pure_primitive = prim_first && i == 2;
    if pure_primitive {
        CAST_FOLLOW_PRIMITIVE.contains(after)
    } else {
        CAST_FOLLOW_REF.contains(after)
    }
}

fn cast_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(LPAREN);
    type_(p);
    while p.at(AMP) {
        p.bump(AMP);
        type_(p);
    }
    p.expect(RPAREN);
    unary_expr(p);
    m.complete(p, CAST_EXPR)
}

fn postfix_expr(p: &mut Parser) -> Option<CompletedMarker> {
    let mut lhs = primary_expr(p)?;
    loop {
        match p.current() {
            DOT if p.nth_at(1, CLASS_KW) => {
                let m = lhs.precede(p);
                p.bump(DOT);
                p.bump(CLASS_KW);
                lhs = m.complete(p, CLASS_LITERAL);
            }
            DOT if p.nth_at(1, IDENT) || p.nth_at(1, THIS_KW) || p.nth_at(1, SUPER_KW) => {
                let m = lhs.precede(p);
                p.bump(DOT);
                p.bump_any(); // IDENT / this / super
                lhs = m.complete(p, FIELD_ACCESS);
            }
            COLON_COLON => {
                let m = lhs.precede(p);
                p.bump(COLON_COLON);
                if p.at(LT) {
                    type_args(p);
                }
                if p.at(NEW_KW) {
                    p.bump(NEW_KW);
                } else {
                    p.expect(IDENT);
                }
                lhs = m.complete(p, METHOD_REF_EXPR);
            }
            LPAREN => {
                let m = lhs.precede(p);
                arg_list(p);
                lhs = m.complete(p, CALL_EXPR);
            }
            LBRACK => {
                let m = lhs.precede(p);
                p.bump(LBRACK);
                expr(p);
                p.expect(RBRACK);
                lhs = m.complete(p, INDEX_EXPR);
            }
            PLUS_PLUS | MINUS_MINUS => {
                let m = lhs.precede(p);
                p.bump_any();
                lhs = m.complete(p, POSTFIX_EXPR);
            }
            _ => break,
        }
    }
    Some(lhs)
}

fn primary_expr(p: &mut Parser) -> Option<CompletedMarker> {
    let cm = match p.current() {
        _ if p.at_ts(LITERAL_TOKEN) => {
            let m = p.start();
            p.bump_any();
            m.complete(p, LITERAL)
        }
        IDENT => {
            let m = p.start();
            p.bump(IDENT);
            m.complete(p, NAME_REF)
        }
        THIS_KW | SUPER_KW => {
            let m = p.start();
            p.bump_any();
            m.complete(p, NAME_REF)
        }
        LPAREN => {
            let m = p.start();
            p.bump(LPAREN);
            expr(p);
            p.expect(RPAREN);
            m.complete(p, PAREN_EXPR)
        }
        NEW_KW => new_expr(p),
        SWITCH_KW => switch_expr(p),
        _ => {
            p.err_and_bump("expected an expression");
            return None;
        }
    };
    Some(cm)
}

fn switch_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(SWITCH_KW);
    p.expect(LPAREN);
    expr(p);
    p.expect(RPAREN);
    switch_block(p);
    m.complete(p, SWITCH_EXPR)
}

fn new_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    p.bump(NEW_KW);
    type_(p);
    if p.at(LPAREN) {
        arg_list(p);
        if p.at(LBRACE) {
            // Anonymous class body.
            class_body(p);
        }
    } else if p.at(LBRACK) || at_annotated_dim(p) {
        // Array creation `new int[n]` / `new int[n][]` / `new int @A [n]`.
        while p.at(LBRACK) || at_annotated_dim(p) {
            while p.at(AT) && !p.nth_at(1, INTERFACE_KW) {
                annotation(p);
            }
            p.expect(LBRACK);
            if !p.at(RBRACK) {
                expr(p);
            }
            p.expect(RBRACK);
        }
        if p.at(LBRACE) {
            array_init(p);
        }
    } else if p.at(LBRACE) {
        // `new int[]{...}` (the type side already consumed `[]`).
        array_init(p);
    }
    m.complete(p, NEW_EXPR)
}

fn arg_list(p: &mut Parser) {
    let m = p.start();
    p.bump(LPAREN);
    while !p.at(RPAREN) && !p.at_eof() {
        let before = p.pos();
        expr(p);
        if p.pos() == before {
            p.err_and_bump("unexpected argument");
        }
        if !p.eat(COMMA) {
            break;
        }
    }
    p.expect(RPAREN);
    m.complete(p, ARG_LIST);
}

// ===== lambda =====

/// Whether this is the start of a lambda (`id ->` or `( ... ) ->`). Matches `)` using fuel-free lookahead.
fn at_lambda(p: &Parser) -> bool {
    if p.at(IDENT) && p.nth_at(1, ARROW) {
        return true;
    }
    if p.at(LPAREN) {
        let mut depth = 0i32;
        let mut i = 0usize;
        loop {
            match p.nth_nofuel(i) {
                LPAREN => depth += 1,
                RPAREN => {
                    depth -= 1;
                    if depth == 0 {
                        return p.nth_nofuel(i + 1) == ARROW;
                    }
                }
                EOF => return false,
                _ => {}
            }
            i += 1;
        }
    }
    false
}

fn lambda_expr(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    lambda_params(p);
    p.expect(ARROW);
    if p.at(LBRACE) {
        block(p);
    } else {
        expr(p);
    }
    m.complete(p, LAMBDA_EXPR)
}

fn lambda_params(p: &mut Parser) {
    let m = p.start();
    if p.at(LPAREN) {
        p.bump(LPAREN);
        while !p.at(RPAREN) && !p.at_eof() {
            let before = p.pos();
            lambda_param(p);
            if p.pos() == before {
                p.err_and_bump("unexpected argument");
            }
            if !p.eat(COMMA) {
                break;
            }
        }
        p.expect(RPAREN);
    } else {
        // Single bare identifier — wrap it in a PARAM node so the tree is uniform with the
        // parenthesized form (`(x) -> ...` and `x -> ...` both expose a PARAM).
        let pm = p.start();
        p.expect(IDENT);
        pm.complete(p, PARAM);
    }
    m.complete(p, LAMBDA_PARAMS);
}

fn lambda_param(p: &mut Parser) {
    let pm = p.start();
    if p.at(IDENT) && (p.nth_at(1, COMMA) || p.nth_at(1, RPAREN)) {
        // Bare untyped parameter.
        p.bump(IDENT);
    } else {
        // Typed parameter (including `var`).
        modifiers(p);
        type_(p);
        p.expect(IDENT);
    }
    pm.complete(p, PARAM);
}
