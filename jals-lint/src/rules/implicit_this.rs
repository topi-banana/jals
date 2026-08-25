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

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::vec::Vec;

use jals_exec::{LocalBoxFuture, Yielder};
use jals_hir::{Def, DefKind, FileAnalysis, Namespace};
use jals_syntax::SyntaxKind::{
    ANNOTATION_TYPE_DECL, CLASS_BODY, CLASS_DECL, CONSTRUCTOR_DECL, ENUM_CONSTANT, ENUM_DECL,
    FIELD_DECL, INITIALIZER, INTERFACE_DECL, METHOD_DECL, MODIFIERS, NAME_REF, NEW_EXPR,
    PARAM_LIST, RECORD_COMPONENT, RECORD_DECL, STATIC_KW,
};
use jals_syntax::ast::{AstNode, FieldDecl, RecordComponent};
use jals_syntax::{SyntaxNode, SyntaxToken};

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
        let root = analysis.root();
        let mut yielder = Yielder::new();

        // One walk builds both indexes: the instance fields a reference may be reported against,
        // and the `NAME_REF` each reference sits in. Both are keyed on the identifier token's
        // start, which is what `Def::name_range` and `Reference::range` both begin at.
        let mut instance_fields: BTreeMap<usize, SyntaxNode> = BTreeMap::new();
        let mut name_refs: BTreeMap<usize, SyntaxNode> = BTreeMap::new();
        for node in root.descendants() {
            yielder.tick().await;
            match node.kind() {
                FIELD_DECL => {
                    if Self::is_static(&node) || Self::in_implicitly_static_type(&node) {
                        continue;
                    }
                    if let Some(decl) = FieldDecl::cast(node.clone()) {
                        for tok in decl.names() {
                            instance_fields.insert(Self::start(&tok), node.clone());
                        }
                    }
                }
                // A record component is the `private final` field the record declares, and
                // `this.x` names it — so it is an instance field like any other. Its declaration
                // sits in the `RECORD_HEADER`, outside the body, which is why the type identity
                // below is the *declaration* and not the body node.
                RECORD_COMPONENT => {
                    if let Some(tok) =
                        RecordComponent::cast(node.clone()).and_then(|c| c.name_token())
                    {
                        instance_fields.insert(Self::start(&tok), node.clone());
                    }
                }
                NAME_REF => {
                    if let Some(tok) = node
                        .children_with_tokens()
                        .filter_map(jals_syntax::SyntaxElement::into_token)
                        .find(|t| t.kind() == jals_syntax::SyntaxKind::IDENT)
                    {
                        name_refs.insert(Self::start(&tok), node.clone());
                    }
                }
                _ => {}
            }
        }

        let mut out = Vec::new();
        for reference in analysis.references() {
            yielder.tick().await;
            if reference.namespace != Namespace::Value {
                continue;
            }
            let Some(def) = reference.resolution.def_id().map(|id| analysis.def(id)) else {
                continue;
            };
            if def.kind != DefKind::Field {
                continue;
            }
            // Absent from the table means `static`, and a `static` field is qualified by its type
            // rather than by `this`.
            let (Some(decl), Some(site)) = (
                instance_fields.get(&def.name_range.start),
                name_refs.get(&reference.range.start),
            ) else {
                continue;
            };
            // The declaring type must be the innermost type around the reference. This one test
            // is what keeps a nested, local or anonymous type — where `this.` denotes the inner
            // instance — out, and what lets a lambda body through, since a lambda declares no type.
            if Self::enclosing_type(site) != Self::enclosing_type(decl) {
                continue;
            }
            let Some(executable) = Self::enclosing_executable(site) else {
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
    fn is_static(node: &SyntaxNode) -> bool {
        node.children()
            .find(|c| c.kind() == MODIFIERS)
            .is_some_and(|m| {
                m.children_with_tokens()
                    .filter_map(jals_syntax::SyntaxElement::into_token)
                    .any(|t| t.kind() == STATIC_KW)
            })
    }

    /// Whether `node` is declared in a type whose fields are `static` without saying so: an
    /// interface or an annotation type (JLS §9.3, implicitly `public static final`).
    fn in_implicitly_static_type(node: &SyntaxNode) -> bool {
        Self::enclosing_type(node)
            .is_some_and(|t| matches!(t.kind(), INTERFACE_DECL | ANNOTATION_TYPE_DECL))
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

    /// A token's start offset, as the byte index the analysis keys its ranges on.
    fn start(token: &SyntaxToken) -> usize {
        usize::from(token.text_range().start())
    }
}
