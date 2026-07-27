//! The passes that surround the layout engine.
//!
//! `DESIGN.md`'s L0 and L4: everything that is not "lower a node into a document". They fall into
//! three groups.
//!
//! **Token-changing (L0).** [`ImportPlan`], [`UnusedImports`], and [`ModifierOrder`] are the only
//! passes that touch the significant-token sequence, and each is a *plan* over the original nodes
//! rather than a tree rewrite — so the multiset is preserved by construction and comments travel
//! with the tokens they are anchored to. [`LiteralRewrite`] changes a token's spelling and nothing
//! else. All four are off or `preserve` by default.
//!
//! **Text (L4).** [`StringWrapper`] re-parses the formatted output and re-splits over-long string
//! concatenations, adopting the result only if it is a fixed point. [`Finalize`] applies
//! `[layout]`'s line-level rules.
//!
//! **Guards.** [`OffOn`] locates regions that must survive byte-identical; [`TokenBudget`] is the
//! fail-safe that decides whether the whole run is trustworthy.

pub(crate) mod finalize;
pub(crate) mod import_order;
pub(crate) mod literals;
pub(crate) mod modifier_order;
pub(crate) mod off_on;
pub(crate) mod string_wrapper;
pub(crate) mod token_budget;
pub(crate) mod unused_imports;

pub(crate) use finalize::Finalize;
pub(crate) use import_order::ImportPlan;
pub(crate) use literals::LiteralRewrite;
pub(crate) use modifier_order::ModifierOrder;
pub(crate) use off_on::OffOn;
pub(crate) use string_wrapper::StringWrapper;
pub(crate) use token_budget::TokenBudget;
pub(crate) use unused_imports::UnusedImports;
