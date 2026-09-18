//! The platform library, for this crate's own tests.
//!
//! One place rather than one per test module. Every unit test here that indexes anything needs a
//! `java.lang` behind it — a `String` return type, an implicit `Object` supertype — and each
//! writing its own would be one more thing to update when the tier vocabulary moves.

use alloc::vec::Vec;

/// The platform, for a test.
pub(crate) struct TestPlatform;

impl TestPlatform {
    /// Every platform unit at **signature** fidelity: what a `javac` build and every editor
    /// session index, and what these tests want unless they are about linking.
    pub(crate) fn records() -> Vec<jals_hir::LibraryFile> {
        jals_exec::block_on_inline(jals_hir::LibraryFile::parse_tiers(
            &jals_platform::JavaBase::tiers(false),
        ))
    }
}
