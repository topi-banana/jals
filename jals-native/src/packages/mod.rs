//! The packages this crate ships.
//!
//! Two. They live here rather than in a crate of their own for the reason `jals-hir`'s
//! standard-library stubs live beside the index that reads them: the crate that says what a
//! package *is* is also the natural place for the ones it ships, and an author writing a third
//! depends on this crate and nothing else either way.
//!
//! - [`jals_io`] is the smallest thing this seam can be — three `native` methods and a `char[]`,
//!   and no dependency on a lowering rule that did not already exist.
//! - [`java_base`] is the largest — the `java.lang` and `java.io` that `jals-hir` publishes
//!   signature-only stubs of, with the bodies the stubs do not have.
//!
//! A project may select either, both, or neither. They overlap in nothing: `jals.io` writes
//! `char[]` and declares no type the JDK does, and `java.base` declares only types the JDK does.

pub mod jals_io;
pub mod java_base;
