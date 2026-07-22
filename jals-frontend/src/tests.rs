extern crate std;

use alloc::borrow::ToOwned;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use jals_exec::block_on_inline;
use jals_storage::{ArtifactCache, MemoryCache, RelativePath};

use crate::driver::Driver;
use crate::ir::{IrFile, LoweredFile, LoweredTree};
use crate::key::FrontendKey;
use crate::vanilla::VanillaFrontend;

/// Fixture namespace for these tests.
struct Fixture;

impl Fixture {
    fn file(path: &str, bytes: &[u8]) -> IrFile {
        IrFile::new(RelativePath::parse(path).unwrap(), Arc::from(bytes))
    }

    /// Two sources, in canonical order.
    fn sources() -> Vec<IrFile> {
        let mut files = vec![
            Self::file("src/main/java/Main.java", b"class Main {}\n"),
            Self::file("src/main/java/Util.java", b"class Util {}\n"),
        ];
        FrontendKey::canonical_order(&mut files);
        files
    }
}

#[test]
fn vanilla_emits_every_input_unchanged() {
    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let lowered = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &files)).unwrap();

    assert_eq!(lowered.tree.files().len(), files.len());
    for (input, output) in files.iter().zip(lowered.tree.files()) {
        assert_eq!(input.path, output.path);
        let bytes = block_on_inline(cache.lookup(&output.key)).unwrap().unwrap();
        assert_eq!(bytes.as_slice(), &input.bytes[..]);
    }
}

/// Pins the provenance fold to a literal.
///
/// Every other test here compares the fold against code that would change alongside it. This
/// one compares against a constant, so a reordered or dropped field turns into a red test
/// rather than a silent invalidation that only surfaces as mysteriously cold rebuilds.
#[test]
fn frontend_out_key_is_stable() {
    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let lowered = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &files)).unwrap();

    let keys: Vec<_> = lowered
        .tree
        .files()
        .iter()
        .map(|file| file.key.provenance().to_hex())
        .collect();

    assert_eq!(
        keys,
        vec![
            FROZEN_MAIN_PROVENANCE.to_owned(),
            FROZEN_UTIL_PROVENANCE.to_owned(),
        ],
        "the provenance fold changed; if that was deliberate, update these literals and \
         understand that it makes every cached frontend output in the wild unreachable"
    );
}

#[test]
fn tree_digest_is_independent_of_construction_order() {
    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let lowered = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &files)).unwrap();

    let mut reversed: Vec<LoweredFile> = lowered.tree.files().to_vec();
    reversed.reverse();
    let rebuilt = LoweredTree::new(reversed).unwrap();

    assert_eq!(lowered.tree.digest(), rebuilt.digest());
    assert_eq!(lowered.tree, rebuilt);
}

/// Discovery walks the filesystem, whose order is neither sorted nor stable across platforms.
/// A project-scoped digest that inherited that order would make a cache entry built on one
/// machine miss on another.
#[test]
fn project_digest_is_independent_of_discovery_order() {
    let ordered = Fixture::sources();
    let mut shuffled = ordered.clone();
    shuffled.reverse();
    FrontendKey::canonical_order(&mut shuffled);

    assert_eq!(
        FrontendKey::project(&ordered),
        FrontendKey::project(&shuffled)
    );
}

#[test]
fn tree_manifest_round_trips() {
    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let lowered = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &files)).unwrap();

    let encoded = lowered.tree.encode();
    assert_eq!(LoweredTree::decode(&encoded).unwrap(), lowered.tree);
}

#[test]
fn a_damaged_manifest_is_rejected_rather_than_misread() {
    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let lowered = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &files)).unwrap();

    let encoded = lowered.tree.encode();
    for cut in 0..encoded.len() {
        assert!(LoweredTree::decode(&encoded[..cut]).is_err());
    }
    let mut trailing = encoded;
    trailing.push(0);
    assert!(LoweredTree::decode(&trailing).is_err());
}

/// A warm rebuild must restore the lowering from its manifest without running the frontend.
/// Asserted with a frontend that fails if invoked twice, so a regression to "always re-run"
/// is loud rather than merely slow.
#[test]
fn second_lowering_restores_from_cache_without_running_the_frontend() {
    use core::cell::Cell;

    struct RunsOnce {
        inner: VanillaFrontend,
        runs: Cell<u32>,
    }

    impl crate::frontend::Frontend for RunsOnce {
        fn caps(&self) -> crate::frontend::FrontendCaps {
            self.inner.caps()
        }
        fn config_digest(&self) -> jals_storage::ContentDigest {
            self.inner.config_digest()
        }
        fn run<'a>(&'a self, ir: crate::ir::Ir<'a>) -> crate::frontend::FrontendFuture<'a> {
            assert_eq!(self.runs.get(), 0, "frontend ran against a warm cache");
            self.runs.set(self.runs.get() + 1);
            self.inner.run(ir)
        }
        fn describe(&self, ir: &crate::ir::Ir<'_>) -> alloc::string::String {
            self.inner.describe(ir)
        }
    }

    let files = Fixture::sources();
    let mut cache = ArtifactCache::new(MemoryCache::default());
    let frontend = RunsOnce {
        inner: VanillaFrontend,
        runs: Cell::new(0),
    };

    let cold = block_on_inline(Driver::lower(&frontend, &mut cache, &files)).unwrap();
    assert!(!cold.cached);

    let warm = block_on_inline(Driver::lower(&frontend, &mut cache, &files)).unwrap();
    assert!(warm.cached);
    assert_eq!(cold.tree, warm.tree);
}

/// Editing one file must change that file's key and leave its sibling's untouched — the
/// per-file invalidation a frontend earns by declaring `IrLevel::Bytes`.
#[test]
fn editing_one_file_leaves_the_other_key_unchanged() {
    let before = Fixture::sources();
    let mut after = before.clone();
    after[0] = Fixture::file("src/main/java/Main.java", b"class Main { int x; }\n");

    let mut cache = ArtifactCache::new(MemoryCache::default());
    let first = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &before)).unwrap();
    let second = block_on_inline(Driver::lower(&VanillaFrontend, &mut cache, &after)).unwrap();

    let changed = &first.tree.files()[0];
    let changed_after = &second.tree.files()[0];
    assert_eq!(changed.path, changed_after.path);
    assert_ne!(changed.key, changed_after.key);

    let untouched = &first.tree.files()[1];
    let untouched_after = &second.tree.files()[1];
    assert_eq!(untouched.path, untouched_after.path);
    assert_eq!(
        untouched.key, untouched_after.key,
        "a Bytes-level frontend must not be invalidated by a sibling edit"
    );
}

// Frozen provenance digests for `sources()` under the vanilla frontend. Deliberately literals
// and not recomputed: recomputing them here would make the test tautological, which is exactly
// the failure mode it exists to prevent.
const FROZEN_MAIN_PROVENANCE: &str =
    "29b0dd5f77ef9e58f4574247179eade0c6d89d19a376ac61bc4ab126fa842ee8";
const FROZEN_UTIL_PROVENANCE: &str =
    "70b703a32c04a480bde53777192b0cf9d11048625ff104ef450f6b6beac094e4";
