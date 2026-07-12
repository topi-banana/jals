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
use crate::syntax_kind::SyntaxKind::{
    ABSTRACT_KW, AMP, AMP_AMP, AMP_EQ, ANNOTATION, ANNOTATION_ARG_LIST, ANNOTATION_DEFAULT,
    ANNOTATION_PAIR, ANNOTATION_TYPE_DECL, ARG_LIST, ARRAY_INIT, ARROW, ASSERT_KW, ASSERT_STMT,
    ASSIGNMENT_EXPR, AT, BANG, BANG_EQ, BINARY_EXPR, BLOCK, BOOLEAN_KW, BREAK_KW, BREAK_STMT,
    BYTE_KW, CALL_EXPR, CARET, CARET_EQ, CASE_KW, CAST_EXPR, CATCH_CLAUSE, CATCH_KW, CHAR_KW,
    CHAR_LITERAL, CLASS_BODY, CLASS_DECL, CLASS_KW, CLASS_LITERAL, COLON, COLON_COLON, COMMA,
    CONSTRUCTOR_DECL, CONTINUE_KW, CONTINUE_STMT, DEFAULT_KW, DO_KW, DO_WHILE_STMT, DOT, DOUBLE_KW,
    ELLIPSIS, ELSE_KW, EMPTY_STMT, ENUM_BODY, ENUM_CONSTANT, ENUM_DECL, ENUM_KW, EOF, EQ, EQ_EQ,
    EXPORTS_DIRECTIVE, EXPORTS_KW, EXPR_STMT, EXTENDS_CLAUSE, EXTENDS_KW, FALSE_KW, FIELD_ACCESS,
    FIELD_DECL, FINAL_KW, FINALLY_CLAUSE, FINALLY_KW, FLOAT_KW, FLOAT_LITERAL, FOR_EACH_STMT,
    FOR_KW, FOR_STMT, GT, GUARD, IDENT, IF_KW, IF_STMT, IMPLEMENTS_CLAUSE, IMPLEMENTS_KW,
    IMPORT_DECL, IMPORT_KW, INDEX_EXPR, INITIALIZER, INSTANCEOF_KW, INT_KW, INT_LITERAL,
    INTERFACE_DECL, INTERFACE_KW, LABELED_STMT, LAMBDA_EXPR, LAMBDA_PARAMS, LBRACE, LBRACK,
    LITERAL, LOCAL_VAR_DECL, LONG_KW, LPAREN, LSHIFT, LSHIFT_EQ, LT, LT_EQ, METHOD_DECL,
    METHOD_REF_EXPR, MINUS, MINUS_EQ, MINUS_MINUS, MODIFIERS, MODULE_BODY, MODULE_DECL, MODULE_KW,
    NAME_REF, NATIVE_KW, NEW_EXPR, NEW_KW, NON_SEALED_KW, NULL_KW, OPEN_KW, OPENS_DIRECTIVE,
    OPENS_KW, PACKAGE_DECL, PACKAGE_KW, PARAM, PARAM_LIST, PAREN_EXPR, PERCENT, PERCENT_EQ,
    PERMITS_CLAUSE, PERMITS_KW, PIPE, PIPE_EQ, PIPE_PIPE, PLUS, PLUS_EQ, PLUS_PLUS, POSTFIX_EXPR,
    PRIVATE_KW, PROTECTED_KW, PROVIDES_DIRECTIVE, PROVIDES_KW, PUBLIC_KW, QUALIFIED_NAME, QUESTION,
    RBRACE, RBRACK, RECORD_COMPONENT, RECORD_DECL, RECORD_HEADER, RECORD_KW, RECORD_PATTERN,
    REQUIRES_DIRECTIVE, REQUIRES_KW, RESOURCE, RESOURCE_LIST, RETURN_KW, RETURN_STMT, RPAREN,
    SEALED_KW, SEMICOLON, SHORT_KW, SLASH, SLASH_EQ, SOURCE_FILE, STAR, STAR_EQ, STATIC_KW,
    STRICTFP_KW, STRING_LITERAL, SUPER_KW, SWITCH_BLOCK, SWITCH_EXPR, SWITCH_GROUP, SWITCH_KW,
    SWITCH_LABEL, SWITCH_RULE, SWITCH_STMT, SYNCHRONIZED_KW, SYNCHRONIZED_STMT, TERNARY_EXPR,
    TEXT_BLOCK, THIS_KW, THROW_KW, THROW_STMT, THROWS_CLAUSE, THROWS_KW, TILDE, TO_KW,
    TRANSIENT_KW, TRANSITIVE_KW, TRUE_KW, TRY_KW, TRY_STMT, TYPE, TYPE_ARGS, TYPE_PARAM,
    TYPE_PARAMS, TYPE_PATTERN, UNARY_EXPR, UNDERSCORE, UNNAMED_PATTERN, USES_DIRECTIVE, USES_KW,
    VAR_KW, VOID_KW, VOLATILE_KW, WHEN_KW, WHILE_KW, WHILE_STMT, WITH_KW, YIELD_KW, YIELD_STMT,
};

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
    // A leading `<` can only begin an explicit constructor invocation type witness,
    // `<Type>this(...)` / `<Type>super(...)` (JLS 8.8.7.1); handled in `primary_expr`.
    LT,
]);

impl Parser<'_> {
    /// Entry point. Parses a compilation unit.
    pub(crate) fn root(&mut self) {
        let m = self.start();
        if self.at(PACKAGE_KW) {
            self.package_decl();
        }
        while self.at(IMPORT_KW) {
            self.import_decl();
        }
        while !self.at_eof() {
            let before = self.pos();
            self.type_decl();
            // Progress guarantee (last-resort safeguard).
            if self.pos() == before {
                self.err_and_bump("unexpected token");
            }
        }
        m.complete(self, SOURCE_FILE);
    }

    fn package_decl(&mut self) {
        let m = self.start();
        self.bump(PACKAGE_KW);
        self.qualified_name(false);
        self.expect(SEMICOLON);
        m.complete(self, PACKAGE_DECL);
    }

    fn import_decl(&mut self) {
        let m = self.start();
        self.bump(IMPORT_KW);
        // `import module M;` (JEP 511). `module` is a restricted keyword (lexed as `IDENT`);
        // it starts a module import only when a name follows it, so `import module.foo.Bar;`
        // and `import module;` remain ordinary type imports of a package/type named `module`.
        if self.at_contextual_kw("module") && self.nth_at(1, IDENT) {
            self.bump_remap(MODULE_KW);
            self.qualified_name(false);
        } else {
            self.eat(STATIC_KW);
            self.qualified_name(true);
        }
        self.expect(SEMICOLON);
        m.complete(self, IMPORT_DECL);
    }

    /// Dotted name. If `allow_star` is true, allows a trailing `.*` (for imports).
    fn qualified_name(&mut self, allow_star: bool) {
        let m = self.start();
        self.expect(IDENT);
        while self.at(DOT) {
            if allow_star && self.nth_at(1, STAR) {
                self.bump(DOT);
                self.bump(STAR);
                break;
            }
            if !self.nth_at(1, IDENT) {
                break;
            }
            self.bump(DOT);
            self.bump(IDENT);
        }
        m.complete(self, QUALIFIED_NAME);
    }

    // ===== Type declarations =====

    /// Whether this is the start of `record Foo(...)` / `record Foo<T>(...)` (`record` is a contextual keyword).
    /// Requires `(` or `<` after the name to distinguish from variable declarations like `record r = 1;`.
    fn at_record_decl(&self) -> bool {
        self.at_contextual_kw("record")
            && self.nth_at(1, IDENT)
            && (self.nth_at(2, LPAREN) || self.nth_at(2, LT))
    }

    /// Top-level declaration: a type declaration (class / interface / enum / record /
    /// `@interface` / module) or, in a compact source file (JEP 512), a top-level field
    /// or method declaration belonging to the file's implicit class.
    fn type_decl(&mut self) {
        let m = self.start();
        self.modifiers();
        match self.current() {
            CLASS_KW => self.class_rest(m),
            INTERFACE_KW => self.interface_rest(m),
            ENUM_KW => self.enum_rest(m),
            AT if self.nth_at(1, INTERFACE_KW) => self.annotation_type_rest(m),
            _ if self.at_module_decl() => self.module_rest(m),
            _ if self.at_record_decl() => self.record_rest(m),
            // Top-level field / (generic) method in a compact source file.
            _ if self.at_type_start() || self.at(LT) => {
                if self.at(LT) {
                    // Type parameters of a generic method, e.g. `<T> T id(T x) { ... }`.
                    self.type_params();
                }
                self.field_or_method(m);
            }
            _ => {
                m.abandon(self);
                self.err_and_bump("expected a type declaration");
            }
        }
    }

    // ===== Module declarations (`module-info.java`) =====

    /// Whether this is the start of a module declaration (`module Name {` / `open module Name {`).
    /// `module` and `open` are restricted keywords (lexed as `IDENT`); at the top level a name
    /// following `module` is unambiguous because only type/module declarations appear there.
    fn at_module_decl(&self) -> bool {
        (self.at_contextual_kw("module") && self.nth_at(1, IDENT))
            || (self.at_contextual_kw("open")
                && self.nth_at(1, IDENT)
                && self.nth_text(1) == "module"
                && self.nth_at(2, IDENT))
    }

    /// Module declaration (`m` is the enclosing start marker; modifiers/annotations already consumed).
    fn module_rest(&mut self, m: Marker) {
        if self.at_contextual_kw("open") {
            self.bump_remap(OPEN_KW);
        }
        self.bump_remap(MODULE_KW);
        self.qualified_name(false);
        self.module_body();
        m.complete(self, MODULE_DECL);
    }

    /// Module body (`{ directive* }`).
    fn module_body(&mut self) {
        let m = self.start();
        if !self.expect(LBRACE) {
            m.complete(self, MODULE_BODY);
            return;
        }
        while !self.at(RBRACE) && !self.at_eof() {
            let before = self.pos();
            self.module_directive();
            if self.pos() == before {
                self.err_and_bump("unexpected token");
            }
        }
        self.expect(RBRACE);
        m.complete(self, MODULE_BODY);
    }

    /// A single module directive (`requires` / `exports` / `opens` / `uses` / `provides`).
    fn module_directive(&mut self) {
        if self.at_contextual_kw("requires") {
            self.requires_directive();
        } else if self.at_contextual_kw("exports") {
            self.exports_opens_directive(EXPORTS_KW, EXPORTS_DIRECTIVE);
        } else if self.at_contextual_kw("opens") {
            self.exports_opens_directive(OPENS_KW, OPENS_DIRECTIVE);
        } else if self.at_contextual_kw("uses") {
            self.uses_provides_directive(USES_KW, USES_DIRECTIVE);
        } else if self.at_contextual_kw("provides") {
            self.uses_provides_directive(PROVIDES_KW, PROVIDES_DIRECTIVE);
        } else {
            self.err_and_bump("expected a module directive");
        }
    }

    /// `requires {transitive | static} ModuleName ;`. `transitive` is itself a valid module name,
    /// so it is a modifier only when another name part or `static` follows.
    fn requires_directive(&mut self) {
        let m = self.start();
        self.bump_remap(REQUIRES_KW);
        loop {
            if self.at_contextual_kw("transitive")
                && (self.nth_at(1, IDENT) || self.nth_at(1, STATIC_KW))
            {
                self.bump_remap(TRANSITIVE_KW);
            } else if self.at(STATIC_KW) {
                self.bump(STATIC_KW);
            } else {
                break;
            }
        }
        self.qualified_name(false);
        self.expect(SEMICOLON);
        m.complete(self, REQUIRES_DIRECTIVE);
    }

    /// `exports PackageName [to ModuleName {, ModuleName}] ;` (and the identical `opens` form).
    fn exports_opens_directive(&mut self, kw: SyntaxKind, node: SyntaxKind) {
        let m = self.start();
        self.bump_remap(kw);
        self.qualified_name(false);
        if self.at_contextual_kw("to") {
            self.bump_remap(TO_KW);
            self.qualified_name(false);
            while self.eat(COMMA) {
                self.qualified_name(false);
            }
        }
        self.expect(SEMICOLON);
        m.complete(self, node);
    }

    /// `uses TypeName ;` and `provides TypeName with TypeName {, TypeName} ;`.
    fn uses_provides_directive(&mut self, kw: SyntaxKind, node: SyntaxKind) {
        let m = self.start();
        self.bump_remap(kw);
        self.qualified_name(false);
        if kw == PROVIDES_KW {
            if self.at_contextual_kw("with") {
                self.bump_remap(WITH_KW);
                self.qualified_name(false);
                while self.eat(COMMA) {
                    self.qualified_name(false);
                }
            } else {
                self.error("expected `with`");
            }
        }
        self.expect(SEMICOLON);
        m.complete(self, node);
    }

    /// Modifier sequence (annotations, modifier keywords, `sealed`, `non-sealed`). Always creates a node.
    fn modifiers(&mut self) {
        let m = self.start();
        loop {
            if self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
                self.annotation();
            } else if self.at_ts(MODIFIER_KW) {
                self.bump_any();
            } else if self.at_non_sealed() {
                self.non_sealed();
            } else if self.at_contextual_kw("sealed") {
                self.bump_remap(SEALED_KW);
            } else {
                break;
            }
        }
        m.complete(self, MODIFIERS);
    }

    fn annotation(&mut self) {
        let m = self.start();
        self.bump(AT);
        self.qualified_name(false);
        if self.at(LPAREN) {
            self.annotation_arg_list();
        }
        m.complete(self, ANNOTATION);
    }

    /// Annotation argument list (`(value)` / `(name = value, ...)`).
    fn annotation_arg_list(&mut self) {
        let m = self.start();
        self.bump(LPAREN);
        while !self.at(RPAREN) && !self.at_eof() {
            let before = self.pos();
            if self.at(IDENT) && self.nth_at(1, EQ) {
                let pair = self.start();
                self.bump(IDENT);
                self.bump(EQ);
                self.element_value();
                pair.complete(self, ANNOTATION_PAIR);
            } else {
                self.element_value();
            }
            if self.pos() == before {
                self.err_and_bump("unexpected argument");
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(RPAREN);
        m.complete(self, ANNOTATION_ARG_LIST);
    }

    /// Annotation element value or array initializer element (expression / nested annotation / array).
    fn element_value(&mut self) {
        if self.at(LBRACE) {
            self.array_init();
        } else if self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
            self.annotation();
        } else {
            self.expr();
        }
    }

    /// Array initializer `{ a, b, c }` (nested, trailing comma allowed).
    fn array_init(&mut self) {
        let m = self.start();
        self.bump(LBRACE);
        while !self.at(RBRACE) && !self.at_eof() {
            let before = self.pos();
            self.element_value();
            if self.pos() == before {
                self.err_and_bump("unexpected element");
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(RBRACE);
        m.complete(self, ARRAY_INIT);
    }

    /// Detects `non-sealed` (`IDENT("non") MINUS IDENT("sealed")` adjacent).
    fn at_non_sealed(&self) -> bool {
        self.at_contextual_kw("non")
            && self.nth_at(1, MINUS)
            && self.nth_adjacent(0)
            && self.nth_adjacent(1)
            && self.nth_at(2, IDENT)
            && self.nth_text(2) == "sealed"
    }

    /// Re-combines `non-sealed` into a single `NON_SEALED_KW` node.
    fn non_sealed(&mut self) {
        let m = self.start();
        self.bump_any(); // non
        self.bump_any(); // -
        self.bump_any(); // sealed
        m.complete(self, NON_SEALED_KW);
    }

    /// After `class` (modifiers already consumed by the caller, `m` is the enclosing start marker).
    fn class_rest(&mut self, m: Marker) {
        self.bump(CLASS_KW);
        self.expect(IDENT);
        if self.at(LT) {
            self.type_params();
        }
        if self.at(EXTENDS_KW) {
            self.extends_clause(false);
        }
        if self.at(IMPLEMENTS_KW) {
            self.implements_clause();
        }
        if self.at_contextual_kw("permits") {
            self.permits_clause();
        }
        self.class_body();
        m.complete(self, CLASS_DECL);
    }

    /// After `interface`.
    fn interface_rest(&mut self, m: Marker) {
        self.bump(INTERFACE_KW);
        self.expect(IDENT);
        if self.at(LT) {
            self.type_params();
        }
        if self.at(EXTENDS_KW) {
            // interfaces can extend multiple types.
            self.extends_clause(true);
        }
        if self.at_contextual_kw("permits") {
            self.permits_clause();
        }
        self.class_body();
        m.complete(self, INTERFACE_DECL);
    }

    /// After `enum`.
    fn enum_rest(&mut self, m: Marker) {
        self.bump(ENUM_KW);
        self.expect(IDENT);
        if self.at(IMPLEMENTS_KW) {
            self.implements_clause();
        }
        self.enum_body();
        m.complete(self, ENUM_DECL);
    }

    fn enum_body(&mut self) {
        let m = self.start();
        if !self.expect(LBRACE) {
            m.complete(self, ENUM_BODY);
            return;
        }
        // Constant list (up to `;` or `}`).
        while !self.at(RBRACE) && !self.at(SEMICOLON) && !self.at_eof() {
            let before = self.pos();
            if self.at(IDENT) || self.at(AT) {
                self.enum_constant();
            } else {
                self.err_and_bump("expected an enum constant");
            }
            if self.pos() == before {
                self.err_and_bump("unexpected token");
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        // Optional `;` followed by members.
        if self.eat(SEMICOLON) {
            while !self.at(RBRACE) && !self.at_eof() {
                let before = self.pos();
                self.member();
                if self.pos() == before {
                    self.err_and_bump("unexpected token");
                }
            }
        }
        self.expect(RBRACE);
        m.complete(self, ENUM_BODY);
    }

    fn enum_constant(&mut self) {
        let m = self.start();
        while self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
            self.annotation();
        }
        self.expect(IDENT);
        if self.at(LPAREN) {
            self.arg_list();
        }
        if self.at(LBRACE) {
            // Class body specific to this constant.
            self.class_body();
        }
        m.complete(self, ENUM_CONSTANT);
    }

    /// After `record`.
    fn record_rest(&mut self, m: Marker) {
        self.bump_remap(RECORD_KW);
        self.expect(IDENT);
        if self.at(LT) {
            self.type_params();
        }
        self.record_header();
        if self.at(IMPLEMENTS_KW) {
            self.implements_clause();
        }
        self.class_body();
        m.complete(self, RECORD_DECL);
    }

    fn record_header(&mut self) {
        let m = self.start();
        if !self.expect(LPAREN) {
            m.complete(self, RECORD_HEADER);
            return;
        }
        while !self.at(RPAREN) && !self.at_eof() {
            let comp = self.start();
            self.modifiers();
            self.type_();
            self.eat_varargs();
            self.expect(IDENT);
            comp.complete(self, RECORD_COMPONENT);
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(RPAREN);
        m.complete(self, RECORD_HEADER);
    }

    /// After `@interface` (annotation type declaration).
    fn annotation_type_rest(&mut self, m: Marker) {
        self.bump(AT);
        self.bump(INTERFACE_KW);
        self.expect(IDENT);
        self.class_body();
        m.complete(self, ANNOTATION_TYPE_DECL);
    }

    fn extends_clause(&mut self, multi: bool) {
        let c = self.start();
        self.bump(EXTENDS_KW);
        self.type_();
        if multi {
            while self.eat(COMMA) {
                self.type_();
            }
        }
        c.complete(self, EXTENDS_CLAUSE);
    }

    fn implements_clause(&mut self) {
        let c = self.start();
        self.bump(IMPLEMENTS_KW);
        self.type_();
        while self.eat(COMMA) {
            self.type_();
        }
        c.complete(self, IMPLEMENTS_CLAUSE);
    }

    fn permits_clause(&mut self) {
        let c = self.start();
        self.bump_remap(PERMITS_KW);
        self.type_();
        while self.eat(COMMA) {
            self.type_();
        }
        c.complete(self, PERMITS_CLAUSE);
    }

    fn type_params(&mut self) {
        let m = self.start();
        self.bump(LT);
        while !self.at(GT) && !self.at_eof() {
            let tp = self.start();
            // Type parameters may also carry annotations.
            while self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
                self.annotation();
            }
            self.expect(IDENT);
            if self.at(EXTENDS_KW) {
                self.bump(EXTENDS_KW);
                self.type_();
                while self.at(AMP) {
                    self.bump(AMP);
                    self.type_();
                }
            }
            tp.complete(self, TYPE_PARAM);
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect_gt();
        m.complete(self, TYPE_PARAMS);
    }

    fn class_body(&mut self) {
        let m = self.start();
        if !self.expect(LBRACE) {
            m.complete(self, CLASS_BODY);
            return;
        }
        while !self.at(RBRACE) && !self.at_eof() {
            let before = self.pos();
            self.member();
            // Progress guarantee: if member consumed no tokens, force-wrap one token as ERROR.
            if self.pos() == before {
                self.err_and_bump("unexpected token");
            }
        }
        self.expect(RBRACE);
        m.complete(self, CLASS_BODY);
    }

    /// Member of class / interface / enum / `@interface`.
    fn member(&mut self) {
        if self.at(SEMICOLON) {
            // Empty member.
            self.bump(SEMICOLON);
            return;
        }
        let m = self.start();
        self.modifiers();

        // Nested type declaration.
        match self.current() {
            CLASS_KW => return self.class_rest(m),
            INTERFACE_KW => return self.interface_rest(m),
            ENUM_KW => return self.enum_rest(m),
            AT if self.nth_at(1, INTERFACE_KW) => return self.annotation_type_rest(m),
            _ => {}
        }
        if self.at_record_decl() {
            return self.record_rest(m);
        }

        // Initializer block (`{ ... }` / `static { ... }`).
        if self.at(LBRACE) {
            self.block();
            m.complete(self, INITIALIZER);
            return;
        }

        // Type arguments for generic methods/constructors.
        if self.at(LT) {
            self.type_params();
        }

        // Compact canonical constructor (record): `Name { ... }`.
        if self.at(IDENT) && self.nth_at(1, LBRACE) {
            self.bump(IDENT);
            self.block();
            m.complete(self, CONSTRUCTOR_DECL);
            return;
        }

        // Constructor: `Name ( ... )`.
        if self.at(IDENT) && self.nth_at(1, LPAREN) {
            self.bump(IDENT);
            self.param_list();
            if self.at(THROWS_KW) {
                self.throws_clause();
            }
            if self.at(LBRACE) {
                self.block();
            } else {
                self.expect(SEMICOLON);
            }
            m.complete(self, CONSTRUCTOR_DECL);
            return;
        }

        // Otherwise starts with a type (field or method).
        self.field_or_method(m);
    }

    /// Parses a field or method declaration, given a marker `m` started before the
    /// modifiers were consumed. The current position is just past the modifiers (and any
    /// leading method type parameters). Shared by class members and top-level members
    /// (JEP 512 compact source files).
    fn field_or_method(&mut self, m: Marker) {
        if !self.at_type_start() {
            m.abandon(self);
            self.err_recover("expected a member declaration", MEMBER_RECOVERY);
            return;
        }
        self.type_();
        self.expect(IDENT);
        if self.at(LPAREN) {
            // Method (including annotation elements).
            self.param_list();
            // Old-style return-type array dimensions `m()[]` (each optionally annotated).
            self.dims();
            if self.at(THROWS_KW) {
                self.throws_clause();
            }
            if self.at(DEFAULT_KW) {
                // Default value for annotation element.
                let d = self.start();
                self.bump(DEFAULT_KW);
                self.element_value();
                d.complete(self, ANNOTATION_DEFAULT);
            }
            if self.at(LBRACE) {
                self.block();
            } else {
                self.expect(SEMICOLON);
            }
            m.complete(self, METHOD_DECL);
        } else {
            // Field (supports multiple declarators, array dimensions, and array initializers).
            self.field_tail();
            self.expect(SEMICOLON);
            m.complete(self, FIELD_DECL);
        }
    }

    /// Remainder of a field/local variable declarator (the first name is already consumed).
    fn field_tail(&mut self) {
        self.dims();
        if self.eat(EQ) {
            self.var_init();
        }
        while self.eat(COMMA) {
            self.binding_name();
            self.dims();
            if self.eat(EQ) {
                self.var_init();
            }
        }
    }

    /// Skips a sequence of array dimensions (`[]`), each optionally annotated (`String @A []`).
    fn dims(&mut self) {
        loop {
            // An annotation here belongs to a dimension only if `[]` follows it
            // (`String @A []`); otherwise leave it for whatever comes next.
            if self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
                let i = self.skip_annotations_lookahead(0);
                if self.nth_nofuel(i) == LBRACK && self.nth_nofuel(i + 1) == RBRACK {
                    while self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
                        self.annotation();
                    }
                    // The lookahead promised `[]` after the annotations, but a malformed
                    // annotation argument list can make the real parse stop short of it
                    // (`String @A(x 0) []` leaves the parser at `0`), so guard the bump:
                    // its lookahead-vs-parse divergence must never panic.
                    if self.at(LBRACK) && self.nth_at(1, RBRACK) {
                        self.bump(LBRACK);
                        self.bump(RBRACK);
                        continue;
                    }
                }
                break;
            }
            if self.at(LBRACK) && self.nth_at(1, RBRACK) {
                self.bump(LBRACK);
                self.bump(RBRACK);
                continue;
            }
            break;
        }
    }

    /// Variable initializer (array initializer `{...}` or an expression).
    fn var_init(&mut self) {
        if self.at(LBRACE) {
            self.array_init();
        } else {
            self.expr();
        }
    }

    fn throws_clause(&mut self) {
        let m = self.start();
        self.bump(THROWS_KW);
        self.type_();
        while self.eat(COMMA) {
            self.type_();
        }
        m.complete(self, THROWS_CLAUSE);
    }

    /// Eats an optional varargs `...`, consuming any type-use annotations on the varargs element
    /// type that `type_` left behind first (`Object @A...`, `String @A [] @B ...`). Such an
    /// annotation is not followed by `[]`, so the array-only `dims` leaves it for this caller.
    fn eat_varargs(&mut self) {
        if self.nth_nofuel(self.skip_annotations_lookahead(0)) == ELLIPSIS {
            while self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
                self.annotation();
            }
        }
        self.eat(ELLIPSIS);
    }

    fn param_list(&mut self) {
        let m = self.start();
        self.bump(LPAREN);
        while !self.at(RPAREN) && !self.at_eof() {
            let param = self.start();
            self.modifiers();
            self.type_();
            self.eat_varargs(); // varargs.
            // Also allows a `this` receiver parameter (`Foo this`).
            if self.at(THIS_KW) {
                self.bump(THIS_KW);
            } else {
                self.expect(IDENT);
                self.dims();
            }
            param.complete(self, PARAM);
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(RPAREN);
        m.complete(self, PARAM_LIST);
    }

    // ===== Types =====

    /// Whether this can begin a type. A leading type-use annotation (`@A String`) counts, so a
    /// member return type carrying one after the type parameters (`<T> @A String m()`) is recognized.
    fn at_type_start(&self) -> bool {
        self.at_ts(PRIMITIVE_TYPE)
            || self.at(IDENT)
            || self.at_contextual_kw("var")
            || (self.at(AT) && !self.nth_at(1, INTERFACE_KW))
    }

    /// Whether a `.` at the current position continues a qualified type: the token after the `.`
    /// (skipping any type-use annotations) is an `IDENT` (`Outer.Inner`, `Outer.@A Inner`).
    fn dot_continues_type(&self) -> bool {
        self.at(DOT) && self.nth_nofuel(self.skip_annotations_lookahead(1)) == IDENT
    }

    fn type_(&mut self) {
        let m = self.start();
        // Annotations on types.
        while self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
            self.annotation();
        }
        if self.at_contextual_kw("var") {
            self.bump_remap(VAR_KW);
        } else if self.at_ts(PRIMITIVE_TYPE) {
            self.bump_any();
        } else {
            // Reference type: name + optional type arguments + dotted continuation.
            self.expect(IDENT);
            if self.at(LT) {
                self.type_args();
            }
            while self.dot_continues_type() {
                self.bump(DOT);
                // Annotations on the inner type (`Outer.@A Inner`).
                while self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
                    self.annotation();
                }
                // `dot_continues_type`'s lookahead promised an `IDENT` past the annotations, but a
                // malformed annotation argument list (`Outer.@A(x y) Inner`) can make the real parse
                // stop short of it, so `expect` (not `bump`) the inner name to stay panic-free.
                self.expect(IDENT);
                if self.at(LT) {
                    self.type_args();
                }
            }
        }
        self.dims();
        m.complete(self, TYPE);
    }

    fn type_args(&mut self) {
        let m = self.start();
        self.bump(LT);
        if !self.at(GT) {
            self.type_arg();
            while self.eat(COMMA) {
                self.type_arg();
            }
        }
        self.expect_gt();
        m.complete(self, TYPE_ARGS);
    }

    fn type_arg(&mut self) {
        // A wildcard may carry leading type-use annotations: `@A ?`, `@A ? extends T`. Annotated
        // non-wildcard arguments (`@A Foo`) are handled by `type_`'s own leading-annotation loop.
        if self.nth_nofuel(self.skip_annotations_lookahead(0)) == QUESTION {
            while self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
                self.annotation();
            }
            // The lookahead promised a `?` past the annotations, but a malformed annotation argument
            // list (`@A(x y) ?`) can make the real parse stop short of it, so `expect` (not `bump`)
            // the wildcard `?` to stay panic-free. `? extends T` / `? super T` follow.
            self.expect(QUESTION);
            if self.at(EXTENDS_KW) || self.at(SUPER_KW) {
                self.bump_any();
                self.type_();
            }
        } else {
            self.type_();
        }
    }

    /// Consumes one `>` that closes a type argument/type parameter. `>>` and similar are
    /// represented as adjacent `GT` tokens, so this always consumes only a single `GT` (the rest is consumed by the outer caller).
    fn expect_gt(&mut self) {
        if !self.eat(GT) {
            self.error("expected `>`");
        }
    }

    /// Skips one type starting at `start` using fuel-free lookahead, returning the offset immediately after it.
    /// Returns `None` if the tokens cannot be interpreted as a type. Used for lambda/cast/pattern/local variable disambiguation.
    fn skip_type(&self, start: usize) -> Option<usize> {
        // Skip leading type-use annotations (`(@A Long) x`).
        let mut i = self.skip_annotations_lookahead(start);
        if PRIMITIVE_TYPE.contains(self.nth_nofuel(i)) {
            i += 1;
        } else if self.nth_nofuel(i) == IDENT {
            i += 1;
            loop {
                if self.nth_nofuel(i) == LT {
                    // Skip a balanced `<...>` (`>` is a single GT, `>>` is two GTs).
                    let mut depth = 0i32;
                    loop {
                        match self.nth_nofuel(i) {
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
                // Dotted continuation, including inner-type annotations (`Outer.@A Inner`).
                let after = self.skip_annotations_lookahead(i + 1);
                if self.nth_nofuel(i) == DOT && self.nth_nofuel(after) == IDENT {
                    i = after + 1;
                    continue;
                }
                break;
            }
        } else {
            return None;
        }
        Some(self.skip_annotated_bracket_pairs(i))
    }

    /// Skips a run of plain `[` `]` pairs starting at `start` using fuel-free lookahead,
    /// returning the offset immediately after them.
    fn skip_bracket_pairs(&self, start: usize) -> usize {
        let mut i = start;
        while self.nth_nofuel(i) == LBRACK && self.nth_nofuel(i + 1) == RBRACK {
            i += 2;
        }
        i
    }

    /// Like [`Self::skip_bracket_pairs`], but also skips type-use annotations preceding each `[]`
    /// (`String @A [] @B []`). Used by [`Self::skip_type`] so cast/declaration disambiguation handles
    /// annotated array dimensions; the plain `skip_bracket_pairs` is left unchanged for the
    /// expression-context callers, where annotated dimensions cannot appear.
    fn skip_annotated_bracket_pairs(&self, start: usize) -> usize {
        let mut i = start;
        loop {
            let after = self.skip_annotations_lookahead(i);
            if self.nth_nofuel(after) == LBRACK && self.nth_nofuel(after + 1) == RBRACK {
                i = after + 2;
                continue;
            }
            break;
        }
        i
    }

    // ===== Statements =====

    fn block(&mut self) {
        let m = self.start();
        self.bump(LBRACE);
        while !self.at(RBRACE) && !self.at_eof() {
            let before = self.pos();
            self.stmt();
            // Progress guarantee (last-resort safeguard).
            if self.pos() == before {
                self.err_and_bump("unexpected token");
            }
        }
        self.expect(RBRACE);
        m.complete(self, BLOCK);
    }

    fn stmt(&mut self) {
        match self.current() {
            LBRACE => self.block(),
            SEMICOLON => {
                let m = self.start();
                self.bump(SEMICOLON);
                m.complete(self, EMPTY_STMT);
            }
            IF_KW => self.if_stmt(),
            WHILE_KW => self.while_stmt(),
            DO_KW => self.do_while_stmt(),
            FOR_KW => self.for_stmt(),
            RETURN_KW => self.return_stmt(),
            THROW_KW => self.throw_stmt(),
            BREAK_KW => self.break_or_continue(BREAK_KW, BREAK_STMT),
            CONTINUE_KW => self.break_or_continue(CONTINUE_KW, CONTINUE_STMT),
            ASSERT_KW => self.assert_stmt(),
            SYNCHRONIZED_KW => self.synchronized_stmt(),
            TRY_KW => self.try_stmt(),
            SWITCH_KW => self.switch_stmt(),
            CLASS_KW | INTERFACE_KW | ENUM_KW => self.type_decl(),
            AT if self.nth_at(1, INTERFACE_KW) => self.type_decl(),
            _ => {
                // Labeled statement (`label:`). Distinguishable from ternary `?:` by the absence of `?`.
                if self.at(IDENT) && self.nth_at(1, COLON) {
                    return self.labeled_stmt();
                }
                // Local record declaration.
                if self.at_record_decl() {
                    return self.type_decl();
                }
                // yield statement (inside a switch expression).
                if self.at_yield_stmt() {
                    return self.yield_stmt();
                }
                // Local type declaration with modifiers/annotations.
                if (self.at_ts(MODIFIER_KW) || self.at(AT)) && self.at_local_type_decl() {
                    return self.type_decl();
                }
                if self.at_local_var_decl() {
                    self.local_var_decl();
                } else if self.at_expr_start() {
                    let m = self.start();
                    self.expr();
                    self.expect(SEMICOLON);
                    m.complete(self, EXPR_STMT);
                } else {
                    self.err_recover("expected a statement", STMT_RECOVERY);
                }
            }
        }
    }

    fn labeled_stmt(&mut self) {
        let m = self.start();
        self.bump(IDENT);
        self.bump(COLON);
        self.stmt();
        m.complete(self, LABELED_STMT);
    }

    fn return_stmt(&mut self) {
        let m = self.start();
        self.bump(RETURN_KW);
        if !self.at(SEMICOLON) {
            self.expr();
        }
        self.expect(SEMICOLON);
        m.complete(self, RETURN_STMT);
    }

    fn throw_stmt(&mut self) {
        let m = self.start();
        self.bump(THROW_KW);
        self.expr();
        self.expect(SEMICOLON);
        m.complete(self, THROW_STMT);
    }

    fn break_or_continue(&mut self, kw: SyntaxKind, node: SyntaxKind) {
        let m = self.start();
        self.bump(kw);
        if self.at(IDENT) {
            self.bump(IDENT); // label.
        }
        self.expect(SEMICOLON);
        m.complete(self, node);
    }

    fn assert_stmt(&mut self) {
        let m = self.start();
        self.bump(ASSERT_KW);
        self.expr();
        if self.eat(COLON) {
            self.expr();
        }
        self.expect(SEMICOLON);
        m.complete(self, ASSERT_STMT);
    }

    fn synchronized_stmt(&mut self) {
        let m = self.start();
        self.bump(SYNCHRONIZED_KW);
        self.expect(LPAREN);
        self.expr();
        self.expect(RPAREN);
        if self.at(LBRACE) {
            self.block();
        }
        m.complete(self, SYNCHRONIZED_STMT);
    }

    fn yield_stmt(&mut self) {
        let m = self.start();
        self.bump_remap(YIELD_KW);
        self.expr();
        self.expect(SEMICOLON);
        m.complete(self, YIELD_STMT);
    }

    fn try_stmt(&mut self) {
        let m = self.start();
        self.bump(TRY_KW);
        if self.at(LPAREN) {
            self.resource_list();
        }
        if self.at(LBRACE) {
            self.block();
        }
        while self.at(CATCH_KW) {
            self.catch_clause();
        }
        if self.at(FINALLY_KW) {
            self.finally_clause();
        }
        m.complete(self, TRY_STMT);
    }

    fn resource_list(&mut self) {
        let m = self.start();
        self.bump(LPAREN);
        while !self.at(RPAREN) && !self.at_eof() {
            let before = self.pos();
            self.resource();
            if self.pos() == before {
                self.err_and_bump("unexpected token");
            }
            if !self.eat(SEMICOLON) {
                break;
            }
        }
        self.expect(RPAREN);
        m.complete(self, RESOURCE_LIST);
    }

    fn resource(&mut self) {
        let m = self.start();
        if self.at_local_var_decl() {
            // Resource variable declaration: {modifiers} Type id = expr
            self.modifiers();
            self.type_();
            self.binding_name();
            self.expect(EQ);
        }
        // Reference to an existing variable (Java 9+).
        self.expr();
        m.complete(self, RESOURCE);
    }

    fn catch_clause(&mut self) {
        let m = self.start();
        self.bump(CATCH_KW);
        self.expect(LPAREN);
        self.modifiers();
        self.type_();
        while self.at(PIPE) {
            // Multi-catch `A | B`.
            self.bump(PIPE);
            self.type_();
        }
        self.binding_name();
        self.expect(RPAREN);
        if self.at(LBRACE) {
            self.block();
        }
        m.complete(self, CATCH_CLAUSE);
    }

    fn finally_clause(&mut self) {
        let m = self.start();
        self.bump(FINALLY_KW);
        if self.at(LBRACE) {
            self.block();
        }
        m.complete(self, FINALLY_CLAUSE);
    }

    /// Whether to treat `yield` as a statement. Uses javac's token-based disambiguation: a
    /// statement-leading `yield` followed by an unambiguous expression start is a yield statement.
    /// `yield = 3;` (variable) and `yield++;` (postfix on a variable) fall through to an expression
    /// statement, as does a method call `yield(...)` (yield used as a method name).
    fn at_yield_stmt(&self) -> bool {
        if !self.at_contextual_kw("yield") {
            return false;
        }
        match self.nth_nofuel(1) {
            IDENT | INT_LITERAL | FLOAT_LITERAL | CHAR_LITERAL | STRING_LITERAL | TEXT_BLOCK
            | TRUE_KW | FALSE_KW | NULL_KW | BANG | TILDE | PLUS | MINUS | NEW_KW | THIS_KW
            | SUPER_KW | SWITCH_KW => true,
            // `yield ++i;` yields a pre-increment, but `yield++;` is a postfix increment of a
            // variable named `yield`. Disambiguate on the token after `++`/`--` (yield statement
            // unless it is `;`), matching javac.
            PLUS_PLUS | MINUS_MINUS => self.nth_nofuel(2) != SEMICOLON,
            // `yield (expr)`, a cast `yield (T) e`, and a lambda `yield () -> e` are yield
            // statements; only an argument-list-shaped `yield()` / `yield(a, b)` is a method call.
            LPAREN => !self.at_yield_method_call(),
            _ => false,
        }
    }

    /// For a statement-leading `yield (`, decides whether the parenthesized run is a method-call
    /// argument list (yield used as a method name) rather than a single yielded expression. It is a
    /// method call when the list is empty (`yield()`) or holds a top-level comma (`yield(a, b)`) —
    /// neither is a valid single expression — and is not a lambda parameter list (the matching `)`
    /// is not followed by `->`). A single parenthesized expression, a cast (`yield (T) e`), and a
    /// lambda (`yield () -> e`) stay yield statements, matching javac.
    fn at_yield_method_call(&self) -> bool {
        debug_assert_eq!(self.nth_nofuel(1), LPAREN);
        let mut paren = 0i32;
        // Generic type-argument depth, so a comma inside `(Map<A, B>) e` is not a top-level arg
        // separator. `>` is always a single `GT`; clamp at 0 so a stray relational `>` cannot
        // hide a genuine top-level comma.
        let mut angle = 0i32;
        let mut comma = false;
        let mut i = 1;
        loop {
            match self.nth_nofuel(i) {
                LPAREN => paren += 1,
                RPAREN => {
                    paren -= 1;
                    if paren == 0 {
                        let empty = i == 2; // `(` immediately followed by `)`
                        return (empty || comma) && self.nth_nofuel(i + 1) != ARROW;
                    }
                }
                LT => angle += 1,
                GT => angle = (angle - 1).max(0),
                COMMA if paren == 1 && angle == 0 => comma = true,
                EOF => return false, // unbalanced; let the yield-statement path report the error
                _ => {}
            }
            i += 1;
        }
    }

    fn if_stmt(&mut self) {
        let m = self.start();
        self.bump(IF_KW);
        self.expect(LPAREN);
        self.expr();
        self.expect(RPAREN);
        self.stmt();
        if self.at(ELSE_KW) {
            self.bump(ELSE_KW);
            self.stmt();
        }
        m.complete(self, IF_STMT);
    }

    fn while_stmt(&mut self) {
        let m = self.start();
        self.bump(WHILE_KW);
        self.expect(LPAREN);
        self.expr();
        self.expect(RPAREN);
        self.stmt();
        m.complete(self, WHILE_STMT);
    }

    fn do_while_stmt(&mut self) {
        let m = self.start();
        self.bump(DO_KW);
        self.stmt();
        self.expect(WHILE_KW);
        self.expect(LPAREN);
        self.expr();
        self.expect(RPAREN);
        self.expect(SEMICOLON);
        m.complete(self, DO_WHILE_STMT);
    }

    fn for_stmt(&mut self) {
        let m = self.start();
        self.bump(FOR_KW);
        self.expect(LPAREN);
        if self.at_for_each() {
            // for-each: {modifiers} Type id : expr
            self.modifiers();
            self.type_();
            self.binding_name();
            self.expect(COLON);
            self.expr();
            self.expect(RPAREN);
            self.stmt();
            m.complete(self, FOR_EACH_STMT);
        } else {
            // C-style for: init ; cond ; update
            self.for_init();
            self.expect(SEMICOLON);
            if !self.at(SEMICOLON) {
                self.expr();
            }
            self.expect(SEMICOLON);
            if !self.at(RPAREN) {
                self.expr();
                while self.eat(COMMA) {
                    self.expr();
                }
            }
            self.expect(RPAREN);
            self.stmt();
            m.complete(self, FOR_STMT);
        }
    }

    /// Whether the for header is a for-each (`:` appears at depth 0 before `;`) — fuel-free lookahead.
    fn at_for_each(&self) -> bool {
        let mut depth = 0i32;
        let mut ternary = 0i32;
        let mut angle = 0i32;
        let mut i = 0usize;
        loop {
            match self.nth_nofuel(i) {
                LPAREN | LBRACK | LBRACE => depth += 1,
                RPAREN | RBRACK | RBRACE => {
                    if depth == 0 {
                        return false; // End of the header: no `:` found.
                    }
                    depth -= 1;
                }
                // Track generic `<...>` nesting so a wildcard `?` (e.g. `<? extends T>`) is not
                // mistaken for a ternary `?`, nor a closing `>` for the for-each separator search.
                // A bare `GT` at `angle == 0` is a comparison/shift `>`, so it is ignored.
                LT => angle += 1,
                GT if angle > 0 => angle -= 1,
                SEMICOLON if depth == 0 && ternary == 0 => return false,
                QUESTION if depth == 0 && angle == 0 => ternary += 1,
                COLON if depth == 0 && angle == 0 => {
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

    fn for_init(&mut self) {
        if self.at(SEMICOLON) {
            return; // empty.
        }
        if (self.at_ts(MODIFIER_KW) || self.at(AT)) && self.at_local_type_decl() {
            // A local type in for-init is unusual, but treat it as a declaration if it appears.
            self.type_decl();
            return;
        }
        if self.at_local_var_decl() {
            let m = self.start();
            self.var_decl_inner();
            m.complete(self, LOCAL_VAR_DECL);
        } else {
            self.expr();
            while self.eat(COMMA) {
                self.expr();
            }
        }
    }

    /// Whether this is the start of a local variable declaration.
    fn at_local_var_decl(&self) -> bool {
        if self.at_ts(MODIFIER_KW) {
            return true; // final etc. (local types are filtered out by the caller first).
        }
        if self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
            return true; // Annotated local variable.
        }
        if self.at_contextual_kw("var") {
            return true;
        }
        if self.at_ts(PRIMITIVE_TYPE) {
            // `int.class` / `int[].class` / `int[]::new` starts an expression, not a declaration.
            return !self.at_primitive_class_literal_or_method_ref();
        }
        if self.at(IDENT) {
            // Type + binding name (`Foo x` / `List<T> x` / `a.B c` / `int[] a` / `Lock _`).
            if let Some(i) = self.skip_type(0) {
                return matches!(self.nth_nofuel(i), IDENT | UNDERSCORE);
            }
        }
        false
    }

    /// Whether the token after skipping modifiers/annotations is a type declaration keyword — fuel-free lookahead.
    fn at_local_type_decl(&self) -> bool {
        let i = self.skip_modifiers_lookahead(0);
        if i == 0 {
            return false;
        }
        matches!(self.nth_nofuel(i), CLASS_KW | INTERFACE_KW | ENUM_KW)
            || (self.nth_nofuel(i) == AT && self.nth_nofuel(i + 1) == INTERFACE_KW)
    }

    /// Skips modifier keywords and annotations (including `@Name(...)`) and returns the next offset.
    fn skip_modifiers_lookahead(&self, start: usize) -> usize {
        let mut i = start;
        loop {
            let k = self.nth_nofuel(i);
            if MODIFIER_KW.contains(k) {
                i += 1;
                continue;
            }
            if k == AT && self.nth_nofuel(i + 1) != INTERFACE_KW {
                i = self.skip_one_annotation_lookahead(i);
                continue;
            }
            break;
        }
        i
    }

    /// Skips a run of annotations (each not `@interface`) starting at `start`, returning the next offset.
    fn skip_annotations_lookahead(&self, start: usize) -> usize {
        let mut i = start;
        while self.nth_nofuel(i) == AT && self.nth_nofuel(i + 1) != INTERFACE_KW {
            i = self.skip_one_annotation_lookahead(i);
        }
        i
    }

    /// Skips a single annotation (`@Name`, `@a.b.Name`, or `@Name(...)`) whose `@` is at offset `i`,
    /// returning the offset just past it.
    fn skip_one_annotation_lookahead(&self, i: usize) -> usize {
        let mut i = i + 1; // `@`
        if self.nth_nofuel(i) == IDENT {
            i += 1;
        }
        while self.nth_nofuel(i) == DOT && self.nth_nofuel(i + 1) == IDENT {
            i += 2;
        }
        if self.nth_nofuel(i) == LPAREN {
            let mut depth = 0i32;
            loop {
                match self.nth_nofuel(i) {
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
    fn at_annotated_dim(&self) -> bool {
        self.at(AT)
            && !self.nth_at(1, INTERFACE_KW)
            && self.nth_nofuel(self.skip_annotations_lookahead(0)) == LBRACK
    }

    fn local_var_decl(&mut self) {
        let m = self.start();
        self.var_decl_inner();
        self.expect(SEMICOLON);
        m.complete(self, LOCAL_VAR_DECL);
    }

    /// Body of a local variable declaration (does not consume `;`). Also used in for-init.
    fn var_decl_inner(&mut self) {
        self.modifiers();
        self.type_();
        self.binding_name();
        self.field_tail();
    }

    // ===== switch (shared body for statement and expression) =====

    fn switch_stmt(&mut self) {
        let m = self.start();
        self.bump(SWITCH_KW);
        self.expect(LPAREN);
        self.expr();
        self.expect(RPAREN);
        self.switch_block();
        m.complete(self, SWITCH_STMT);
    }

    fn switch_block(&mut self) {
        let m = self.start();
        if !self.expect(LBRACE) {
            m.complete(self, SWITCH_BLOCK);
            return;
        }
        while !self.at(RBRACE) && !self.at_eof() {
            let before = self.pos();
            self.switch_entry();
            if self.pos() == before {
                self.err_and_bump("unexpected token");
            }
        }
        self.expect(RBRACE);
        m.complete(self, SWITCH_BLOCK);
    }

    fn switch_entry(&mut self) {
        if !(self.at(CASE_KW) || self.at(DEFAULT_KW)) {
            self.err_and_bump("expected `case` or `default`");
            return;
        }
        if self.label_is_arrow() {
            // Arrow rule: label -> (block | throw | expr ;)
            let m = self.start();
            self.switch_label();
            self.expect(ARROW);
            if self.at(LBRACE) {
                self.block();
            } else if self.at(THROW_KW) {
                self.throw_stmt();
            } else {
                self.expr();
                self.expect(SEMICOLON);
            }
            m.complete(self, SWITCH_RULE);
        } else {
            // Colon group: label: (label:)* statements
            let m = self.start();
            self.switch_label();
            self.expect(COLON);
            while self.at(CASE_KW) || self.at(DEFAULT_KW) {
                self.switch_label();
                self.expect(COLON);
            }
            while !self.at(RBRACE) && !self.at(CASE_KW) && !self.at(DEFAULT_KW) && !self.at_eof() {
                let before = self.pos();
                self.stmt();
                if self.pos() == before {
                    break;
                }
            }
            m.complete(self, SWITCH_GROUP);
        }
    }

    /// Whether this label is in arrow form (`->` appears at depth 0 before `:`). Fuel-free lookahead.
    fn label_is_arrow(&self) -> bool {
        let mut depth = 0i32;
        let mut ternary = 0i32;
        let mut i = 0usize;
        loop {
            match self.nth_nofuel(i) {
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

    fn switch_label(&mut self) {
        let m = self.start();
        if self.at(DEFAULT_KW) {
            self.bump(DEFAULT_KW);
        } else {
            self.bump(CASE_KW);
            self.switch_case_item();
            while self.eat(COMMA) {
                self.switch_case_item();
            }
        }
        m.complete(self, SWITCH_LABEL);
    }

    fn switch_case_item(&mut self) {
        if self.at(DEFAULT_KW) {
            // `case null, default`.
            self.bump(DEFAULT_KW);
        } else if self.at_pattern() {
            self.pattern();
        } else {
            // A case constant is an expression, but in an arrow rule the trailing
            // `->` is the rule arrow, not a lambda: `case A -> ...` is the label `A`
            // followed by the arrow, never the lambda `A -> ...`. Parse just below
            // the lambda layer so the arrow is left for the rule.
            let _ = self.assignment_expr();
        }
        // A guard (`when <expr>`) may follow a pattern or — leniently, for error
        // resilience — a bare constant label.
        if self.at_contextual_kw("when") {
            self.guard();
        }
    }

    fn guard(&mut self) {
        let m = self.start();
        self.bump_remap(WHEN_KW);
        // The guard is a boolean expression; like a case constant, it must not eat
        // the rule's trailing `->` as a lambda arrow, so parse below the lambda layer.
        let _ = self.assignment_expr();
        m.complete(self, GUARD);
    }

    // ===== Patterns (instanceof / switch) =====

    /// Whether this is the start of a type pattern / record pattern: optional variable modifiers
    /// (`final`, annotations) and a type, followed by a binding name (`IDENT` / `_`) or `(` (record).
    fn at_pattern(&self) -> bool {
        // A keyword modifier (`final`) can only begin a pattern's variable modifiers here; in both
        // caller contexts (instanceof RHS, switch case label) it never starts a type or constant.
        // (`default` is filtered by the switch label callers before `at_pattern` is reached.)
        if self.at_ts(MODIFIER_KW) {
            return true;
        }
        // Leading annotations may be type-use annotations (`@DA String`, still a plain type) or
        // variable modifiers (`@DA String s`), so they alone are not decisive: skip them and require
        // a binding / record `(` after the type.
        let after_anno = self.skip_annotations_lookahead(0);
        let k = self.nth_nofuel(after_anno);
        if !(k == IDENT || PRIMITIVE_TYPE.contains(k)) {
            return false;
        }
        let Some(i) = self.skip_type(after_anno) else {
            return false;
        };
        match self.nth_nofuel(i) {
            // `Type(...)` is a record pattern; `Type _` is a type pattern with an unnamed binding.
            LPAREN | UNDERSCORE => true,
            // A type pattern's binding is a plain identifier. The contextual keyword `when` instead
            // begins a guard, so `Type when ...` is a bare constant label with a guard.
            IDENT => self.nth_text(i) != "when",
            _ => false,
        }
    }

    /// Consumes a binding name: an identifier or the unnamed-variable marker `_` (Java 21+).
    fn binding_name(&mut self) -> bool {
        if self.eat(UNDERSCORE) {
            true
        } else {
            self.expect(IDENT)
        }
    }

    fn pattern(&mut self) {
        let m = self.start();
        // A type pattern may carry variable modifiers (`final`). Annotations are consumed by `type_`,
        // so only a keyword modifier needs `modifiers` here (keeps annotation placement symmetric with
        // the binding-less type case and avoids an empty `MODIFIERS` node on the common path).
        if self.at_ts(MODIFIER_KW) {
            self.modifiers();
        }
        self.type_();
        if self.at(LPAREN) {
            // Record pattern: Type(component, ...)
            self.bump(LPAREN);
            while !self.at(RPAREN) && !self.at_eof() {
                let before = self.pos();
                self.record_component();
                if self.pos() == before {
                    self.err_and_bump("unexpected token");
                }
                if !self.eat(COMMA) {
                    break;
                }
            }
            self.expect(RPAREN);
            m.complete(self, RECORD_PATTERN);
        } else {
            // Type pattern: {modifier} Type binding
            self.binding_name();
            m.complete(self, TYPE_PATTERN);
        }
    }

    /// One component of a record pattern: the unnamed pattern `_`, or a nested pattern
    /// (type pattern, `var`/annotated binding, or another record pattern).
    fn record_component(&mut self) {
        if self.at(UNDERSCORE) {
            let m = self.start();
            self.bump(UNDERSCORE);
            m.complete(self, UNNAMED_PATTERN);
        } else {
            self.pattern();
        }
    }

    // ===== Expressions (assignment -> ternary -> binary via precedence climbing -> unary -> postfix -> primary) =====

    fn at_expr_start(&self) -> bool {
        // A primitive type keyword can begin an expression only as a class literal
        // (`int.class`, `boolean[].class`) or an array constructor reference (`int[]::new`).
        self.at_ts(EXPR_START)
            || (self.at_ts(PRIMITIVE_TYPE) && self.at_primitive_class_literal_or_method_ref())
    }

    /// Whether a primitive type keyword at the current position begins an expression:
    /// a class literal (`int.class`, `boolean[].class`) or an array constructor
    /// reference (`int[]::new` — at least one `[]` pair; bare `int::new` is not a type).
    fn at_primitive_class_literal_or_method_ref(&self) -> bool {
        let i = self.skip_bracket_pairs(1);
        self.nth_nofuel(i) == DOT || (i > 1 && self.nth_nofuel(i) == COLON_COLON)
    }

    /// Parses an expression (entry point).
    fn expr(&mut self) {
        let _ = self.expr_opt();
    }

    fn expr_opt(&mut self) -> Option<CompletedMarker> {
        if self.at_lambda() {
            return Some(self.lambda_expr());
        }
        self.assignment_expr()
    }

    /// Assignment expression (right-associative). Handles `=` / `+=` etc., including fused `>>=` / `>>>=`.
    fn assignment_expr(&mut self) -> Option<CompletedMarker> {
        let lhs = self.ternary_expr()?;
        if let Some(len) = self.at_assign_op() {
            let m = lhs.precede(self);
            for _ in 0..len {
                self.bump_any();
            }
            self.expr(); // right-associative: allows lambda/ternary/nested assignment.
            return Some(m.complete(self, ASSIGNMENT_EXPR));
        }
        Some(lhs)
    }

    /// Length (token count) of an assignment operator. `>>=` is GT GT EQ = 3, `>>>=` is 4.
    fn at_assign_op(&self) -> Option<u8> {
        match self.current() {
            EQ | PLUS_EQ | MINUS_EQ | STAR_EQ | SLASH_EQ | PERCENT_EQ | AMP_EQ | PIPE_EQ
            | CARET_EQ | LSHIFT_EQ => Some(1),
            GT => {
                if self.nth_at(1, GT) && self.nth_adjacent(0) {
                    if self.nth_at(2, GT) && self.nth_adjacent(1) {
                        if self.nth_at(3, EQ) && self.nth_adjacent(2) {
                            return Some(4); // >>>=
                        }
                        return None;
                    }
                    if self.nth_at(2, EQ) && self.nth_adjacent(1) {
                        return Some(3); // >>=
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn ternary_expr(&mut self) -> Option<CompletedMarker> {
        let cond = self.expr_bp(0)?;
        if self.at(QUESTION) {
            let m = cond.precede(self);
            self.bump(QUESTION);
            self.expr(); // then
            self.expect(COLON);
            self.expr(); // else (right-associative)
            return Some(m.complete(self, TERNARY_EXPR));
        }
        Some(cond)
    }

    /// Parses a binary expression with minimum binding power `min_bp` (precedence climbing).
    fn expr_bp(&mut self, min_bp: u8) -> Option<CompletedMarker> {
        let mut lhs = self.unary_expr()?;

        while let Some((op_bp, op_len, right_assoc)) = self.peek_bin_op() {
            if op_bp < min_bp {
                break;
            }
            let m = lhs.precede(self);
            if self.at(INSTANCEOF_KW) {
                // Right-hand side of `instanceof` is a type or pattern (`o instanceof String s`).
                self.bump(INSTANCEOF_KW);
                if self.at_pattern() {
                    self.pattern();
                } else {
                    self.type_();
                }
            } else {
                self.consume_bin_op(op_len);
                let next_min = if right_assoc { op_bp } else { op_bp + 1 };
                self.expr_bp(next_min);
            }
            lhs = m.complete(self, BINARY_EXPR);
        }
        Some(lhs)
    }

    /// Returns (binding power, token length, is right-associative) for the next binary operator, including fused `>` family.
    /// Returns `None` for `>>=` / `>>>=` (assignment), deferring to the assignment layer.
    fn peek_bin_op(&self) -> Option<(u8, u8, bool)> {
        let bp = match self.current() {
            PIPE_PIPE => return Some((1, 1, false)),
            AMP_AMP => return Some((2, 1, false)),
            PIPE => return Some((3, 1, false)),
            CARET => return Some((4, 1, false)),
            AMP => return Some((5, 1, false)),
            EQ_EQ | BANG_EQ => return Some((6, 1, false)),
            LT | LT_EQ | INSTANCEOF_KW => return Some((7, 1, false)),
            GT => {
                if self.nth_at(1, GT) && self.nth_adjacent(0) {
                    if self.nth_at(2, GT) && self.nth_adjacent(1) {
                        if self.nth_at(3, EQ) && self.nth_adjacent(2) {
                            return None; // >>>= is assignment.
                        }
                        return Some((8, 3, false)); // >>>
                    }
                    if self.nth_at(2, EQ) && self.nth_adjacent(1) {
                        return None; // >>= is assignment.
                    }
                    return Some((8, 2, false)); // >>
                }
                if self.nth_at(1, EQ) && self.nth_adjacent(0) {
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
    fn consume_bin_op(&mut self, len: u8) {
        for _ in 0..len {
            self.bump_any();
        }
    }

    fn unary_expr(&mut self) -> Option<CompletedMarker> {
        if let Some(pure_primitive) = self.cast_kind() {
            return Some(self.cast_expr(pure_primitive));
        }
        match self.current() {
            BANG | TILDE | PLUS | MINUS | PLUS_PLUS | MINUS_MINUS => {
                let m = self.start();
                self.bump_any();
                self.unary_expr();
                Some(m.complete(self, UNARY_EXPR))
            }
            _ => self.postfix_expr(),
        }
    }

    /// Classifies `( ... ) operand` as a cast (fuel-free lookahead). `Some(true)` = a single
    /// primitive type (`(int)`), where a lambda operand is illegal; `Some(false)` = a reference or
    /// intersection type, where a lambda operand is allowed (JLS §15.16); `None` = not a cast.
    /// A top-level lambda is already disambiguated before this call (in `expr_opt`).
    fn cast_kind(&self) -> Option<bool> {
        if !self.at(LPAREN) {
            return None;
        }
        // A pure primitive cast (`(int) x`) has no annotations and a single primitive token; an
        // annotated primitive (`(@A int) x`) is treated as a reference-like cast (JLS §15.16), so a
        // unary `+`/`-` operand is not allowed to follow.
        let first = self.skip_annotations_lookahead(1);
        let prim_first = first == 1 && PRIMITIVE_TYPE.contains(self.nth_nofuel(first));
        let mut i = self.skip_type(1)?;
        // Intersection type `(A & B)`.
        while self.nth_nofuel(i) == AMP {
            i = self.skip_type(i + 1)?;
        }
        if self.nth_nofuel(i) != RPAREN {
            return None;
        }
        let after = self.nth_nofuel(i + 1);
        let pure_primitive = prim_first && i == 2;
        let ok = if pure_primitive {
            CAST_FOLLOW_PRIMITIVE.contains(after)
        } else {
            CAST_FOLLOW_REF.contains(after)
        };
        ok.then_some(pure_primitive)
    }

    fn cast_expr(&mut self, pure_primitive: bool) -> CompletedMarker {
        let m = self.start();
        self.bump(LPAREN);
        self.type_();
        while self.at(AMP) {
            self.bump(AMP);
            self.type_();
        }
        self.expect(RPAREN);
        // A lambda operand is legal only after a reference/intersection type (JLS §15.16);
        // a pure primitive cast keeps the unary-operand path.
        if !pure_primitive && self.at_lambda() {
            self.lambda_expr();
        } else {
            self.unary_expr();
        }
        m.complete(self, CAST_EXPR)
    }

    /// Whether `[ ]`+ `. class` follows — the tail of an array class literal.
    fn at_array_class_literal(&self) -> bool {
        let i = self.skip_bracket_pairs(0);
        i > 0 && self.nth_nofuel(i) == DOT && self.nth_nofuel(i + 1) == CLASS_KW
    }

    /// Whether `[ ]`+ `::` follows — the tail of an array constructor method reference.
    fn at_array_method_ref(&self) -> bool {
        let i = self.skip_bracket_pairs(0);
        i > 0 && self.nth_nofuel(i) == COLON_COLON
    }

    /// If the token at `start` is `<`, skips a balanced angle-bracket group — counting each
    /// `>` as closing one level, since `>>`/`>>>` are lexed as adjacent `GT` tokens — and
    /// returns the offset just past the closing `>`. Returns `None` if the brackets do not
    /// balance before a hard delimiter (`;`, `{`, `}`, EOF), matching the angle-skip in
    /// [`Self::skip_type`].
    fn skip_angle_brackets(&self, start: usize) -> Option<usize> {
        if self.nth_nofuel(start) != LT {
            return None;
        }
        let mut i = start;
        let mut depth = 0i32;
        loop {
            match self.nth_nofuel(i) {
                LT => {
                    depth += 1;
                    i += 1;
                }
                GT => {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                EOF | SEMICOLON | LBRACE | RBRACE => return None,
                _ => i += 1,
            }
        }
    }

    /// Whether `<...> ('[' ']')* ::` follows — the tail of a generic-qualified reference whose
    /// receiver is a parameterized type: a method/constructor reference (`Foo<String>::new`,
    /// `a.b.C<X>::method`, JLS 15.13 `ClassType :: ...`) or a generic array constructor
    /// reference (`Foo<?>[]::new`, JLS 15.13 `ArrayType :: new`). The balanced `<...>` (and any
    /// trailing `[]` dimensions) must be followed by `::`, which keeps an ordinary comparison
    /// (`a < b > c`) from matching.
    fn at_generic_method_ref(&self) -> bool {
        let Some(i) = self.skip_angle_brackets(0) else {
            return false;
        };
        self.nth_nofuel(self.skip_bracket_pairs(i)) == COLON_COLON
    }

    /// Parses the `:: [type_args] (new | ident)` tail of a method reference.
    fn method_ref_tail(&mut self) {
        // `expect` rather than `bump`: the `::` lookahead (`at_generic_method_ref` /
        // `at_array_method_ref`) skips a balanced `<...>`/`[]` run permissively, but the
        // real consumer (`type_args`) can stop short on malformed input (e.g. `x<0<>>::`),
        // leaving the cursor off the `::`. Recording an error keeps the parser panic-free.
        self.expect(COLON_COLON);
        if self.at(LT) {
            self.type_args();
        }
        if self.at(NEW_KW) {
            self.bump(NEW_KW);
        } else {
            self.expect(IDENT);
        }
    }

    fn postfix_expr(&mut self) -> Option<CompletedMarker> {
        let mut lhs = self.primary_expr()?;
        loop {
            match self.current() {
                DOT if self.nth_at(1, CLASS_KW) => {
                    let m = lhs.precede(self);
                    self.bump(DOT);
                    self.bump(CLASS_KW);
                    lhs = m.complete(self, CLASS_LITERAL);
                }
                DOT if self.nth_at(1, LT) => {
                    // Explicit type arguments (a "type witness") on a method call:
                    // `recv.<Type>method(...)`, e.g. `List.<String>of()`. The `<Type>method`
                    // selector folds into a FIELD_ACCESS; the trailing argument list is then
                    // consumed by the LPAREN arm on the next iteration to form the CALL_EXPR.
                    // The member may also be `super`/`this` for a qualified explicit constructor
                    // invocation `recv.<Type>super(...)` / `recv.<Type>this(...)` (JLS 8.8.7.1).
                    let m = lhs.precede(self);
                    self.bump(DOT);
                    self.type_args();
                    if self.at(THIS_KW) || self.at(SUPER_KW) {
                        self.bump_any();
                    } else {
                        self.expect(IDENT);
                    }
                    lhs = m.complete(self, FIELD_ACCESS);
                }
                DOT if self.nth_at(1, IDENT)
                    || self.nth_at(1, THIS_KW)
                    || self.nth_at(1, SUPER_KW) =>
                {
                    let m = lhs.precede(self);
                    self.bump(DOT);
                    self.bump_any(); // IDENT / this / super
                    lhs = m.complete(self, FIELD_ACCESS);
                }
                DOT if self.nth_at(1, NEW_KW) => {
                    // Qualified class instance creation for an inner class:
                    // `outer.new Inner(...)`, chained as `a.new B().new C()`. The qualifier is
                    // the current `lhs`; the rest mirrors the constructor-call form of `new_expr`
                    // (no array creation is legal here).
                    let m = lhs.precede(self);
                    self.bump(DOT);
                    self.bump(NEW_KW);
                    if self.at(LT) {
                        self.type_args();
                    }
                    self.type_();
                    if self.at(LPAREN) {
                        self.arg_list();
                        if self.at(LBRACE) {
                            self.class_body();
                        }
                    }
                    lhs = m.complete(self, NEW_EXPR);
                }
                LT if self.at_generic_method_ref() => {
                    // Generic-qualified method/constructor reference: the receiver is a
                    // parameterized type (`Foo<String>::new`, `a.b.C<X>::method`, JLS 15.13
                    // `ClassType :: ...`), optionally with array dimensions for a generic array
                    // constructor reference (`Foo<?>[]::new`, `ArrayType :: new`). The lhs is an
                    // already-completed NAME_REF / FIELD_ACCESS receiver, so the TYPE_ARGS node
                    // and any dimension tokens sit directly under METHOD_REF_EXPR, mirroring the
                    // array-dimension tokens of `String[]::new`.
                    let m = lhs.precede(self);
                    self.type_args();
                    while self.at(LBRACK) && self.nth_at(1, RBRACK) {
                        self.bump(LBRACK);
                        self.bump(RBRACK);
                    }
                    self.method_ref_tail();
                    lhs = m.complete(self, METHOD_REF_EXPR);
                }
                COLON_COLON => {
                    let m = lhs.precede(self);
                    self.method_ref_tail();
                    lhs = m.complete(self, METHOD_REF_EXPR);
                }
                LPAREN => {
                    let m = lhs.precede(self);
                    self.arg_list();
                    lhs = m.complete(self, CALL_EXPR);
                }
                LBRACK if self.at_array_method_ref() => {
                    // `String[]::new` / `a.b.C[][]::new` (JLS 15.13 `ArrayType :: new`). As
                    // with the array class literal below, the lhs is an already-completed
                    // NAME_REF / FIELD_ACCESS, so the dimension tokens sit directly under
                    // METHOD_REF_EXPR.
                    let m = lhs.precede(self);
                    while self.at(LBRACK) && self.nth_at(1, RBRACK) {
                        self.bump(LBRACK);
                        self.bump(RBRACK);
                    }
                    self.method_ref_tail();
                    lhs = m.complete(self, METHOD_REF_EXPR);
                }
                LBRACK if self.at_array_class_literal() => {
                    // `String[].class` / `a.b.C[][].class` (JLS 15.8.2 `TypeName {[]} . class`).
                    // Unlike the primitive form (which wraps its type in a TYPE node via
                    // `primary_expr`), the lhs here is an already-completed NAME_REF /
                    // FIELD_ACCESS, so the dimension tokens sit directly under CLASS_LITERAL.
                    let m = lhs.precede(self);
                    while self.at(LBRACK) && self.nth_at(1, RBRACK) {
                        self.bump(LBRACK);
                        self.bump(RBRACK);
                    }
                    self.bump(DOT);
                    self.bump(CLASS_KW);
                    lhs = m.complete(self, CLASS_LITERAL);
                }
                LBRACK => {
                    let m = lhs.precede(self);
                    self.bump(LBRACK);
                    self.expr();
                    self.expect(RBRACK);
                    lhs = m.complete(self, INDEX_EXPR);
                }
                PLUS_PLUS | MINUS_MINUS => {
                    let m = lhs.precede(self);
                    self.bump_any();
                    lhs = m.complete(self, POSTFIX_EXPR);
                }
                _ => break,
            }
        }
        Some(lhs)
    }

    fn primary_expr(&mut self) -> Option<CompletedMarker> {
        let cm = match self.current() {
            _ if self.at_ts(LITERAL_TOKEN) => {
                let m = self.start();
                self.bump_any();
                m.complete(self, LITERAL)
            }
            IDENT => {
                let m = self.start();
                self.bump(IDENT);
                m.complete(self, NAME_REF)
            }
            THIS_KW | SUPER_KW => {
                let m = self.start();
                self.bump_any();
                m.complete(self, NAME_REF)
            }
            LT => {
                // Explicit constructor invocation with a leading type witness:
                // `<Type>this(...)` / `<Type>super(...)` (JLS 8.8.7.1). A `<` is otherwise never
                // legal at the start of an operand, so this is the only meaning. The whole call is
                // built here (including the argument list) so the TYPE_ARGS node sits directly
                // under CALL_EXPR, before the `this`/`super` callee — letting the postfix LPAREN
                // arm form the CALL_EXPR instead would nest the witness one level too deep.
                let m = self.start();
                self.type_args();
                let nm = self.start();
                if self.at(THIS_KW) || self.at(SUPER_KW) {
                    self.bump_any();
                } else {
                    self.error("expected `this` or `super`");
                }
                nm.complete(self, NAME_REF);
                if self.at(LPAREN) {
                    self.arg_list();
                }
                m.complete(self, CALL_EXPR)
            }
            LPAREN => {
                let m = self.start();
                self.bump(LPAREN);
                self.expr();
                self.expect(RPAREN);
                m.complete(self, PAREN_EXPR)
            }
            NEW_KW => self.new_expr(),
            SWITCH_KW => self.switch_expr(),
            _ if self.at_ts(PRIMITIVE_TYPE) && self.at_primitive_class_literal_or_method_ref() => {
                // Class literal `int.class` / `boolean[].class` / `void.class` (JLS 15.8.2)
                // or array constructor reference `int[]::new` (JLS 15.13) — the only
                // expressions that can begin with a primitive type keyword.
                let m = self.start();
                self.type_(); // keyword + `[]` dims → TYPE node
                if self.at(COLON_COLON) {
                    self.method_ref_tail();
                    m.complete(self, METHOD_REF_EXPR)
                } else {
                    self.expect(DOT); // guaranteed by the gate above
                    self.expect(CLASS_KW);
                    m.complete(self, CLASS_LITERAL)
                }
            }
            _ => {
                self.err_and_bump("expected an expression");
                return None;
            }
        };
        Some(cm)
    }

    fn switch_expr(&mut self) -> CompletedMarker {
        let m = self.start();
        self.bump(SWITCH_KW);
        self.expect(LPAREN);
        self.expr();
        self.expect(RPAREN);
        self.switch_block();
        m.complete(self, SWITCH_EXPR)
    }

    fn new_expr(&mut self) -> CompletedMarker {
        let m = self.start();
        self.bump(NEW_KW);
        if self.at(LT) {
            // Explicit type arguments (a "type witness") on a constructor call:
            // `new <Integer>Foo<>(...)` (JLS 15.9 `new [TypeArguments] ...`). The qualified
            // inner-class form `outer.new <T>Inner()` is handled by the matching arm in
            // `postfix_expr`; the TYPE_ARGS node sits directly under NEW_EXPR before `ty`.
            self.type_args();
        }
        self.type_();
        if self.at(LPAREN) {
            self.arg_list();
            if self.at(LBRACE) {
                // Anonymous class body.
                self.class_body();
            }
        } else if self.at(LBRACK) || self.at_annotated_dim() {
            // Array creation `new int[n]` / `new int[n][]` / `new int @A [n]`.
            while self.at(LBRACK) || self.at_annotated_dim() {
                while self.at(AT) && !self.nth_at(1, INTERFACE_KW) {
                    self.annotation();
                }
                self.expect(LBRACK);
                if !self.at(RBRACK) {
                    self.expr();
                }
                self.expect(RBRACK);
            }
            if self.at(LBRACE) {
                self.array_init();
            }
        } else if self.at(LBRACE) {
            // `new int[]{...}` (the type side already consumed `[]`).
            self.array_init();
        }
        m.complete(self, NEW_EXPR)
    }

    fn arg_list(&mut self) {
        let m = self.start();
        self.bump(LPAREN);
        while !self.at(RPAREN) && !self.at_eof() {
            let before = self.pos();
            self.expr();
            if self.pos() == before {
                self.err_and_bump("unexpected argument");
            }
            if !self.eat(COMMA) {
                break;
            }
        }
        self.expect(RPAREN);
        m.complete(self, ARG_LIST);
    }

    // ===== lambda =====

    /// Whether this is the start of a lambda (`id ->` or `( ... ) ->`). Matches `)` using fuel-free lookahead.
    fn at_lambda(&self) -> bool {
        if (self.at(IDENT) || self.at(UNDERSCORE)) && self.nth_at(1, ARROW) {
            return true;
        }
        if self.at(LPAREN) {
            let mut depth = 0i32;
            let mut i = 0usize;
            loop {
                match self.nth_nofuel(i) {
                    LPAREN => depth += 1,
                    RPAREN => {
                        depth -= 1;
                        if depth == 0 {
                            return self.nth_nofuel(i + 1) == ARROW;
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

    fn lambda_expr(&mut self) -> CompletedMarker {
        let m = self.start();
        self.lambda_params();
        self.expect(ARROW);
        if self.at(LBRACE) {
            self.block();
        } else {
            self.expr();
        }
        m.complete(self, LAMBDA_EXPR)
    }

    fn lambda_params(&mut self) {
        let m = self.start();
        if self.at(LPAREN) {
            self.bump(LPAREN);
            while !self.at(RPAREN) && !self.at_eof() {
                let before = self.pos();
                self.lambda_param();
                if self.pos() == before {
                    self.err_and_bump("unexpected argument");
                }
                if !self.eat(COMMA) {
                    break;
                }
            }
            self.expect(RPAREN);
        } else {
            // Single bare identifier — wrap it in a PARAM node so the tree is uniform with the
            // parenthesized form (`(x) -> ...` and `x -> ...` both expose a PARAM).
            let pm = self.start();
            self.binding_name();
            pm.complete(self, PARAM);
        }
        m.complete(self, LAMBDA_PARAMS);
    }

    fn lambda_param(&mut self) {
        let pm = self.start();
        if (self.at(IDENT) || self.at(UNDERSCORE))
            && (self.nth_at(1, COMMA) || self.nth_at(1, RPAREN))
        {
            // Bare untyped parameter (`x` / `_`).
            self.bump_any();
        } else {
            // Typed parameter (including `var`).
            self.modifiers();
            self.type_();
            self.binding_name();
        }
        pm.complete(self, PARAM);
    }
}
