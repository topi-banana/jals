//! Pluggable formatter rules: the opt-in transformations gathered behind traits and a
//! per-format [`Registry`].
//!
//! Two trait shapes capture the rule families that suit abstraction:
//! - [`LiteralRule`] — a pure text rewrite of a numeric-literal token (`hex-literal-case`,
//!   `float-literal-trailing-zero`, `literal-suffix-case`). Homogeneous and composable, so the
//!   active ones are collected into a [`LiteralRegistry`] and applied in turn.
//! - [`StructuralRule`] — a node-level lowering a rule owns wholesale (`imports` reordering for the
//!   source file, `modifiers` reordering for a `MODIFIERS` node). Dispatched from `lower`'s `match`
//!   by kind, so [`Registry::structural`] is a static O(1) lookup, never a per-node linear scan.
//!
//! Layout-affecting options (brace style, parameter layout, operator separators, …) are *not*
//! modeled here: they are structural dispatch woven through `lower` / `render`, not a discrete list
//! of rules, and `dyn`-dispatching them would add vtable hops on the formatter's hottest path
//! without untangling anything. `trailing-comma` likewise stays a plain function
//! ([`trailing_comma::doc`]) — it has a single static call site and nothing to iterate.

use alloc::string::String;

use jals_exec::LocalBoxFuture;
use jals_syntax::{SyntaxKind as S, SyntaxNode};

use crate::config::Config;
use crate::doc::Doc;
use crate::lower::Ctx;

pub(crate) mod imports;
pub(crate) mod literals;
pub(crate) mod modifiers;
pub(crate) mod parameter_comment;
pub(crate) mod trailing_comma;

pub(crate) use literals::LiteralRegistry;

/// A pure rewrite of a numeric-literal token's text. An implementor is built from `&Config`
/// (reading its own option) and carries the resolved, non-`Preserve` policy, so `rewrite` needs no
/// `Config`. Returns the rewritten text, or `None` to leave the token unchanged.
pub(crate) trait LiteralRule {
    fn rewrite(&self, text: &str, kind: S) -> Option<String>;
}

/// A node-level lowering a rule owns wholesale. Implementors are zero-sized handles held by the
/// [`Registry`]; their `lower` reads `ctx.cfg` for the gating options exactly as the prior free
/// functions did. The rule recurses back into the async [`Ctx::lower`] walk, and the trait is a
/// `dyn` object, so the method returns the boxed future (one box per structural node).
pub(crate) trait StructuralRule {
    fn lower<'t>(&'t self, node: &'t SyntaxNode, ctx: &'t Ctx<'t>) -> LocalBoxFuture<'t, Doc>;
}

/// The per-format rule set, built once from `&Config` and carried on [`Ctx`].
pub(crate) struct Registry {
    literals: LiteralRegistry,
    imports: imports::ImportRule,
    modifiers: modifiers::ModifierRule,
}

impl Registry {
    /// Resolve the active rules from `cfg`. Literal rules whose option is `Preserve` are omitted,
    /// so the default config yields an empty literal chain.
    pub(crate) fn from_config(cfg: &Config) -> Self {
        Self {
            literals: LiteralRegistry::from_config(cfg),
            imports: imports::ImportRule,
            modifiers: modifiers::ModifierRule,
        }
    }

    /// The literal-rewrite chain applied in [`crate::lower`]'s token emission.
    pub(crate) const fn literals(&self) -> &LiteralRegistry {
        &self.literals
    }

    /// The structural rule that owns lowering for `kind`, if any. A static `match` — the same O(1)
    /// dispatch the `lower` arms were — so most nodes pay only a single comparison.
    pub(crate) fn structural(&self, kind: S) -> Option<&dyn StructuralRule> {
        match kind {
            S::SOURCE_FILE => Some(&self.imports),
            S::MODIFIERS => Some(&self.modifiers),
            _ => None,
        }
    }
}
