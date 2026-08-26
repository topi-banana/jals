//! `implicit-this`: an instance field read or written by simple name where `this.` would qualify
//! it.
//!
//! The rule a project enables when it wants a field distinguishable from a local by the spelling
//! alone — checkstyle's `RequireThis`. Neither rustc nor clippy carries an analogue: Rust has no
//! implicit receiver to leave out.
//!
//! **What makes the detection cheap is a fact about the resolver, not a filter here.** `this.x`
//! parses as a `FIELD_ACCESS` whose member name is a bare `IDENT` token rather than a `NAME_REF`,
//! and `jals-hir` records a reference only for a `NAME_REF` carrying an identifier. So a
//! [`Reference`](jals_hir::Reference) that is a [`Value`](Namespace::Value) and resolves to a
//! [`Field`](DefKind::Field) is *already* an unqualified simple name — there is no qualified form
//! to exclude. Shadowing needs no handling either: a local hiding a field wins the scope-chain
//! lookup, so `x` inside `int x = 1;`'s scope resolves to the local and never reaches this rule.
//!
//! [`ThisScope`] chooses which references are required to carry the qualifier.
//! [`ShadowedOnly`](ThisScope::ShadowedOnly) is a true subset of
//! [`Always`](ThisScope::Always) — the same walk, filtered by whether the enclosing executable
//! also declares that name — so the two answers share one detection path.
//!
//! Three limits are documented rather than solved:
//!
//! - **An inherited field is out of scope.** A simple name naming a superclass field stays
//!   [`Unresolved`](jals_hir::Resolution::Unresolved) in the file-local pass, and binding it needs
//!   a `ProjectIndex` — which would make the rule silent whenever a host supplies no project,
//!   which is exactly how `LintOutput::lint_source` calls the engine.
//! - **The enclosing type must be the declaring one.** A field reached from inside a nested,
//!   local or anonymous type is not reported, because `this.` there denotes the *inner* instance
//!   and the qualifier Java wants is `Outer.this.`. Where the inner type happens to *inherit* the
//!   field — an anonymous subclass of the enclosing class, an `enum` constant with a body — that
//!   is a deliberate miss rather than an oversight: conservative is the right side for a rule
//!   nobody enabled by default.
//! - **A record's compact constructor is skipped.** A component is a real `private final` field,
//!   so `this.x` is right in an ordinary record method and the reference is reported there. Inside
//!   `Point { ... }` the same spelling denotes the implicit parameter, which no qualifier can
//!   name.

use alloc::format;
use alloc::vec::Vec;

use jals_exec::{LocalBoxFuture, Yielder};
use jals_hir::{Def, DefKind, FileAnalysis, Namespace};
use jals_syntax::SyntaxKind::{
    ANNOTATION_TYPE_DECL, CLASS_BODY, CLASS_DECL, CONSTRUCTOR_DECL, ENUM_CONSTANT, ENUM_DECL,
    FIELD_DECL, INITIALIZER, INTERFACE_DECL, METHOD_DECL, MODIFIERS, NEW_EXPR, PARAM_LIST,
    RECORD_DECL, STATIC_KW,
};
use jals_syntax::SyntaxNode;

use jals_config::Category;
use jals_config::lint::{Config, ThisScope};

use crate::rules::{Checker, Finding, RuleMeta, Significant};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "implicit-this",
    category: Category::Restriction,
    level: |config| config.restriction.implicit_this.level,
    needs_clean_parse: false,
    check: Checker::Analyzed(ImplicitThis::check),
};

/// The `implicit-this` rule.
struct ImplicitThis;

impl ImplicitThis {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(
        analysis: &'a FileAnalysis,
        config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(analysis, config))
    }

    async fn check_impl(analysis: &FileAnalysis, config: &Config) -> Vec<Finding> {
        let scope = config.restriction.implicit_this.options.scope;
        let mut yielder = Yielder::new();

        let mut out = Vec::new();
        for reference in analysis.references() {
            yielder.tick().await;
            if reference.namespace != Namespace::Value {
                continue;
            }
            let Some(def) = reference.resolution.def_id().map(|id| analysis.def(id)) else {
                continue;
            };
            // A `static` field is qualified by its type rather than by `this`. `Def::is_static`
            // already folds in the set JLS §9.3 implies, so an interface field needs no separate
            // ancestor check here — and a record component, which `jals-hir` registers as a field
            // like any other, is correctly not one.
            if def.kind != DefKind::Field || def.is_static {
                continue;
            }
            let (Some(site), Some(decl)) = (analysis.site_of(reference), analysis.decl_of(def))
            else {
                continue;
            };
            // The declaring type must be the innermost type around the reference. This one test
            // is what keeps a nested, local or anonymous type — where `this.` denotes the inner
            // instance — out, and what lets a lambda body through, since a lambda declares no type.
            if Self::enclosing_type(&site) != Self::enclosing_type(&decl) {
                continue;
            }
            let Some(executable) = Self::enclosing_executable(&site) else {
                continue;
            };
            if Self::is_static(&executable) || Self::is_compact_constructor(&executable) {
                continue;
            }
            if scope == ThisScope::ShadowedOnly && !Self::shadowed_in(analysis, &executable, def) {
                continue;
            }
            out.push(Finding::at_range(
                reference.range.clone(),
                format!("field `{}` should be qualified with `this.`", def.name),
            ));
        }
        out
    }

    /// The innermost declaration that introduces a type around `node`, itself included.
    ///
    /// A type declaration is one; so is the body of an anonymous class or of an `enum` constant,
    /// which introduce a type with no declaration of their own. A lambda is deliberately not one:
    /// it runs in its enclosing instance, so a field reference inside one is still unqualified.
    fn enclosing_type(node: &SyntaxNode) -> Option<SyntaxNode> {
        node.ancestors().find(|a| match a.kind() {
            CLASS_DECL | INTERFACE_DECL | ENUM_DECL | RECORD_DECL | ANNOTATION_TYPE_DECL => true,
            CLASS_BODY => a
                .parent()
                .is_some_and(|p| matches!(p.kind(), NEW_EXPR | ENUM_CONSTANT)),
            _ => false,
        })
    }

    /// The innermost executable or initializing declaration around `node`, stopping at the type
    /// that encloses it.
    ///
    /// `FIELD_DECL` is in the set because an instance field initializer (`int b = a + 1;`) runs in
    /// the same instance context a constructor does, and checkstyle checks it. Stopping at the
    /// type keeps a name written outside any of them — a supertype's type argument, say — from
    /// being read as if it sat in the previous member.
    fn enclosing_executable(node: &SyntaxNode) -> Option<SyntaxNode> {
        let boundary = Self::enclosing_type(node);
        node.ancestors()
            .take_while(|a| boundary.as_ref() != Some(a))
            .find(|a| {
                matches!(
                    a.kind(),
                    METHOD_DECL | CONSTRUCTOR_DECL | INITIALIZER | FIELD_DECL
                )
            })
    }

    /// Whether `node` writes the `static` modifier.
    ///
    /// Asked of the enclosing *executable*, not of the field — a field's staticness is
    /// [`Def::is_static`]. `jals-hir` registers no definition for an `INITIALIZER`, so a `static`
    /// initializer block has no `Def` to ask and this reads its modifiers directly.
    fn is_static(node: &SyntaxNode) -> bool {
        node.children()
            .find(|c| c.kind() == MODIFIERS)
            .is_some_and(|m| {
                m.children_with_tokens()
                    .filter_map(jals_syntax::SyntaxElement::into_token)
                    .any(|t| t.kind() == STATIC_KW)
            })
    }

    /// Whether `executable` is a record's compact constructor — a `CONSTRUCTOR_DECL` with no
    /// parameter list, whose component-named locals are parameters no qualifier can reach.
    fn is_compact_constructor(executable: &SyntaxNode) -> bool {
        executable.kind() == CONSTRUCTOR_DECL
            && !executable.children().any(|c| c.kind() == PARAM_LIST)
    }

    /// Whether `executable` also declares a binding named like `def` — the condition
    /// [`ShadowedOnly`](ThisScope::ShadowedOnly) reports on.
    ///
    /// The span is [`Significant::range`] rather than the node range: rowan parks a member's
    /// leading trivia inside it, so the node range reaches back into the previous member.
    fn shadowed_in(analysis: &FileAnalysis, executable: &SyntaxNode, def: &Def) -> bool {
        let Some(span) = Significant::range(executable) else {
            return false;
        };
        analysis.defs().iter().any(|other| {
            matches!(
                other.kind,
                DefKind::Local
                    | DefKind::Param
                    | DefKind::LambdaParam
                    | DefKind::CatchParam
                    | DefKind::Resource
                    | DefKind::PatternVar
            ) && other.name == def.name
                && span.start <= other.name_range.start
                && other.name_range.end <= span.end
        })
    }
}
