//! The passes that surround the layout engine.
//!
//! `DESIGN.md`'s L0 and L4: everything that is not "lower a node into a document". They fall into
//! four groups.
//!
//! **Token-changing (L0).** [`ImportPlan`], [`UnusedImports`], and [`ModifierOrder`] each build a
//! *plan* over the original nodes rather than rewriting the tree, so comments travel with the tokens
//! they are anchored to. [`Granularity`] is the one that cannot promise that — merging and
//! splitting a grouped import change the token multiset by construction — so it carries a
//! different promise instead, [`ImportNames`], which the fail-safe checks. [`LiteralRewrite`] changes a token's spelling; it runs inside
//! `Ctx::token` rather than as a tree pass, which is why it is filed by what it changes and not by
//! when it runs.
//!
//! **Text (L4).** [`StringWrapper`] re-parses the formatted output and re-splits over-long string
//! concatenations, adopting the result only if it is a fixed point — and it *does* add `+` operators
//! when it splits a lone literal, so it is not multiset-preserving. [`Finalize`] applies
//! `[layout]`'s line-level rules.
//!
//! **Guards.** [`OffOn`] locates regions that must survive byte-identical, and is consumed as a
//! `Ctx` field rather than run as a pipeline stage. [`TokenBudget`] is the fail-safe that decides
//! whether the whole run is trustworthy, and [`token_license`] is the declaration of what it will
//! accept — the one place that enumerates the token-changing operations, including the one with no
//! config key. Which of these passes changes tokens, and how, is that table's answer and not this
//! paragraph's.
//!
//! **The driver.** [`Formatter`] sequences all of them around [`visit`](crate::visit)'s lowering.
//! It is `DESIGN.md` §8.1's seam **S4** — which passes run, in what order — so the gates live here
//! rather than at a call site inside the lowering walk.

pub(crate) mod finalize;
pub(crate) mod import_granularity;
pub(crate) mod import_order;
pub(crate) mod literals;
pub(crate) mod modifier_order;
pub(crate) mod off_on;
pub(crate) mod pipeline;
pub(crate) mod string_wrapper;
pub(crate) mod token_budget;
pub(crate) mod token_license;
pub(crate) mod unused_imports;

pub(crate) use import_granularity::Unit;
pub(crate) use import_order::ImportPlan;
