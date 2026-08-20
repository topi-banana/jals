//! `boxed-primitive-constructor`: `new Integer(1)` and its siblings, where `Integer.valueOf(1)`
//! allocates nothing.
//!
//! The Java analogue of `clippy::box_default`. Every wrapper class publishes a `valueOf` that
//! returns a cached instance for the common range, so the constructor's only effect over it is a
//! guaranteed allocation and the loss of `==` identity for values the cache would have shared.
//! The constructors have also been deprecated since Java 9 — which is a `[compatibility]`
//! statement, while the allocation is what the program keeps paying for as long as it still
//! compiles, so the rule is filed under what it costs.
//!
//! Matched on the constructed type's **simple name**, spelled either bare (`new Integer(…)`) or
//! fully qualified (`new java.lang.Integer(…)`). A project that declares its own `Integer` is the
//! recognized false positive, and it is the reason the rule reads the *name*: resolving the type
//! would make this a project-aware rule for a finding that a name settles in every codebase that
//! does not shadow `java.lang`.
//!
//! The name comes from [`Type::simple_name`], which is the last **top-level** `IDENT` of the type.
//! Reading the last `IDENT` of the whole subtree instead would name the type *argument*, so
//! `new ArrayList<Integer>()` would be reported as a boxed `Integer` — which is the shape half of
//! real Java is written in.

use alloc::vec::Vec;

use jals_config::Category;
use jals_config::lint::Config;
use jals_exec::{LocalBoxFuture, Yielder};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{AstNode, NewExpr, Type};

use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "boxed-primitive-constructor",
    category: Category::Performance,
    level: |config| config.performance.boxed_primitive_constructor.level,
    needs_clean_parse: false,
    check: Checker::Syntactic(api::check),
};

/// The `boxed-primitive-constructor` rule.
mod api {
    use super::{
        AstNode, Config, Finding, LocalBoxFuture, NewExpr, SyntaxNode, Type, Vec, Yielder,
    };

    /// The eight `java.lang` wrappers, each of which publishes a caching `valueOf`.
    const WRAPPERS: &[&str] = &[
        "Boolean",
        "Byte",
        "Character",
        "Short",
        "Integer",
        "Long",
        "Float",
        "Double",
    ];

    /// The table-edge shim: boxes the async rule body once per file.
    pub(crate) fn check<'a>(
        root: &'a SyntaxNode,
        _config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(check_impl(root))
    }

    async fn check_impl(root: &SyntaxNode) -> Vec<Finding> {
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            let Some(new) = NewExpr::cast(node) else {
                continue;
            };
            // An anonymous subclass (`new Integer(1) {}`) is a different construct: `valueOf`
            // cannot express it, so the rewrite the message asks for does not exist. An array
            // creation (`new Integer[10]`) has no argument list at all, and allocates the array
            // rather than a wrapper — `valueOf` has nothing to say about it.
            if new.body().is_some() || new.args().is_none() {
                continue;
            }
            let Some(wrapper) = new.ty().as_ref().and_then(wrapper_name) else {
                continue;
            };
            out.push(Finding::at_node(
                new.syntax(),
                alloc::format!("`new {wrapper}(…)` always allocates; use `{wrapper}.valueOf(…)`"),
            ));
        }
        out
    }

    /// The wrapper class `ty` names, if it names one. `java.lang.Integer` and `Integer` answer
    /// alike, and `ArrayList<Integer>` answers `None` — [`Type::simple_name`] takes the last
    /// *top-level* `IDENT`, so a type argument is not the name.
    fn wrapper_name(ty: &Type) -> Option<&'static str> {
        let name = ty.simple_name()?;
        WRAPPERS.iter().copied().find(|wrapper| *wrapper == name)
    }
}
