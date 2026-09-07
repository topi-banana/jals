//! The packages this crate ships.
//!
//! One today. They live here rather than in a crate of their own for the reason `jals-hir`'s
//! standard-library stubs live beside the index that reads them: the crate that says what a
//! package *is* is also the natural place for the first one, and an author writing a second one
//! depends on this crate and nothing else either way.

pub mod jals_io;
