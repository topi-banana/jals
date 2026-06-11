//! A typed AST layer over the untyped `rowan` CST.
//!
//! Each grammar node gets a newtype wrapper that implements [`AstNode`]. The wrappers are
//! zero-cost views into the green tree: casting is a kind check, and accessors walk children
//! lazily via [`support`]. `jals-fmt` / `jals-lint` / `jals-lsp` read the tree through this layer
//! instead of matching on raw [`SyntaxKind`]s.
//!
//! The layer is hand-written for now (per the plan); migrating to a generator (`ungrammar`) is a
//! later option. Accessors are intentionally permissive — they return `Option`/iterators and never
//! panic — because the parser is error-resilient and may produce incomplete nodes.
//!
//! Three flavors of wrapper appear here:
//! - **Node wrappers** (e.g. [`ClassDecl`]): one struct per `SyntaxKind` node, via [`ast_node!`].
//! - **Enums** (e.g. [`Decl`], [`Stmt`], [`Expr`], [`Type`]): a sum over related node kinds, so a
//!   caller can match on "any statement" without knowing the concrete kind.
//! - **Tokens** ([`NameRef`], modifier/operator queries): typed access to significant tokens.

use rowan::ast::support;
pub use rowan::ast::{AstChildren, AstNode, AstPtr, SyntaxNodePtr};

use crate::language::{JavaLanguage, SyntaxNode, SyntaxToken};
use crate::syntax_kind::SyntaxKind::{self, *};

/// Defines a newtype wrapper around a single [`SyntaxKind`] node and implements [`AstNode`].
macro_rules! ast_node {
    ($(#[$meta:meta])* $name:ident, $kind:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[repr(transparent)]
        pub struct $name {
            syntax: SyntaxNode,
        }

        impl AstNode for $name {
            type Language = JavaLanguage;

            fn can_cast(kind: SyntaxKind) -> bool {
                kind == $kind
            }

            fn cast(syntax: SyntaxNode) -> Option<Self> {
                if syntax.kind() == $kind {
                    Some($name { syntax })
                } else {
                    None
                }
            }

            fn syntax(&self) -> &SyntaxNode {
                &self.syntax
            }
        }
    };
}

/// Defines an enum over several node kinds, with one variant per wrapper, and implements
/// [`AstNode`] by dispatching on [`SyntaxKind`].
macro_rules! ast_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident($node:ident)),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant($node)),+
        }

        impl AstNode for $name {
            type Language = JavaLanguage;

            fn can_cast(kind: SyntaxKind) -> bool {
                $(<$node as AstNode>::can_cast(kind))||+
            }

            fn cast(syntax: SyntaxNode) -> Option<Self> {
                $(
                    if <$node as AstNode>::can_cast(syntax.kind()) {
                        return <$node as AstNode>::cast(syntax).map($name::$variant);
                    }
                )+
                None
            }

            fn syntax(&self) -> &SyntaxNode {
                match self {
                    $($name::$variant(it) => it.syntax()),+
                }
            }
        }
    };
}

// ===== Shared accessor helpers =====

/// Returns the first significant token (non-trivia) of `node`, if any.
fn first_sig_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|t| !t.kind().is_trivia())
}

/// Concatenates the text of all non-trivia tokens beneath `node` (drops whitespace/comments).
fn non_trivia_text(node: &SyntaxNode) -> String {
    node.descendants_with_tokens()
        .filter_map(|it| it.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| t.text().to_string())
        .collect()
}

/// Returns the name (`IDENT`) declared directly under `node` (e.g. the type/method name).
fn name_text(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|t| t.kind() == IDENT)
        .map(|t| t.text().to_string())
}

// ===== Source file =====

ast_node!(
    /// The whole compilation unit (root node).
    SourceFile,
    SOURCE_FILE
);

impl SourceFile {
    /// The `package` declaration, if present.
    pub fn package(&self) -> Option<PackageDecl> {
        support::child(&self.syntax)
    }

    /// The `import` declarations.
    pub fn imports(&self) -> AstChildren<ImportDecl> {
        support::children(&self.syntax)
    }

    /// The top-level type declarations.
    pub fn decls(&self) -> AstChildren<Decl> {
        support::children(&self.syntax)
    }

    /// The module declaration, if this is a `module-info.java`.
    pub fn module(&self) -> Option<ModuleDecl> {
        support::child(&self.syntax)
    }
}

ast_node!(
    /// `package a.b.c;`
    PackageDecl,
    PACKAGE_DECL
);

impl PackageDecl {
    /// The qualified package name.
    pub fn name(&self) -> Option<QualifiedName> {
        support::child(&self.syntax)
    }
}

ast_node!(
    /// `import a.b.C;` / `import static a.b.C;` / `import a.b.*;`
    ImportDecl,
    IMPORT_DECL
);

impl ImportDecl {
    /// Whether this is a `static` import.
    pub fn is_static(&self) -> bool {
        support::token(&self.syntax, STATIC_KW).is_some()
    }

    /// The imported qualified name (may end in `.*`).
    pub fn name(&self) -> Option<QualifiedName> {
        support::child(&self.syntax)
    }
}

ast_node!(
    /// A dotted name (`a.b.c`, or `a.b.*` in imports).
    QualifiedName,
    QUALIFIED_NAME
);

impl QualifiedName {
    /// The full dotted text as written (without surrounding trivia), e.g. `a.b.c` or `a.b.*`.
    pub fn text(&self) -> String {
        non_trivia_text(&self.syntax)
    }

    /// Whether the name ends in `.*` (on-demand import).
    pub fn is_wildcard(&self) -> bool {
        support::token(&self.syntax, STAR).is_some()
    }
}

// ===== Module declarations =====

ast_node!(
    /// `{@Anno} [open] module a.b.c { directives }` (the contents of a `module-info.java`).
    ModuleDecl,
    MODULE_DECL
);

impl ModuleDecl {
    /// The modifier list (annotations on the module), always present, possibly empty.
    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    /// Whether the module is `open`.
    pub fn is_open(&self) -> bool {
        support::token(&self.syntax, OPEN_KW).is_some()
    }

    /// The module name (`a.b.c`).
    pub fn name(&self) -> Option<QualifiedName> {
        support::child(&self.syntax)
    }

    /// The module body.
    pub fn body(&self) -> Option<ModuleBody> {
        support::child(&self.syntax)
    }
}

ast_node!(
    /// The `{ ... }` body of a module declaration.
    ModuleBody,
    MODULE_BODY
);

impl ModuleBody {
    /// The directives, in source order.
    pub fn directives(&self) -> AstChildren<Directive> {
        support::children(&self.syntax)
    }
}

ast_enum!(
    /// Any module directive.
    Directive {
        Requires(RequiresDirective),
        Exports(ExportsDirective),
        Opens(OpensDirective),
        Uses(UsesDirective),
        Provides(ProvidesDirective),
    }
);

ast_node!(
    /// `requires {transitive | static} ModuleName ;`
    RequiresDirective,
    REQUIRES_DIRECTIVE
);

impl RequiresDirective {
    /// Whether the `transitive` modifier is present.
    pub fn is_transitive(&self) -> bool {
        support::token(&self.syntax, TRANSITIVE_KW).is_some()
    }

    /// Whether the `static` modifier is present.
    pub fn is_static(&self) -> bool {
        support::token(&self.syntax, STATIC_KW).is_some()
    }

    /// The required module name.
    pub fn module_name(&self) -> Option<QualifiedName> {
        support::child(&self.syntax)
    }
}

ast_node!(
    /// `exports PackageName [to ModuleName, ...] ;`
    ExportsDirective,
    EXPORTS_DIRECTIVE
);

impl ExportsDirective {
    /// The exported package name.
    pub fn package_name(&self) -> Option<QualifiedName> {
        support::child(&self.syntax)
    }

    /// The target modules of a qualified `exports ... to ...`, if any.
    pub fn to_modules(&self) -> impl Iterator<Item = QualifiedName> {
        self.syntax
            .children()
            .filter_map(QualifiedName::cast)
            .skip(1)
    }
}

ast_node!(
    /// `opens PackageName [to ModuleName, ...] ;`
    OpensDirective,
    OPENS_DIRECTIVE
);

impl OpensDirective {
    /// The opened package name.
    pub fn package_name(&self) -> Option<QualifiedName> {
        support::child(&self.syntax)
    }

    /// The target modules of a qualified `opens ... to ...`, if any.
    pub fn to_modules(&self) -> impl Iterator<Item = QualifiedName> {
        self.syntax
            .children()
            .filter_map(QualifiedName::cast)
            .skip(1)
    }
}

ast_node!(
    /// `uses TypeName ;`
    UsesDirective,
    USES_DIRECTIVE
);

impl UsesDirective {
    /// The used service type.
    pub fn type_name(&self) -> Option<QualifiedName> {
        support::child(&self.syntax)
    }
}

ast_node!(
    /// `provides TypeName with TypeName, ... ;`
    ProvidesDirective,
    PROVIDES_DIRECTIVE
);

impl ProvidesDirective {
    /// The provided service type.
    pub fn service(&self) -> Option<QualifiedName> {
        support::child(&self.syntax)
    }

    /// The implementation types listed after `with`.
    pub fn providers(&self) -> impl Iterator<Item = QualifiedName> {
        self.syntax
            .children()
            .filter_map(QualifiedName::cast)
            .skip(1)
    }
}

// ===== Declarations =====

ast_enum!(
    /// Any type declaration (class / interface / enum / record / `@interface`).
    Decl {
        Class(ClassDecl),
        Interface(InterfaceDecl),
        Enum(EnumDecl),
        Record(RecordDecl),
        AnnotationType(AnnotationTypeDecl),
    }
);

ast_node!(
    /// `class Foo<T> extends Bar implements I { ... }`
    ClassDecl,
    CLASS_DECL
);

ast_node!(
    /// `interface I extends A, B { ... }`
    InterfaceDecl,
    INTERFACE_DECL
);

ast_node!(
    /// `enum E implements I { A, B; ... }`
    EnumDecl,
    ENUM_DECL
);

ast_node!(
    /// `record R(int x, int y) implements S { ... }`
    RecordDecl,
    RECORD_DECL
);

ast_node!(
    /// `@interface Ann { ... }`
    AnnotationTypeDecl,
    ANNOTATION_TYPE_DECL
);

impl ClassDecl {
    /// The class name.
    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    /// The modifier list (always present, possibly empty).
    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    /// The type parameter list (`<T, U>`), if any.
    pub fn type_params(&self) -> Option<TypeParams> {
        support::child(&self.syntax)
    }

    /// The `extends` clause, if any.
    pub fn extends_clause(&self) -> Option<ExtendsClause> {
        support::child(&self.syntax)
    }

    /// The `implements` clause, if any.
    pub fn implements_clause(&self) -> Option<ImplementsClause> {
        support::child(&self.syntax)
    }

    /// The `permits` clause, if any.
    pub fn permits_clause(&self) -> Option<PermitsClause> {
        support::child(&self.syntax)
    }

    /// The class body.
    pub fn body(&self) -> Option<ClassBody> {
        support::child(&self.syntax)
    }
}

impl InterfaceDecl {
    /// The interface name.
    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    pub fn type_params(&self) -> Option<TypeParams> {
        support::child(&self.syntax)
    }

    /// The `extends` clause (interfaces may extend several).
    pub fn extends_clause(&self) -> Option<ExtendsClause> {
        support::child(&self.syntax)
    }

    pub fn permits_clause(&self) -> Option<PermitsClause> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<ClassBody> {
        support::child(&self.syntax)
    }
}

impl EnumDecl {
    /// The enum name.
    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    pub fn implements_clause(&self) -> Option<ImplementsClause> {
        support::child(&self.syntax)
    }

    /// The enum body (constants + members).
    pub fn body(&self) -> Option<EnumBody> {
        support::child(&self.syntax)
    }
}

impl RecordDecl {
    /// The record name.
    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    pub fn type_params(&self) -> Option<TypeParams> {
        support::child(&self.syntax)
    }

    /// The record header (`(components)`).
    pub fn header(&self) -> Option<RecordHeader> {
        support::child(&self.syntax)
    }

    pub fn implements_clause(&self) -> Option<ImplementsClause> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<ClassBody> {
        support::child(&self.syntax)
    }
}

impl AnnotationTypeDecl {
    /// The annotation type name.
    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<ClassBody> {
        support::child(&self.syntax)
    }
}

ast_node!(
    /// A modifier list (annotations, `public`, `sealed`, `non-sealed`, ...).
    Modifiers,
    MODIFIERS
);

impl Modifiers {
    /// The annotations applied here.
    pub fn annotations(&self) -> AstChildren<Annotation> {
        support::children(&self.syntax)
    }

    /// Whether a plain keyword modifier `kind` (e.g. `PUBLIC_KW`) is present.
    pub fn has(&self, kind: SyntaxKind) -> bool {
        support::token(&self.syntax, kind).is_some()
    }

    /// Whether the `sealed` contextual modifier is present.
    pub fn is_sealed(&self) -> bool {
        support::token(&self.syntax, SEALED_KW).is_some()
    }

    /// Whether the `non-sealed` modifier is present.
    pub fn is_non_sealed(&self) -> bool {
        self.syntax.children().any(|n| n.kind() == NON_SEALED_KW)
    }
}

ast_node!(
    /// `@Foo` / `@Foo(args)`
    Annotation,
    ANNOTATION
);

impl Annotation {
    /// The annotation name.
    pub fn name(&self) -> Option<QualifiedName> {
        support::child(&self.syntax)
    }

    /// The argument list, if any.
    pub fn args(&self) -> Option<AnnotationArgList> {
        support::child(&self.syntax)
    }
}

ast_node!(AnnotationArgList, ANNOTATION_ARG_LIST);

impl AnnotationArgList {
    /// The `name = value` pairs (for normal annotations).
    pub fn pairs(&self) -> AstChildren<AnnotationPair> {
        support::children(&self.syntax)
    }
}

ast_node!(
    /// `name = value` inside an annotation.
    AnnotationPair,
    ANNOTATION_PAIR
);

impl AnnotationPair {
    /// The element name.
    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    /// The value expression, if it is an expression (not an array/nested annotation).
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(TypeParams, TYPE_PARAMS);

impl TypeParams {
    pub fn params(&self) -> AstChildren<TypeParam> {
        support::children(&self.syntax)
    }
}

ast_node!(
    /// A single type parameter (`T`, `T extends Bound & Other`).
    TypeParam,
    TYPE_PARAM
);

impl TypeParam {
    /// The type variable name.
    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    /// The bound types (after `extends`).
    pub fn bounds(&self) -> AstChildren<Type> {
        support::children(&self.syntax)
    }
}

ast_node!(ExtendsClause, EXTENDS_CLAUSE);

impl ExtendsClause {
    /// The supertypes (one for classes, possibly several for interfaces).
    pub fn types(&self) -> AstChildren<Type> {
        support::children(&self.syntax)
    }
}

ast_node!(ImplementsClause, IMPLEMENTS_CLAUSE);

impl ImplementsClause {
    pub fn types(&self) -> AstChildren<Type> {
        support::children(&self.syntax)
    }
}

ast_node!(PermitsClause, PERMITS_CLAUSE);

impl PermitsClause {
    pub fn types(&self) -> AstChildren<Type> {
        support::children(&self.syntax)
    }
}

ast_node!(ThrowsClause, THROWS_CLAUSE);

impl ThrowsClause {
    pub fn types(&self) -> AstChildren<Type> {
        support::children(&self.syntax)
    }
}

ast_node!(RecordHeader, RECORD_HEADER);

impl RecordHeader {
    pub fn components(&self) -> AstChildren<RecordComponent> {
        support::children(&self.syntax)
    }
}

ast_node!(RecordComponent, RECORD_COMPONENT);

impl RecordComponent {
    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }
}

// ===== Class body & members =====

ast_node!(ClassBody, CLASS_BODY);

impl ClassBody {
    /// The members declared in this body.
    pub fn members(&self) -> AstChildren<Member> {
        support::children(&self.syntax)
    }
}

ast_node!(EnumBody, ENUM_BODY);

impl EnumBody {
    /// The enum constants.
    pub fn constants(&self) -> AstChildren<EnumConstant> {
        support::children(&self.syntax)
    }

    /// The members declared after the `;`.
    pub fn members(&self) -> AstChildren<Member> {
        support::children(&self.syntax)
    }
}

ast_node!(EnumConstant, ENUM_CONSTANT);

impl EnumConstant {
    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    /// The constructor argument list, if any.
    pub fn args(&self) -> Option<ArgList> {
        support::child(&self.syntax)
    }

    /// The constant-specific class body, if any.
    pub fn body(&self) -> Option<ClassBody> {
        support::child(&self.syntax)
    }
}

ast_enum!(
    /// Any class/interface member.
    Member {
        Field(FieldDecl),
        Method(MethodDecl),
        Constructor(ConstructorDecl),
        Initializer(Initializer),
        Class(ClassDecl),
        Interface(InterfaceDecl),
        Enum(EnumDecl),
        Record(RecordDecl),
        AnnotationType(AnnotationTypeDecl),
    }
);

ast_node!(FieldDecl, FIELD_DECL);

impl FieldDecl {
    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    /// The declared type.
    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    /// The first declared name (`int a, b;` exposes `a`).
    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    /// The initializer expression of the first declarator, if any.
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(MethodDecl, METHOD_DECL);

impl MethodDecl {
    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    pub fn type_params(&self) -> Option<TypeParams> {
        support::child(&self.syntax)
    }

    /// The return type.
    pub fn return_type(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    pub fn params(&self) -> Option<ParamList> {
        support::child(&self.syntax)
    }

    pub fn throws_clause(&self) -> Option<ThrowsClause> {
        support::child(&self.syntax)
    }

    /// The method body, if it has one (abstract/interface methods do not).
    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

ast_node!(ConstructorDecl, CONSTRUCTOR_DECL);

impl ConstructorDecl {
    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    pub fn params(&self) -> Option<ParamList> {
        support::child(&self.syntax)
    }

    pub fn throws_clause(&self) -> Option<ThrowsClause> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

ast_node!(
    /// An instance or `static` initializer block.
    Initializer,
    INITIALIZER
);

impl Initializer {
    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    pub fn block(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

ast_node!(ParamList, PARAM_LIST);

impl ParamList {
    pub fn params(&self) -> AstChildren<Param> {
        support::children(&self.syntax)
    }
}

ast_node!(Param, PARAM);

impl Param {
    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }
}

// ===== Types =====

ast_node!(
    /// A type reference (`int`, `List<T>`, `a.b.C`, `int[]`, `var`).
    Type,
    TYPE
);

impl Type {
    /// The type arguments of the outermost segment (`List<T>` → `<T>`), if any.
    pub fn type_args(&self) -> Option<TypeArgs> {
        support::child(&self.syntax)
    }

    /// The type text with surrounding/interleaved trivia removed (e.g. `List<T>`).
    ///
    /// Use [`AstNode::syntax`]`().text()` if you need the verbatim slice including trivia.
    pub fn text(&self) -> String {
        non_trivia_text(&self.syntax)
    }
}

ast_node!(TypeArgs, TYPE_ARGS);

impl TypeArgs {
    /// The argument types (wildcards are skipped — only concrete `Type`s are returned).
    pub fn args(&self) -> AstChildren<Type> {
        support::children(&self.syntax)
    }
}

// ===== Statements =====

ast_node!(Block, BLOCK);

impl Block {
    /// The statements in this block.
    pub fn stmts(&self) -> AstChildren<Stmt> {
        support::children(&self.syntax)
    }
}

ast_enum!(
    /// Any statement.
    Stmt {
        LocalVar(LocalVarDecl),
        Block(Block),
        Expr(ExprStmt),
        Return(ReturnStmt),
        If(IfStmt),
        While(WhileStmt),
        DoWhile(DoWhileStmt),
        For(ForStmt),
        ForEach(ForEachStmt),
        Break(BreakStmt),
        Continue(ContinueStmt),
        Throw(ThrowStmt),
        Yield(YieldStmt),
        Assert(AssertStmt),
        Synchronized(SynchronizedStmt),
        Try(TryStmt),
        Switch(SwitchStmt),
        Labeled(LabeledStmt),
        Empty(EmptyStmt),
    }
);

ast_node!(LocalVarDecl, LOCAL_VAR_DECL);

impl LocalVarDecl {
    pub fn modifiers(&self) -> Option<Modifiers> {
        support::child(&self.syntax)
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    /// The initializer of the first declarator, if any.
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(ExprStmt, EXPR_STMT);

impl ExprStmt {
    pub fn expr(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(ReturnStmt, RETURN_STMT);

impl ReturnStmt {
    pub fn expr(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(IfStmt, IF_STMT);

impl IfStmt {
    /// The condition expression.
    pub fn condition(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The `then` and (optional) `else` branches, in source order.
    pub fn branches(&self) -> AstChildren<Stmt> {
        support::children(&self.syntax)
    }
}

ast_node!(WhileStmt, WHILE_STMT);

impl WhileStmt {
    pub fn condition(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<Stmt> {
        support::child(&self.syntax)
    }
}

ast_node!(DoWhileStmt, DO_WHILE_STMT);

impl DoWhileStmt {
    pub fn body(&self) -> Option<Stmt> {
        support::child(&self.syntax)
    }

    pub fn condition(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(ForStmt, FOR_STMT);

ast_node!(ForEachStmt, FOR_EACH_STMT);

impl ForEachStmt {
    /// The loop variable type.
    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    /// The loop variable name.
    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    /// The iterable expression and the body are both children; this returns the iterable.
    pub fn iterable(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<Stmt> {
        support::child(&self.syntax)
    }
}

ast_node!(BreakStmt, BREAK_STMT);

ast_node!(ContinueStmt, CONTINUE_STMT);

ast_node!(ThrowStmt, THROW_STMT);

impl ThrowStmt {
    pub fn expr(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(YieldStmt, YIELD_STMT);

impl YieldStmt {
    pub fn expr(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(AssertStmt, ASSERT_STMT);

ast_node!(SynchronizedStmt, SYNCHRONIZED_STMT);

impl SynchronizedStmt {
    pub fn lock(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

ast_node!(TryStmt, TRY_STMT);

impl TryStmt {
    pub fn resources(&self) -> Option<ResourceList> {
        support::child(&self.syntax)
    }

    /// The `try` block.
    pub fn block(&self) -> Option<Block> {
        support::child(&self.syntax)
    }

    pub fn catches(&self) -> AstChildren<CatchClause> {
        support::children(&self.syntax)
    }

    pub fn finally(&self) -> Option<FinallyClause> {
        support::child(&self.syntax)
    }
}

ast_node!(ResourceList, RESOURCE_LIST);

impl ResourceList {
    pub fn resources(&self) -> AstChildren<Resource> {
        support::children(&self.syntax)
    }
}

ast_node!(Resource, RESOURCE);

ast_node!(CatchClause, CATCH_CLAUSE);

impl CatchClause {
    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn block(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

ast_node!(FinallyClause, FINALLY_CLAUSE);

impl FinallyClause {
    pub fn block(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

ast_node!(LabeledStmt, LABELED_STMT);

impl LabeledStmt {
    /// The label name.
    pub fn label(&self) -> Option<String> {
        name_text(&self.syntax)
    }

    pub fn stmt(&self) -> Option<Stmt> {
        support::child(&self.syntax)
    }
}

ast_node!(EmptyStmt, EMPTY_STMT);

// ===== Switch (shared by statement and expression) =====

ast_node!(SwitchStmt, SWITCH_STMT);

impl SwitchStmt {
    /// The selector expression.
    pub fn selector(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<SwitchBlock> {
        support::child(&self.syntax)
    }
}

ast_node!(SwitchBlock, SWITCH_BLOCK);

impl SwitchBlock {
    /// The arrow-form rules.
    pub fn rules(&self) -> AstChildren<SwitchRule> {
        support::children(&self.syntax)
    }

    /// The colon-form groups.
    pub fn groups(&self) -> AstChildren<SwitchGroup> {
        support::children(&self.syntax)
    }
}

ast_node!(SwitchRule, SWITCH_RULE);

impl SwitchRule {
    pub fn label(&self) -> Option<SwitchLabel> {
        support::child(&self.syntax)
    }
}

ast_node!(SwitchGroup, SWITCH_GROUP);

impl SwitchGroup {
    pub fn labels(&self) -> AstChildren<SwitchLabel> {
        support::children(&self.syntax)
    }

    pub fn stmts(&self) -> AstChildren<Stmt> {
        support::children(&self.syntax)
    }
}

ast_node!(SwitchLabel, SWITCH_LABEL);

impl SwitchLabel {
    /// Whether this is the `default` label.
    pub fn is_default(&self) -> bool {
        support::token(&self.syntax, DEFAULT_KW).is_some()
    }
}

// ===== Patterns =====

ast_enum!(
    /// A pattern (in `instanceof` or `switch`).
    Pattern {
        Type(TypePattern),
        Record(RecordPattern),
        Unnamed(UnnamedPattern),
    }
);

ast_node!(TypePattern, TYPE_PATTERN);

impl TypePattern {
    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    /// The binding name.
    pub fn name(&self) -> Option<String> {
        name_text(&self.syntax)
    }
}

ast_node!(RecordPattern, RECORD_PATTERN);

impl RecordPattern {
    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    /// The sub-patterns.
    pub fn components(&self) -> AstChildren<Pattern> {
        support::children(&self.syntax)
    }
}

ast_node!(
    /// An unnamed pattern (`_`), appearing as a record-pattern component.
    UnnamedPattern,
    UNNAMED_PATTERN
);

ast_node!(
    /// A `when` guard following a pattern.
    Guard,
    GUARD
);

impl Guard {
    pub fn condition(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

// ===== Expressions =====

ast_enum!(
    /// Any expression.
    Expr {
        Literal(Literal),
        NameRef(NameRef),
        Binary(BinaryExpr),
        Unary(UnaryExpr),
        Postfix(PostfixExpr),
        Paren(ParenExpr),
        Call(CallExpr),
        FieldAccess(FieldAccess),
        Index(IndexExpr),
        New(NewExpr),
        Assignment(AssignmentExpr),
        Ternary(TernaryExpr),
        Lambda(LambdaExpr),
        MethodRef(MethodRefExpr),
        Cast(CastExpr),
        Switch(SwitchExpr),
        ClassLiteral(ClassLiteral),
        ArrayInit(ArrayInit),
    }
);

ast_node!(
    /// A literal (`1`, `"s"`, `true`, `null`, ...).
    Literal,
    LITERAL
);

impl Literal {
    /// The literal token.
    pub fn token(&self) -> Option<SyntaxToken> {
        first_sig_token(&self.syntax)
    }

    /// The literal text as written.
    pub fn text(&self) -> Option<String> {
        self.token().map(|t| t.text().to_string())
    }
}

ast_node!(
    /// A name reference (`x`, `this`, `super`).
    NameRef,
    NAME_REF
);

impl NameRef {
    /// The referenced name text.
    pub fn text(&self) -> Option<String> {
        first_sig_token(&self.syntax).map(|t| t.text().to_string())
    }
}

ast_node!(BinaryExpr, BINARY_EXPR);

impl BinaryExpr {
    /// The left and right operands, in source order.
    pub fn operands(&self) -> AstChildren<Expr> {
        support::children(&self.syntax)
    }

    /// The left-hand operand.
    pub fn lhs(&self) -> Option<Expr> {
        self.operands().next()
    }

    /// The right-hand operand (absent for `instanceof`, whose RHS is a type/pattern).
    pub fn rhs(&self) -> Option<Expr> {
        self.operands().nth(1)
    }
}

ast_node!(UnaryExpr, UNARY_EXPR);

impl UnaryExpr {
    pub fn operand(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(PostfixExpr, POSTFIX_EXPR);

impl PostfixExpr {
    pub fn operand(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(ParenExpr, PAREN_EXPR);

impl ParenExpr {
    pub fn expr(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(CallExpr, CALL_EXPR);

impl CallExpr {
    /// The callee expression.
    pub fn callee(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn args(&self) -> Option<ArgList> {
        support::child(&self.syntax)
    }
}

ast_node!(FieldAccess, FIELD_ACCESS);

impl FieldAccess {
    /// The receiver expression.
    pub fn receiver(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The accessed field/member name (the `IDENT` after the dot).
    pub fn field(&self) -> Option<String> {
        self.syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .filter(|t| t.kind() == IDENT)
            .last()
            .map(|t| t.text().to_string())
    }

    /// The explicit type arguments of a type witness (`recv.<String>method`), if present.
    /// Only method-call selectors carry these; a plain field access returns `None`.
    pub fn type_args(&self) -> Option<TypeArgs> {
        support::child(&self.syntax)
    }
}

ast_node!(IndexExpr, INDEX_EXPR);

impl IndexExpr {
    /// The array and index expressions, in source order.
    pub fn parts(&self) -> AstChildren<Expr> {
        support::children(&self.syntax)
    }
}

ast_node!(NewExpr, NEW_EXPR);

impl NewExpr {
    /// The qualifying expression of a qualified inner-class creation
    /// (`qualifier.new Inner()`), or `None` for an unqualified `new`.
    pub fn qualifier(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The created type.
    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn args(&self) -> Option<ArgList> {
        support::child(&self.syntax)
    }

    /// The anonymous class body, if any.
    pub fn body(&self) -> Option<ClassBody> {
        support::child(&self.syntax)
    }
}

ast_node!(ArgList, ARG_LIST);

impl ArgList {
    pub fn args(&self) -> AstChildren<Expr> {
        support::children(&self.syntax)
    }
}

ast_node!(AssignmentExpr, ASSIGNMENT_EXPR);

impl AssignmentExpr {
    /// The target and value, in source order.
    pub fn parts(&self) -> AstChildren<Expr> {
        support::children(&self.syntax)
    }

    pub fn target(&self) -> Option<Expr> {
        self.parts().next()
    }

    pub fn value(&self) -> Option<Expr> {
        self.parts().nth(1)
    }
}

ast_node!(TernaryExpr, TERNARY_EXPR);

impl TernaryExpr {
    /// The condition, then-branch, and else-branch, in source order.
    pub fn parts(&self) -> AstChildren<Expr> {
        support::children(&self.syntax)
    }
}

ast_node!(LambdaExpr, LAMBDA_EXPR);

impl LambdaExpr {
    pub fn params(&self) -> Option<LambdaParams> {
        support::child(&self.syntax)
    }

    /// The expression body, if the lambda has one (block-bodied lambdas return `None`).
    pub fn expr_body(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The block body, if the lambda has one.
    pub fn block_body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

ast_node!(LambdaParams, LAMBDA_PARAMS);

impl LambdaParams {
    pub fn params(&self) -> AstChildren<Param> {
        support::children(&self.syntax)
    }
}

ast_node!(MethodRefExpr, METHOD_REF_EXPR);

impl MethodRefExpr {
    /// The qualifier expression (`expr::m`). For an array constructor reference
    /// like `String[]::new` this is the part before the dimensions (`String`).
    /// `None` for primitive-array forms (see [`Self::ty`]).
    pub fn qualifier(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The receiver type of a primitive-array constructor reference
    /// (`int[]::new`). `None` for reference forms (see [`Self::qualifier`]).
    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }
}

ast_node!(CastExpr, CAST_EXPR);

impl CastExpr {
    /// The target type.
    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    /// The operand expression.
    pub fn expr(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

ast_node!(SwitchExpr, SWITCH_EXPR);

impl SwitchExpr {
    pub fn selector(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<SwitchBlock> {
        support::child(&self.syntax)
    }
}

ast_node!(ClassLiteral, CLASS_LITERAL);

impl ClassLiteral {
    /// The expression before `.class` in a reference form (`String.class`,
    /// `a.b.C.class`). For an array form like `String[].class` this is the part
    /// before the dimensions (`String`). `None` for primitive forms (see [`Self::ty`]).
    pub fn expr(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    /// The type of a primitive or primitive-array class literal (`int.class`,
    /// `long[].class`, `void.class`). `None` for reference forms (see [`Self::expr`]).
    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }
}

ast_node!(ArrayInit, ARRAY_INIT);

impl ArrayInit {
    /// The element expressions (nested array initializers are not `Expr` and are skipped).
    pub fn elements(&self) -> AstChildren<Expr> {
        support::children(&self.syntax)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    /// Casts the parsed root to a [`SourceFile`].
    fn source_file(src: &str) -> SourceFile {
        SourceFile::cast(parse(src).syntax()).expect("root is SOURCE_FILE")
    }

    /// Parses `class C { void m() { <body> } }` and returns the statements of `m`.
    fn method_stmts(body: &str) -> Vec<Stmt> {
        let src = format!("class C {{ void m(Object o) {{ {body} }} }}");
        let file = source_file(&src);
        let Decl::Class(class) = file.decls().next().unwrap() else {
            panic!("expected class");
        };
        let Member::Method(method) = class.body().unwrap().members().next().unwrap() else {
            panic!("expected method");
        };
        method.body().unwrap().stmts().collect()
    }

    /// Returns the single expression of the first expression statement in `body`.
    fn first_expr(body: &str) -> Expr {
        match method_stmts(body).into_iter().next().unwrap() {
            Stmt::Expr(es) => es.expr().unwrap(),
            other => panic!("expected expression statement, got {other:?}"),
        }
    }

    #[test]
    fn package_and_imports() {
        let file = source_file(
            "package a.b.c;\nimport java.util.List;\nimport static a.B.c;\nimport a.b.*;\n",
        );
        assert_eq!(file.package().unwrap().name().unwrap().text(), "a.b.c");

        let imports: Vec<_> = file.imports().collect();
        assert_eq!(imports.len(), 3);
        assert!(!imports[0].is_static());
        assert_eq!(imports[0].name().unwrap().text(), "java.util.List");
        assert!(imports[1].is_static());
        assert!(imports[2].name().unwrap().is_wildcard());
        assert_eq!(imports[2].name().unwrap().text(), "a.b.*");
    }

    #[test]
    fn class_shape() {
        let file = source_file(
            "public final class Foo<T> extends Bar implements I, J { private int x = 1; void m(int a) { return; } }",
        );
        let decl = file.decls().next().unwrap();
        let Decl::Class(class) = decl else {
            panic!("expected class");
        };
        assert_eq!(class.name().as_deref(), Some("Foo"));

        let mods = class.modifiers().unwrap();
        assert!(mods.has(PUBLIC_KW));
        assert!(mods.has(FINAL_KW));
        assert!(!mods.is_sealed());

        let tps: Vec<_> = class.type_params().unwrap().params().collect();
        assert_eq!(tps.len(), 1);
        assert_eq!(tps[0].name().as_deref(), Some("T"));

        assert_eq!(class.extends_clause().unwrap().types().count(), 1);
        assert_eq!(class.implements_clause().unwrap().types().count(), 2);

        let members: Vec<_> = class.body().unwrap().members().collect();
        assert_eq!(members.len(), 2);
        let Member::Field(field) = &members[0] else {
            panic!("expected field");
        };
        assert_eq!(field.name().as_deref(), Some("x"));
        assert_eq!(field.ty().unwrap().text(), "int");
        let Member::Method(method) = &members[1] else {
            panic!("expected method");
        };
        assert_eq!(method.name().as_deref(), Some("m"));
        assert_eq!(method.params().unwrap().params().count(), 1);
        assert!(method.body().is_some());
    }

    #[test]
    fn sealed_modifiers() {
        let file = source_file("public sealed interface S permits A, B { }");
        let Decl::Interface(iface) = file.decls().next().unwrap() else {
            panic!("expected interface");
        };
        assert!(iface.modifiers().unwrap().is_sealed());
        assert_eq!(iface.permits_clause().unwrap().types().count(), 2);

        let file2 = source_file("non-sealed class C { }");
        let Decl::Class(class) = file2.decls().next().unwrap() else {
            panic!("expected class");
        };
        assert!(class.modifiers().unwrap().is_non_sealed());
    }

    #[test]
    fn record_shape() {
        let file = source_file("record Point(int x, int y) implements Shape { }");
        let Decl::Record(record) = file.decls().next().unwrap() else {
            panic!("expected record");
        };
        assert_eq!(record.name().as_deref(), Some("Point"));
        let comps: Vec<_> = record.header().unwrap().components().collect();
        assert_eq!(comps.len(), 2);
        assert_eq!(comps[0].name().as_deref(), Some("x"));
        assert_eq!(comps[1].name().as_deref(), Some("y"));
        assert_eq!(record.implements_clause().unwrap().types().count(), 1);
    }

    #[test]
    fn enum_shape() {
        let file = source_file("enum E { A, B(1), C; int x; }");
        let Decl::Enum(en) = file.decls().next().unwrap() else {
            panic!("expected enum");
        };
        let body = en.body().unwrap();
        let constants: Vec<_> = body.constants().collect();
        assert_eq!(constants.len(), 3);
        assert_eq!(constants[0].name().as_deref(), Some("A"));
        assert!(constants[1].args().is_some());
        assert_eq!(body.members().count(), 1);
    }

    #[test]
    fn statements_and_exprs() {
        let file =
            source_file("class C { void m() { int a = 1 + 2; if (a) return; while (a) m(); } }");
        let Decl::Class(class) = file.decls().next().unwrap() else {
            panic!("expected class");
        };
        let Member::Method(method) = class.body().unwrap().members().next().unwrap() else {
            panic!("expected method");
        };
        let stmts: Vec<_> = method.body().unwrap().stmts().collect();
        assert_eq!(stmts.len(), 3);

        let Stmt::LocalVar(local) = &stmts[0] else {
            panic!("expected local var");
        };
        assert_eq!(local.name().as_deref(), Some("a"));
        assert_eq!(local.ty().unwrap().text(), "int");
        let Some(Expr::Binary(bin)) = local.value() else {
            panic!("expected binary initializer");
        };
        assert!(matches!(bin.lhs(), Some(Expr::Literal(_))));
        assert!(matches!(bin.rhs(), Some(Expr::Literal(_))));

        assert!(matches!(&stmts[1], Stmt::If(_)));
        assert!(matches!(&stmts[2], Stmt::While(_)));
    }

    #[test]
    fn switch_and_patterns() {
        let file = source_file(
            "class C { void m(Object o) { switch (o) { case Integer i when i > 0 -> f(); default -> g(); } } }",
        );
        let Decl::Class(class) = file.decls().next().unwrap() else {
            panic!("expected class");
        };
        let Member::Method(method) = class.body().unwrap().members().next().unwrap() else {
            panic!("expected method");
        };
        let Stmt::Switch(switch) = method.body().unwrap().stmts().next().unwrap() else {
            panic!("expected switch");
        };
        assert!(matches!(switch.selector(), Some(Expr::NameRef(_))));
        let rules: Vec<_> = switch.body().unwrap().rules().collect();
        assert_eq!(rules.len(), 2);
        let first_label = rules[0].label().unwrap();
        assert!(!first_label.is_default());
        assert!(rules[1].label().unwrap().is_default());
    }

    #[test]
    fn if_branches_and_condition() {
        let Stmt::If(if_stmt) = method_stmts("if (o) f(); else g();")
            .into_iter()
            .next()
            .unwrap()
        else {
            panic!("expected if");
        };
        assert!(matches!(if_stmt.condition(), Some(Expr::NameRef(_))));
        // Both the then- and else-statements are `Stmt` children.
        assert_eq!(if_stmt.branches().count(), 2);
    }

    #[test]
    fn for_each_separates_iterable_and_body() {
        let Stmt::ForEach(fe) = method_stmts("for (String s : list) use(s);")
            .into_iter()
            .next()
            .unwrap()
        else {
            panic!("expected for-each");
        };
        assert_eq!(fe.ty().unwrap().text(), "String");
        assert_eq!(fe.name().as_deref(), Some("s"));
        // The iterable is the `Expr` child; the body is a `Stmt` child — they must not collide.
        assert!(matches!(fe.iterable(), Some(Expr::NameRef(_))));
        assert!(matches!(fe.body(), Some(Stmt::Expr(_))));
    }

    #[test]
    fn cast_splits_type_and_operand() {
        let Expr::Assignment(assign) = first_expr("x = (String) o;") else {
            panic!("expected assignment");
        };
        let Some(Expr::Cast(cast)) = assign.value() else {
            panic!("expected cast value");
        };
        assert_eq!(cast.ty().unwrap().text(), "String");
        assert!(matches!(cast.expr(), Some(Expr::NameRef(_))));
    }

    #[test]
    fn call_callee_and_args() {
        let Expr::Call(call) = first_expr("f(a, b, c);") else {
            panic!("expected call");
        };
        assert!(matches!(call.callee(), Some(Expr::NameRef(_))));
        assert_eq!(call.args().unwrap().args().count(), 3);
    }

    #[test]
    fn lambda_body_forms() {
        // Expression-bodied lambda.
        let Expr::Call(call) = first_expr("f(x -> x);") else {
            panic!("expected call");
        };
        let Some(Expr::Lambda(lambda)) = call.args().unwrap().args().next() else {
            panic!("expected lambda arg");
        };
        assert_eq!(lambda.params().unwrap().params().count(), 1);
        assert!(lambda.expr_body().is_some());
        assert!(lambda.block_body().is_none());

        // Block-bodied lambda.
        let Expr::Call(call2) = first_expr("g(() -> { return 0; });") else {
            panic!("expected call");
        };
        let Some(Expr::Lambda(lambda2)) = call2.args().unwrap().args().next() else {
            panic!("expected lambda arg");
        };
        assert!(lambda2.expr_body().is_none());
        assert!(lambda2.block_body().is_some());
    }

    #[test]
    fn try_with_resources_parts() {
        let Stmt::Try(try_stmt) =
            method_stmts("try (var r = open()) { use(r); } catch (E e) { } finally { }")
                .into_iter()
                .next()
                .unwrap()
        else {
            panic!("expected try");
        };
        assert_eq!(try_stmt.resources().unwrap().resources().count(), 1);
        assert!(try_stmt.block().is_some());
        assert_eq!(try_stmt.catches().count(), 1);
        assert!(try_stmt.finally().is_some());
        assert_eq!(try_stmt.catches().next().unwrap().ty().unwrap().text(), "E");
    }

    #[test]
    fn field_access_chain() {
        let Expr::FieldAccess(fa) = first_expr("a.b.c;") else {
            panic!("expected field access");
        };
        // The outermost access is `.c`; its receiver is `a.b`.
        assert_eq!(fa.field().as_deref(), Some("c"));
        assert!(matches!(fa.receiver(), Some(Expr::FieldAccess(_))));
    }

    #[test]
    fn ast_is_lossless_view() {
        // The typed layer is a pure view: the underlying node text still equals the source.
        let src = "class C<T> { List<T> xs; void m() { for (var x : xs) sum += x; } }";
        let file = source_file(src);
        assert_eq!(file.syntax().text().to_string(), src);
    }
}
