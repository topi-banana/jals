//! `nullness-mismatch`: a value that may be `null` written into a slot declared never to hold one.
//!
//! The rule reads **annotations**, not inference. Which annotation types count is
//! [`Options::nullable`] / [`Options::non_null`] — fully-qualified names, matched against the
//! annotation as written and qualified through the declaring file's own single-type imports — and
//! what a declaration carrying neither means is [`Options::default`], whose built-in value is
//! [`Nullness::NonNull`]. That is the strict reading: an unannotated slot rejects `null`.
//!
//! # The four contexts, and why there is no fifth
//!
//! A value reaches a slot in four places: a declarator's initializer, a simple `=`, a `return`, and
//! an argument — a call's, and the constructor arguments of a `new`. The first four are the places
//! `jals-hir`'s own assignment checking looks (`TypeInference::mismatches`); the `new` is one it
//! does not reach, and this rule reaches it for nothing because `record_member_targets` already
//! keys the constructor a `new` selected on the `NEW_EXPR`'s own span, in the same map a call's
//! target lives in. A fifth — dereferencing a nullable value — is deliberately absent. Answering it
//! without false positives needs to know whether a guard ran (`if (x != null)`), and there is no
//! control-flow or definite-assignment layer anywhere below this crate to ask: `jals-hir` has a
//! constant folder and nothing else, and `jals_syntax::CfgMap` is conditional compilation rather
//! than control flow. A rule in `[correctness]` that guessed there would be wrong on ordinary Java.
//!
//! # Two ways to reach a declaration, and what each one settles
//!
//! With a project index the rule asks it: `call_target_of` / `field_target_of` name the member the
//! call or access actually resolves to — the *selected* overload, in whichever file declares it —
//! and [`Member::annotations`] carries what that declaration wrote. That is the half a real project
//! needs, because the `@Nullable` a call has to respect is almost never in the file making the call.
//!
//! Without one (`LintOutput::lint_source`, and any file whose parse the driver rejected) it falls
//! back to what this file alone settles: a name the scope chain binds to a declaration here. The
//! fallback is a strict subset, so the two never disagree — the index answers more, never
//! differently.
//!
//! **What an assignment target has to be**, and which route reaches it, is the same split read from
//! the other side. A **bare name** is this file's scope chain and only it — the memo keys a target
//! on a `CALL_EXPR`, a `NEW_EXPR` or a `FIELD_ACCESS` span, so a `NAME_REF` has no entry to ask
//! for. A **member access** (`o.s`, `this.s`, `a.b.c`) is the index's answer and only the index's:
//! the name after the dot is a bare `IDENT` token rather than a `NAME_REF`, so the file-local pass
//! records no reference for it and the first identifier under the node is the *receiver*. That is
//! why an index-less run does not check one at all, rather than checking the receiver in its place
//! — and why the name a finding uses comes off the same [`Member`] as the verdict, so the two
//! cannot end up describing different slots. An **array element** (`a[0] = null`) is neither: no
//! target is recorded for an `INDEX_EXPR`, and what an element may hold is a nested type-use
//! annotation (`String @Nullable []` against `@Nullable String[]`) this rule does not read — so
//! resolving it to the array variable would check an element against the array's own annotation,
//! which is the conflation [`check_call`](NullnessRule::check_call) refuses for a varargs
//! parameter.
//!
//! # Conservative by construction
//!
//! Only a value whose nullness is *known* is reported, and only into a slot whose nullness is
//! *known*. Everything else is [`Value::Unknown`] and silent:
//!
//! - A member the index did not read annotations for. An embedded stub carries none at all and a
//!   class file's are decoded by `jals-classfile` and not yet lowered, so for those two an empty
//!   annotation list means *nobody looked* rather than *the author wrote none* —
//!   [`ItemOrigin::carries_annotations`] is the question, and `Map.put(k, null)` is what asking it
//!   wrong would report.
//! - A **conditional** (`cond ? find() : "x"`) is unknown even when one arm is nullable. A reader
//!   sees a guarded expression, and reporting the arm would be the false positive this scope was
//!   chosen to avoid.
//! - A name neither route binds — an inherited member, a receiver whose type is unresolved.
//! - An **overloaded** callee, on the file-local route only: the scope chain binds a call to *an*
//!   overload rather than to the one the arguments select. With an index there is no such doubt,
//!   which is one more thing the project route answers rather than skips.
//!
//! # What it reports about a declaration itself
//!
//! One thing: a declaration annotated **both** nullable and non-null — every declaring form the
//! walk visits, an enum constant included. That is not a value flowing anywhere, it is a contract
//! that contradicts itself, and it is the one finding here that needs no second declaration to
//! compare against.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_config::Category;
use jals_config::lint::{Config, Nullness, NullnessMismatch as Options};
use jals_exec::{LocalBoxFuture, Yielder};
use jals_hir::{DefKind, FileAnalysis, FileSemantics, Member, Namespace, TypedFile};
use jals_syntax::SyntaxKind::{
    ASSIGNMENT_EXPR, CALL_EXPR, CONSTRUCTOR_DECL, ENUM_CONSTANT, FIELD_DECL, IDENT, LAMBDA_EXPR,
    LOCAL_VAR_DECL, METHOD_DECL, NEW_EXPR, NULL_KW, PARAM, RESOURCE, RETURN_STMT,
};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{SyntaxElement, SyntaxNode};

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
    check: Checker::Semantic(NullnessRule::check),
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
    /// How a finding names this value, or `None` when there is nothing to report.
    const fn subject(self) -> Option<&'static str> {
        match self {
            Self::Null => Some("`null`"),
            Self::Nullable => Some("a nullable value"),
            Self::Unknown => None,
        }
    }
}

/// A resolved assignment target: the slot's own name, and whether writing `null` into it is a
/// finding.
///
/// The point of the type is that the name travels with the verdict it belongs to. Reading the
/// verdict off one declaration and the name off the first `IDENT` under the target let the two
/// disagree: `a.s = null` reported the receiver `a` while judging the field `s`, and `a[0] = null`
/// reported and judged the array variable for a write into an element.
struct Slot {
    /// How the finding names the slot — the member's or variable's own name, never a receiver's.
    name: String,
    /// Whether the slot's declaration says it never holds `null`.
    rejects_null: bool,
}

/// The nullness vocabulary one file is read with: the configured name lists, plus the file's own
/// single-type imports, which are what turn a written `@Nullable` into a fully-qualified name.
struct Vocabulary<'a> {
    options: &'a Options,
    /// This file's single-type imports, as `ast::Annotations::denoted` takes them.
    imports: Vec<(String, String)>,
}

impl<'a> Vocabulary<'a> {
    /// Reads the file's imports once.
    fn of(root: &SyntaxNode, options: &'a Options) -> Self {
        Self {
            options,
            imports: ast::Annotations::imports_of(root),
        }
    }

    /// An entry's simple name — what an annotation whose own name nothing resolves is matched on.
    fn last_segment(entry: &str) -> &str {
        entry.rsplit('.').next().unwrap_or(entry)
    }

    /// Whether the annotation type `name` denotes is one of `list`.
    ///
    /// `name` arrives from [`ast::Annotations::denoted`] — the same shape whether this file read it
    /// off the tree or the index captured it in the file that declared it — and says which case it
    /// is by carrying a dot or not. A fully-qualified name is compared whole, which is the
    /// precision an FQN list is for: `import com.acme.Nullable;` makes `@Nullable` com.acme's, and
    /// no configured entry matches it. A simple name nothing resolved falls back to matching an
    /// entry's last segment — the same limit `@SuppressWarnings` carries in `crate::suppress`, and
    /// for the same reason: an on-demand import leaves the question open, and resolving the
    /// annotation *type* would need the analysis the rules have not run yet.
    fn matches(list: &[String], name: &str) -> bool {
        if name.contains('.') {
            return list.iter().any(|entry| entry == name);
        }
        list.iter().any(|entry| Self::last_segment(entry) == name)
    }

    /// What a declaration carrying `names` says about `null`.
    fn declared_from(&self, names: &[String]) -> Declared {
        let nullable = names
            .iter()
            .any(|name| Self::matches(&self.options.nullable, name));
        let non_null = names
            .iter()
            .any(|name| Self::matches(&self.options.non_null, name));
        match (nullable, non_null) {
            (true, true) => Declared::Contradictory,
            (true, false) => Declared::Nullable,
            (false, true) => Declared::NonNull,
            (false, false) => Declared::Absent,
        }
    }

    /// The annotation types written on `decl`, read in this file's import context.
    fn denoted_on(&self, decl: &SyntaxNode) -> Vec<String> {
        ast::Annotations::on(decl)
            .iter()
            .filter_map(|annotation| ast::Annotations::denoted(annotation, &self.imports))
            .collect()
    }

    /// What `decl` says about `null`.
    fn declared(&self, decl: &SyntaxNode) -> Declared {
        self.declared_from(&self.denoted_on(decl))
    }

    /// Whether a declaration carrying `names` is a slot that rejects `null`.
    fn rejects_null_from(&self, names: &[String]) -> bool {
        match self.declared_from(names) {
            Declared::NonNull => true,
            // A contradictory declaration is reported as one; nothing is concluded from it.
            Declared::Nullable | Declared::Contradictory => false,
            Declared::Absent => self.options.default == Nullness::NonNull,
        }
    }

    /// Whether reading a declaration carrying `names` may produce `null`.
    fn produces_null_from(&self, names: &[String]) -> bool {
        match self.declared_from(names) {
            Declared::Nullable => true,
            Declared::NonNull | Declared::Contradictory => false,
            Declared::Absent => self.options.default == Nullness::Nullable,
        }
    }

    /// [`rejects_null_from`](Self::rejects_null_from) for a declaration in this file.
    fn rejects_null(&self, decl: &SyntaxNode) -> bool {
        self.rejects_null_from(&self.denoted_on(decl))
    }

    /// [`produces_null_from`](Self::produces_null_from) for a declaration in this file.
    fn produces_null(&self, decl: &SyntaxNode) -> bool {
        self.produces_null_from(&self.denoted_on(decl))
    }
}

/// The `nullness-mismatch` rule.
struct NullnessRule;

impl NullnessRule {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(
        analysis: &'a FileAnalysis,
        project: Option<&'a FileSemantics<'a>>,
        config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(analysis, project, config))
    }

    async fn check_impl(
        analysis: &FileAnalysis,
        project: Option<&FileSemantics<'_>>,
        config: &Config,
    ) -> Vec<Finding> {
        let root = analysis.root();
        let vocabulary = Vocabulary::of(root, &config.correctness.nullness_mismatch.options);
        let typed = match project {
            Some(semantics) => Some(semantics.typed().await),
            None => None,
        };
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            match node.kind() {
                // A `try` resource is a declarator like any other — it declares a name, and a
                // `null` written into it is an NPE at the implicit `close()` rather than a
                // different kind of finding.
                LOCAL_VAR_DECL | FIELD_DECL | RESOURCE => {
                    Self::check_declaration(&node, analysis, typed, &vocabulary, &mut out);
                }
                // An enum constant declares a name too, and writes its annotations as direct
                // children rather than into a `MODIFIERS` child — the second shape
                // `ast::Annotations::on` reads, and the reason it reads two.
                PARAM | METHOD_DECL | CONSTRUCTOR_DECL | ENUM_CONSTANT => {
                    Self::check_contradiction(&node, &vocabulary, &mut out);
                }
                ASSIGNMENT_EXPR => {
                    Self::check_assignment(&node, analysis, typed, &vocabulary, &mut out);
                }
                RETURN_STMT => Self::check_return(&node, analysis, typed, &vocabulary, &mut out),
                CALL_EXPR => Self::check_call(&node, analysis, typed, &vocabulary, &mut out),
                NEW_EXPR => Self::check_new(&node, analysis, typed, &vocabulary, &mut out),
                _ => {}
            }
        }
        out
    }

    /// A declarator's initializer against the declarator's own nullness, plus the contradiction
    /// check every declaring form shares.
    ///
    /// One node can declare several names (`String a = null, b;`), and only the token order says
    /// which initializer belongs to which — which is why the pairs come from the one walk that
    /// knows (`ast::Declarators::initializers`) rather than from the first-name accessor.
    fn check_declaration(
        node: &SyntaxNode,
        analysis: &FileAnalysis,
        typed: Option<TypedFile<'_>>,
        vocabulary: &Vocabulary<'_>,
        out: &mut Vec<Finding>,
    ) {
        Self::check_contradiction(node, vocabulary, out);
        if !vocabulary.rejects_null(node) {
            return;
        }
        for (name, value) in ast::Declarators::initializers(node) {
            let Some(subject) = Self::value_of(&value, analysis, typed, vocabulary).subject()
            else {
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

    /// A simple `=` whose target is a slot one of the two routes resolves.
    fn check_assignment(
        node: &SyntaxNode,
        analysis: &FileAnalysis,
        typed: Option<TypedFile<'_>>,
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
        let Some(slot) = Self::target_slot(&target, analysis, typed, vocabulary) else {
            return;
        };
        if !slot.rejects_null {
            return;
        }
        let Some(subject) = Self::value_of(&value, analysis, typed, vocabulary).subject() else {
            return;
        };
        out.push(Finding::at_range(
            Self::span(value.syntax()),
            format!(
                "{subject} cannot be assigned to `{}`, which is non-null",
                slot.name
            ),
        ));
    }

    /// The slot `target` writes into, or `None` where this rule cannot say which declaration that
    /// is.
    ///
    /// Resolving the *slot* is the whole job, and the shape of the target decides which route can:
    ///
    /// - A **simple name** is what the file-local pass binds, and only it: a bare `NAME_REF` gets
    ///   no entry in the inference memo, so the index has nothing to add here. A name it does not
    ///   bind — an inherited field written unqualified — is therefore silent on both routes.
    /// - A **member access** (`o.s`, `this.s`, `super.s`) is the mirror case: the name on the right
    ///   is a bare `IDENT` token rather than a `NAME_REF`, so the file-local pass records no
    ///   reference for it and only the index can say which member it denotes. The index is keyed on
    ///   the whole node's span, which is what picks the outer `c` out of `a.b.c` rather than the
    ///   inner `a.b` the two share a start with. Asking the *file-local* route here is what named
    ///   the receiver instead of the field, and read the receiver's contract as if it were the
    ///   field's.
    /// - **Everything else** is `None`, and an array element (`a[0]`) is the case that matters:
    ///   what an element may hold is a nested type-use annotation (`String @Nullable []` against
    ///   `@Nullable String[]`) this rule does not read, and the array variable's own annotation is
    ///   a contract about the array rather than about what it holds. Reading one for the other is
    ///   the conflation [`check_call`](Self::check_call) refuses for a varargs trailing parameter.
    fn target_slot(
        target: &ast::Expr,
        analysis: &FileAnalysis,
        typed: Option<TypedFile<'_>>,
        vocabulary: &Vocabulary<'_>,
    ) -> Option<Slot> {
        match target {
            ast::Expr::NameRef(_) => {
                let decl = Self::declaration_of(target.syntax(), analysis, Namespace::Value)?;
                Some(Slot {
                    name: Self::ident_text(target.syntax()),
                    rejects_null: vocabulary.rejects_null(&decl),
                })
            }
            ast::Expr::FieldAccess(_) => {
                let member = Self::member_of(typed, target.syntax())?;
                Some(Slot {
                    name: member.name.clone(),
                    rejects_null: vocabulary.rejects_null_from(&member.annotations),
                })
            }
            _ => None,
        }
    }

    /// A `return` against the nullness its enclosing method declares.
    fn check_return(
        node: &SyntaxNode,
        analysis: &FileAnalysis,
        typed: Option<TypedFile<'_>>,
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
        let Some(subject) = Self::value_of(&value, analysis, typed, vocabulary).subject() else {
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

    /// A call's arguments against the parameters of the method it resolves to.
    ///
    /// The index route needs no arity or overload guard: `call_target_of` names the member the call
    /// selected, so its parameters are the ones this call fills. The file-local route has to guard
    /// both, and does.
    fn check_call(
        node: &SyntaxNode,
        analysis: &FileAnalysis,
        typed: Option<TypedFile<'_>>,
        vocabulary: &Vocabulary<'_>,
        out: &mut Vec<Finding>,
    ) {
        let Some(call) = ast::CallExpr::cast(node.clone()) else {
            return;
        };
        let (Some(callee), Some(args)) = (call.callee(), call.args()) else {
            return;
        };
        let arguments: Vec<ast::Expr> = args.args().collect();
        if let Some(member) = Self::member_of(typed, node) {
            Self::check_member_arguments(member, &arguments, analysis, typed, vocabulary, out);
            return;
        }
        let Some(method) = Self::declaration_of(callee.syntax(), analysis, Namespace::Method)
            .and_then(ast::MethodDecl::cast)
        else {
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
        // Guessing the pairing across an arity mismatch would report against a parameter the call
        // never fills.
        if params.len() != arguments.len() {
            return;
        }
        let name = method.name().unwrap_or_default();
        for (param, argument) in params.iter().zip(&arguments) {
            if !vocabulary.rejects_null(param.syntax()) {
                continue;
            }
            Self::report_argument(
                argument,
                &param.name().unwrap_or_default(),
                &name,
                analysis,
                typed,
                vocabulary,
                out,
            );
        }
    }

    /// A `new C(…)`'s arguments against the constructor the index says it selected.
    ///
    /// The index route only, and deliberately no file-local fallback beside it: a call has a name
    /// the scope chain can bind and then be guarded for overloading, while a constructor's
    /// declarations all share the type's name, so there is nothing the file-local pass could bind
    /// that would say *which* one this `new` reaches. An anonymous class body changes none of that
    /// — the target is still the superclass constructor the arguments select — and an array
    /// creation (`new String[]{null}`) has no `ArgList` at all, so it never arrives here.
    fn check_new(
        node: &SyntaxNode,
        analysis: &FileAnalysis,
        typed: Option<TypedFile<'_>>,
        vocabulary: &Vocabulary<'_>,
        out: &mut Vec<Finding>,
    ) {
        let Some(args) = ast::NewExpr::cast(node.clone()).and_then(|new| new.args()) else {
            return;
        };
        let Some(member) = Self::member_of(typed, node) else {
            return;
        };
        let arguments: Vec<ast::Expr> = args.args().collect();
        Self::check_member_arguments(member, &arguments, analysis, typed, vocabulary, out);
    }

    /// Each argument against the parameter of `member` it fills — the index route's body, shared by
    /// a call and a `new` so the pairing rule is written once.
    ///
    /// No arity guard is needed and none is written: a target is recorded only from a *selected*
    /// overload, and selection admits a non-varargs member only at `params.len() == args.len()`
    /// (JLS §15.12.2.2, and `resolve_explicit_constructor` for the `this(…)` / `super(…)` forms).
    /// A varargs member is the one that can disagree, and only past its fixed prefix — the trailing
    /// parameter stands for any number of arguments, so pairing it with one of them would check an
    /// element against the array's own annotation.
    fn check_member_arguments(
        member: &Member,
        arguments: &[ast::Expr],
        analysis: &FileAnalysis,
        typed: Option<TypedFile<'_>>,
        vocabulary: &Vocabulary<'_>,
        out: &mut Vec<Finding>,
    ) {
        let fixed = if member.varargs {
            member.params.len().saturating_sub(1)
        } else {
            member.params.len()
        };
        for (param, argument) in member.params.iter().take(fixed).zip(arguments) {
            if !vocabulary.rejects_null_from(&param.annotations) {
                continue;
            }
            Self::report_argument(
                argument,
                param.name.as_deref().unwrap_or_default(),
                &member.name,
                analysis,
                typed,
                vocabulary,
                out,
            );
        }
    }

    /// One argument's finding, shared by the two routes so they word it identically.
    fn report_argument(
        argument: &ast::Expr,
        param: &str,
        method: &str,
        analysis: &FileAnalysis,
        typed: Option<TypedFile<'_>>,
        vocabulary: &Vocabulary<'_>,
        out: &mut Vec<Finding>,
    ) {
        let Some(subject) = Self::value_of(argument, analysis, typed, vocabulary).subject() else {
            return;
        };
        out.push(Finding::at_range(
            Self::span(argument.syntax()),
            format!(
                "{subject} cannot be passed to parameter `{param}` of `{method}`, which is non-null"
            ),
        ));
    }

    /// What `value` is known to produce.
    fn value_of(
        value: &ast::Expr,
        analysis: &FileAnalysis,
        typed: Option<TypedFile<'_>>,
        vocabulary: &Vocabulary<'_>,
    ) -> Value {
        match value {
            // Parentheses change nothing about what the expression produces.
            ast::Expr::Paren(paren) => paren.expr().map_or(Value::Unknown, |inner| {
                Self::value_of(&inner, analysis, typed, vocabulary)
            }),
            ast::Expr::Literal(literal) => {
                if literal
                    .syntax()
                    .children_with_tokens()
                    .filter_map(SyntaxElement::into_token)
                    .any(|token| token.kind() == NULL_KW)
                {
                    Value::Null
                } else {
                    Value::Unknown
                }
            }
            ast::Expr::Call(_) | ast::Expr::FieldAccess(_) | ast::Expr::NameRef(_) => {
                Self::read_value(value, analysis, typed, vocabulary)
            }
            _ => Value::Unknown,
        }
    }

    /// The nullness of a name, field access or call — the index first, this file second.
    fn read_value(
        value: &ast::Expr,
        analysis: &FileAnalysis,
        typed: Option<TypedFile<'_>>,
        vocabulary: &Vocabulary<'_>,
    ) -> Value {
        if let Some(member) = Self::member_of(typed, value.syntax()) {
            return if vocabulary.produces_null_from(&member.annotations) {
                Value::Nullable
            } else {
                Value::Unknown
            };
        }
        let (node, namespace) = match value {
            ast::Expr::NameRef(_) => (value.syntax().clone(), Namespace::Value),
            ast::Expr::Call(call) => {
                let Some(callee) = call.callee() else {
                    return Value::Unknown;
                };
                // Without an index the scope chain binds a call to *an* overload rather than to the
                // one the arguments select, so an annotation read off it is about a declaration
                // this call may never reach.
                if Self::is_overloaded(
                    analysis,
                    Self::declaration_of(callee.syntax(), analysis, Namespace::Method)
                        .and_then(ast::MethodDecl::cast)
                        .and_then(|method| method.name())
                        .as_deref(),
                ) {
                    return Value::Unknown;
                }
                (callee.syntax().clone(), Namespace::Method)
            }
            // A member access records no reference at all in the file-local pass — the right-hand
            // name is a bare token — so without an index there is nothing to read.
            _ => return Value::Unknown,
        };
        Self::declaration_of(&node, analysis, namespace)
            .filter(|decl| vocabulary.produces_null(decl))
            .map_or(Value::Unknown, |_| Value::Nullable)
    }

    /// The member `node` resolves to, when there is an index **and** it read that member's
    /// annotations.
    ///
    /// The second half is the whole point of [`ItemOrigin::carries_annotations`]: an embedded stub
    /// carries no annotations at all and a class file's are not lowered yet, so an empty list there
    /// means *nobody looked*. Reading it as *the author wrote none* is what would report
    /// `map.put(k, null)` against a method that documents itself as taking one.
    ///
    /// [`ItemOrigin::carries_annotations`]: jals_hir::ItemOrigin::carries_annotations
    fn member_of<'s>(typed: Option<TypedFile<'s>>, node: &SyntaxNode) -> Option<&'s Member> {
        let typed = typed?;
        // The inference memo is keyed on the node's own range, leading trivia included — not on the
        // significant span a finding is ranged with.
        let span = Self::memo_span(node);
        let id = typed
            .call_target_of(span.clone())
            .or_else(|| typed.field_target_of(span))?;
        let member = typed.index().member(id);
        typed
            .index()
            .item(member.owner)
            .origin
            .carries_annotations()
            .then_some(member)
    }

    /// The declaration `node` names, when this file binds it in `namespace`.
    ///
    /// `None` for everything the file-local pass leaves open — an inherited member, a name another
    /// file declares, the right-hand side of a member access. That is the silence the fallback
    /// route's conservatism rests on.
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

    /// A node's own byte range — the key the inference memo records against.
    fn memo_span(node: &SyntaxNode) -> Range<usize> {
        let range = node.text_range();
        usize::from(range.start())..usize::from(range.end())
    }

    /// A node's significant span, falling back to its own range when it holds no significant token.
    fn span(node: &SyntaxNode) -> Range<usize> {
        Significant::range(node).unwrap_or_else(|| Self::memo_span(node))
    }

    /// The text of `node`'s own identifier token — how a finding names a simple-name slot.
    ///
    /// Direct children only, because that is the difference between a name and a name's receiver:
    /// a `NAME_REF` holds exactly one, while descending into a `FIELD_ACCESS` would find the
    /// receiver's first.
    fn ident_text(node: &SyntaxNode) -> String {
        node.children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|token| token.kind() == IDENT)
            .map_or_else(String::new, |token| token.text().to_owned())
    }
}
