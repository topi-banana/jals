//! `nullness-mismatch`: a value that may be `null` written into a slot declared never to hold one.
//!
//! The rule reads **annotations**, not inference. Which annotation types count is
//! [`NullnessMismatch::nullable`] / [`NullnessMismatch::non_null`] — fully-qualified names, matched
//! against the annotation as written and qualified through the file's own single-type imports — and
//! what a declaration carrying neither means is [`NullnessMismatch::default`], whose built-in value
//! is [`Nullness::NonNull`]. That is the strict reading: an unannotated slot rejects `null`.
//!
//! # The four contexts, and why there is no fifth
//!
//! A value reaches a slot in exactly the places `jals-hir`'s own assignment checking looks
//! (`TypeInference::mismatches`): a declarator's initializer, a simple `=`, a `return`, and a call
//! argument. A fifth — dereferencing a nullable value — is deliberately absent. Answering it
//! without false positives needs to know whether a guard ran (`if (x != null)`), and there is no
//! control-flow or definite-assignment layer anywhere below this crate to ask: `jals-hir` has a
//! constant folder and nothing else, and `jals_syntax::CfgMap` is conditional compilation rather
//! than control flow. A rule in `[correctness]` that guessed there would be wrong on ordinary Java.
//!
//! # Conservative by construction
//!
//! Only a value whose nullness is *known* is reported, and only into a slot whose nullness is
//! *known*. Everything else is `Unknown` and silent:
//!
//! - A **conditional** (`cond ? find() : "x"`) is unknown even when one arm is nullable. A reader
//!   sees a guarded expression, and reporting the arm would be the false positive this scope was
//!   chosen to avoid.
//! - A name or call this file cannot bind — an inherited member, another file's method, a
//!   qualified receiver (`obj.find()`) — is unknown. The file-local pass records no reference for
//!   the right-hand name of a member access, so there is nothing to read an annotation off.
//! - An **overloaded** callee is unknown: the scope chain binds a call to *an* overload rather than
//!   to the one the arguments select, so a parameter read off the wrong one would be a finding
//!   about a declaration the call never reaches.
//!
//! # What it does report about a declaration itself
//!
//! One thing: a declaration annotated **both** nullable and non-null. That is not a value flowing
//! anywhere, it is a contract that contradicts itself, and it is the one finding here that needs no
//! second declaration to compare against.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use alloc::collections::BTreeMap;

use jals_config::Category;
use jals_config::lint::{Config, Nullness, NullnessMismatch as Options};
use jals_exec::{LocalBoxFuture, Yielder};
use jals_hir::{DefKind, FileAnalysis, Namespace};
use jals_syntax::SyntaxKind::{
    ASSIGNMENT_EXPR, CALL_EXPR, CONSTRUCTOR_DECL, FIELD_DECL, LAMBDA_EXPR, LOCAL_VAR_DECL,
    METHOD_DECL, MODIFIERS, NULL_KW, PARAM,
};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::rules::{Checker, Finding, RuleMeta, Significant};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "nullness-mismatch",
    category: Category::Correctness,
    level: |config| config.correctness.nullness_mismatch.level,
    // Not because the findings come from inference — they come from annotations — but because
    // `default = "non-null"` reads *silence* as a claim. An annotation error recovery dropped
    // turns a declaration that was saying something into one that says nothing, and this rule
    // then reports the slot it was told to leave alone.
    needs_clean_parse: true,
    check: Checker::Analyzed(NullnessRule::check),
};

/// What a declaration says about `null`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Declared {
    /// A configured `@Nullable`.
    Nullable,
    /// A configured `@NonNull`.
    NonNull,
    /// Both, which is the contradiction the rule reports on the declaration itself.
    Contradictory,
    /// Neither, so [`Options::default`] decides.
    Absent,
}

/// What an expression is known to produce.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Value {
    /// The `null` literal itself.
    Null,
    /// A value read out of a declaration that may hold `null`.
    Nullable,
    /// Anything this rule cannot settle. Never reported.
    Unknown,
}

impl Value {
    /// How the finding names this value, or `None` when there is nothing to report.
    const fn subject(self) -> Option<&'static str> {
        match self {
            Self::Null => Some("`null`"),
            Self::Nullable => Some("a nullable value"),
            Self::Unknown => None,
        }
    }
}

/// The nullness vocabulary one file is read with: the configured name lists, plus the file's own
/// single-type imports, which are what turn a written `@Nullable` into a fully-qualified name.
struct Vocabulary<'a> {
    options: &'a Options,
    /// Simple name → fully-qualified name, from this file's single-type imports. A wildcard import
    /// contributes nothing: it names no single type, so it cannot settle which `Nullable` a bare
    /// `@Nullable` means.
    imports: BTreeMap<String, String>,
}

impl<'a> Vocabulary<'a> {
    /// Reads the file's imports once.
    fn of(root: &SyntaxNode, options: &'a Options) -> Self {
        let mut imports = BTreeMap::new();
        if let Some(file) = ast::SourceFile::cast(root.clone()) {
            for import in file.imports() {
                if import.is_static() || import.is_module() {
                    continue;
                }
                let Some(name) = import.name() else {
                    continue;
                };
                // A jals grouped import (`import a.{B, C};`) writes the shared prefix on the
                // declaration and the types in the group, so each member is qualified with it.
                if let Some(group) = import.group() {
                    let prefix = name.text();
                    for member in group.members() {
                        if let Some(last) = member.last_segment() {
                            imports.insert(last, format!("{prefix}.{}", member.text()));
                        }
                    }
                } else if let Some(last) = name.last_segment() {
                    imports.insert(last, name.text());
                }
            }
        }
        Self { options, imports }
    }

    /// An entry's simple name — what an annotation whose own name no import resolves is matched on.
    fn last_segment(entry: &str) -> &str {
        entry.rsplit('.').next().unwrap_or(entry)
    }

    /// Whether `annotation` is one of `list`.
    ///
    /// Three readings, in the order the source settles them. A name written qualified is already a
    /// fully-qualified name. A simple name a single-type import resolves *is* that import's type,
    /// so `import com.acme.Nullable;` makes `@Nullable` com.acme's and no configured entry matches
    /// it — which is the precision an FQN list is for. A simple name nothing resolves falls back to
    /// matching an entry's last segment: the same limit `@SuppressWarnings` carries in
    /// `crate::suppress`, and for the same reason — an on-demand import leaves the question open,
    /// and resolving the annotation *type* would need the analysis the rules have not run yet.
    fn matches(&self, list: &[String], annotation: &ast::Annotation) -> bool {
        let Some(written) = annotation.name().map(|name| name.text()) else {
            return false;
        };
        if written.contains('.') {
            return list.contains(&written);
        }
        if let Some(fqn) = self.imports.get(&written) {
            return list.iter().any(|entry| entry == fqn);
        }
        list.iter()
            .any(|entry| Self::last_segment(entry) == written)
    }

    /// What `decl` says about `null`.
    fn declared(&self, decl: &SyntaxNode) -> Declared {
        let mut nullable = false;
        let mut non_null = false;
        for annotation in Self::annotations_on(decl) {
            nullable |= self.matches(&self.options.nullable, &annotation);
            non_null |= self.matches(&self.options.non_null, &annotation);
        }
        match (nullable, non_null) {
            (true, true) => Declared::Contradictory,
            (true, false) => Declared::Nullable,
            (false, true) => Declared::NonNull,
            (false, false) => Declared::Absent,
        }
    }

    /// The annotations written on `decl`.
    ///
    /// Both shapes, because the parser produces both: most declarations park their annotations in a
    /// `MODIFIERS` child, while a type parameter, an enum constant and a parameter's type-use
    /// position write them as direct children — the same two `jals-hir`'s `decl_facts` distinguishes
    /// when it records whether a declaration is annotated at all.
    fn annotations_on(decl: &SyntaxNode) -> Vec<ast::Annotation> {
        let mut out = Vec::new();
        for child in decl.children() {
            if child.kind() == MODIFIERS {
                out.extend(child.children().filter_map(ast::Annotation::cast));
            } else if let Some(annotation) = ast::Annotation::cast(child) {
                out.push(annotation);
            }
        }
        out
    }

    /// Whether `decl` is a slot that rejects `null`.
    fn rejects_null(&self, decl: &SyntaxNode) -> bool {
        match self.declared(decl) {
            Declared::NonNull => true,
            // A contradictory declaration is reported as one; nothing is concluded from it.
            Declared::Nullable | Declared::Contradictory => false,
            Declared::Absent => self.options.default == Nullness::NonNull,
        }
    }

    /// Whether reading `decl` may produce `null`.
    fn produces_null(&self, decl: &SyntaxNode) -> bool {
        match self.declared(decl) {
            Declared::Nullable => true,
            Declared::NonNull | Declared::Contradictory => false,
            Declared::Absent => self.options.default == Nullness::Nullable,
        }
    }
}

/// The `nullness-mismatch` rule.
struct NullnessRule;

impl NullnessRule {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(
        analysis: &'a FileAnalysis,
        config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(analysis, config))
    }

    async fn check_impl(analysis: &FileAnalysis, config: &Config) -> Vec<Finding> {
        let root = analysis.root();
        let vocabulary = Vocabulary::of(root, &config.correctness.nullness_mismatch.options);
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            match node.kind() {
                LOCAL_VAR_DECL | FIELD_DECL => {
                    Self::check_declaration(&node, analysis, &vocabulary, &mut out);
                }
                PARAM | METHOD_DECL => Self::check_contradiction(&node, &vocabulary, &mut out),
                ASSIGNMENT_EXPR => Self::check_assignment(&node, analysis, &vocabulary, &mut out),
                jals_syntax::SyntaxKind::RETURN_STMT => {
                    Self::check_return(&node, analysis, &vocabulary, &mut out);
                }
                CALL_EXPR => Self::check_call(&node, analysis, &vocabulary, &mut out),
                _ => {}
            }
        }
        out
    }

    /// A declarator's initializer against the declarator's own nullness, plus the contradiction
    /// check the declaration shares with every other declaring form.
    ///
    /// One node can declare several names (`String a = null, b;`), and only the token order says
    /// which initializer belongs to which — which is why the pairs come from the one walk that
    /// knows (`ast::Declarators::initializers`) rather than from the first-name accessor.
    fn check_declaration(
        node: &SyntaxNode,
        analysis: &FileAnalysis,
        vocabulary: &Vocabulary<'_>,
        out: &mut Vec<Finding>,
    ) {
        Self::check_contradiction(node, vocabulary, out);
        if !vocabulary.rejects_null(node) {
            return;
        }
        for (name, value) in ast::Declarators::initializers(node) {
            let Some(subject) = Self::value_of(&value, analysis, vocabulary).subject() else {
                continue;
            };
            out.push(Finding::at_range(
                Self::span(value.syntax()),
                format!(
                    "{subject} cannot be assigned to `{}`, which is non-null",
                    name.text()
                ),
            ));
        }
    }

    /// A declaration annotated both nullable and non-null.
    fn check_contradiction(node: &SyntaxNode, vocabulary: &Vocabulary<'_>, out: &mut Vec<Finding>) {
        if vocabulary.declared(node) != Declared::Contradictory {
            return;
        }
        let Some(range) = Significant::range(node) else {
            return;
        };
        out.push(Finding::at_range(
            range,
            "this declaration is annotated both nullable and non-null".to_owned(),
        ));
    }

    /// A simple `=` whose target is a name this file declares.
    fn check_assignment(
        node: &SyntaxNode,
        analysis: &FileAnalysis,
        vocabulary: &Vocabulary<'_>,
        out: &mut Vec<Finding>,
    ) {
        let Some(assignment) = ast::AssignmentExpr::cast(node.clone()) else {
            return;
        };
        // A compound assignment (`+=`) never writes a bare reference through, so only `=` is asked.
        if !assignment.is_simple() {
            return;
        }
        let (Some(target), Some(value)) = (assignment.target(), assignment.value()) else {
            return;
        };
        let Some(decl) = Self::declaration_of(target.syntax(), analysis, Namespace::Value) else {
            return;
        };
        if !vocabulary.rejects_null(&decl) {
            return;
        }
        let Some(subject) = Self::value_of(&value, analysis, vocabulary).subject() else {
            return;
        };
        let name = Self::name_text(target.syntax());
        out.push(Finding::at_range(
            Self::span(value.syntax()),
            format!("{subject} cannot be assigned to `{name}`, which is non-null"),
        ));
    }

    /// A `return` against the nullness its enclosing method declares.
    fn check_return(
        node: &SyntaxNode,
        analysis: &FileAnalysis,
        vocabulary: &Vocabulary<'_>,
        out: &mut Vec<Finding>,
    ) {
        let Some(value) = ast::ReturnStmt::cast(node.clone()).and_then(|stmt| stmt.expr()) else {
            return;
        };
        let Some(method) = Self::enclosing_method(node) else {
            return;
        };
        if !vocabulary.rejects_null(&method) {
            return;
        }
        let Some(subject) = Self::value_of(&value, analysis, vocabulary).subject() else {
            return;
        };
        let name = ast::MethodDecl::cast(method)
            .and_then(|method| method.name())
            .unwrap_or_default();
        out.push(Finding::at_range(
            Self::span(value.syntax()),
            format!("{subject} cannot be returned from `{name}`, which is non-null"),
        ));
    }

    /// A call's arguments against the parameters of the method this file resolves it to.
    fn check_call(
        node: &SyntaxNode,
        analysis: &FileAnalysis,
        vocabulary: &Vocabulary<'_>,
        out: &mut Vec<Finding>,
    ) {
        let Some(call) = ast::CallExpr::cast(node.clone()) else {
            return;
        };
        let (Some(callee), Some(args)) = (call.callee(), call.args()) else {
            return;
        };
        let Some(decl) = Self::declaration_of(callee.syntax(), analysis, Namespace::Method) else {
            return;
        };
        let Some(method) = ast::MethodDecl::cast(decl) else {
            return;
        };
        // The scope chain binds a call to *an* overload, so a second method of the same name means
        // this is not the pass that says which one — see the module docs.
        if Self::is_overloaded(analysis, method.name().as_deref()) {
            return;
        }
        let Some(params) = method.params() else {
            return;
        };
        let params: Vec<ast::Param> = params.params().collect();
        let arguments: Vec<ast::Expr> = args.args().collect();
        // A varargs or arity-mismatched call does not line arguments up with parameters one for
        // one, and guessing the pairing would report against a parameter the call never fills.
        if params.len() != arguments.len() {
            return;
        }
        let name = method.name().unwrap_or_default();
        for (param, argument) in params.iter().zip(&arguments) {
            if !vocabulary.rejects_null(param.syntax()) {
                continue;
            }
            let Some(subject) = Self::value_of(argument, analysis, vocabulary).subject() else {
                continue;
            };
            let param_name = param.name().unwrap_or_default();
            out.push(Finding::at_range(
                Self::span(argument.syntax()),
                format!(
                    "{subject} cannot be passed to parameter `{param_name}` of `{name}`, which is \
                     non-null"
                ),
            ));
        }
    }

    /// What `value` is known to produce.
    fn value_of(value: &ast::Expr, analysis: &FileAnalysis, vocabulary: &Vocabulary<'_>) -> Value {
        match value {
            // Parentheses change nothing about what the expression produces.
            ast::Expr::Paren(paren) => paren.expr().map_or(Value::Unknown, |inner| {
                Self::value_of(&inner, analysis, vocabulary)
            }),
            ast::Expr::Literal(literal) => {
                if literal
                    .syntax()
                    .children_with_tokens()
                    .filter_map(jals_syntax::SyntaxElement::into_token)
                    .any(|token| token.kind() == NULL_KW)
                {
                    Value::Null
                } else {
                    Value::Unknown
                }
            }
            ast::Expr::NameRef(_) => {
                Self::from_declaration(value.syntax(), analysis, vocabulary, Namespace::Value)
            }
            ast::Expr::Call(call) => call.callee().map_or(Value::Unknown, |callee| {
                if Self::is_overloaded(
                    analysis,
                    Self::declaration_of(callee.syntax(), analysis, Namespace::Method)
                        .and_then(ast::MethodDecl::cast)
                        .and_then(|method| method.name())
                        .as_deref(),
                ) {
                    return Value::Unknown;
                }
                Self::from_declaration(callee.syntax(), analysis, vocabulary, Namespace::Method)
            }),
            _ => Value::Unknown,
        }
    }

    /// [`Value::Nullable`] when `node` names a declaration this file says may hold `null`.
    fn from_declaration(
        node: &SyntaxNode,
        analysis: &FileAnalysis,
        vocabulary: &Vocabulary<'_>,
        namespace: Namespace,
    ) -> Value {
        Self::declaration_of(node, analysis, namespace)
            .filter(|decl| vocabulary.produces_null(decl))
            .map_or(Value::Unknown, |_| Value::Nullable)
    }

    /// The declaration `node` names, when this file binds it in `namespace`.
    ///
    /// `None` for everything the file-local pass leaves open — an inherited member, a name another
    /// file declares, the right-hand side of a member access (which records no reference at all).
    /// That is the silence the rule's conservatism rests on.
    fn declaration_of(
        node: &SyntaxNode,
        analysis: &FileAnalysis,
        namespace: Namespace,
    ) -> Option<SyntaxNode> {
        let start = Significant::range(node)?.start;
        let reference = analysis.reference_at(start)?;
        if reference.namespace != namespace || reference.range.start != start {
            return None;
        }
        let def = analysis.def(reference.resolution.def_id()?);
        analysis.decl_of(def)
    }

    /// Whether this file declares more than one method named `name`.
    fn is_overloaded(analysis: &FileAnalysis, name: Option<&str>) -> bool {
        let Some(name) = name else {
            return true;
        };
        analysis
            .defs()
            .iter()
            .filter(|def| def.kind == DefKind::Method && def.name == name)
            .count()
            > 1
    }

    /// The method a `return` returns from, or `None` where this rule cannot say which slot the
    /// value reaches.
    ///
    /// A lambda body is the case that matters: `return null;` inside one returns from the lambda,
    /// whose nullness is the functional interface's rather than the enclosing method's. A
    /// constructor stops the walk too — it has no return type, so a `return` in one carries no
    /// value that could be checked.
    fn enclosing_method(node: &SyntaxNode) -> Option<SyntaxNode> {
        node.ancestors()
            .find(|ancestor| {
                matches!(
                    ancestor.kind(),
                    METHOD_DECL | LAMBDA_EXPR | CONSTRUCTOR_DECL
                )
            })
            .filter(|ancestor| ancestor.kind() == METHOD_DECL)
    }

    /// A node's significant span, falling back to its own range when it holds no significant token.
    fn span(node: &SyntaxNode) -> Range<usize> {
        Significant::range(node).unwrap_or_else(|| {
            let range = node.text_range();
            usize::from(range.start())..usize::from(range.end())
        })
    }

    /// The text of the first identifier under `node` — how a finding names an assignment target.
    fn name_text(node: &SyntaxNode) -> String {
        node.descendants_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token: &SyntaxToken| token.kind() == jals_syntax::SyntaxKind::IDENT)
            .map_or_else(String::new, |token| token.text().to_owned())
    }
}
