//! Pure resolution of the build artifacts that `jals clean` removes.
//!
//! [`CleanTargets::keys`] turns a [`Manifest`] into the set of root-relative directory keys whose
//! removal constitutes a clean — the Java analogue of `cargo clean` deleting `target/`. Like the
//! rest of this crate it is pure: it computes keys but never touches the filesystem. `jals-cli`
//! resolves them against the project root and owns the removal, which keeps this logic
//! deterministic, unit-testable, and `wasm32`-compatible.

use alloc::vec;
use alloc::vec::Vec;
use jals_config::Manifest;
use jals_storage::DirKey;

/// Namespace for resolving the build artifacts that `jals clean` removes.
pub struct CleanTargets;

impl CleanTargets {
    /// Resolve the build-output directories that `jals clean` should remove for `manifest`, as
    /// root-relative keys the caller resolves against the project root.
    ///
    /// This is the compiler output directory (`classes-dir`), the dedicated `target/jals/build`
    /// script-artifact root, the two roots a `[[test-target]]` run writes under, and — when
    /// `[build] remap` declares one — the directory holding the jar that step writes. Returning a
    /// `Vec` leaves room for future artifacts (a packaged jar, a dependency cache) without changing
    /// the signature.
    ///
    /// A target contributes its own `classes-dir` **and** the managed root that dir defaults into,
    /// which are two different claims. The first is a `javac -d` destination and is removed wherever
    /// the author put it, exactly as `classes-dir` and `[test] classes-dir` are. The second is what
    /// reaps a target that was *renamed or deleted*: its classes stay under the old name, and no
    /// manifest names them any more — the same stale-output argument that makes `target/jals/build`
    /// unconditional, which is why these roots are unconditional too rather than gated on a
    /// declaration being present.
    ///
    /// A declared remap contributes `target/jals/remap` — the root its jar defaults into — because
    /// nothing else claims it: the jar sits outside both roots above by construction
    /// ([`Manifest::validate`] rejects one inside either), so a clean that skipped it would leave
    /// the previous build's distributable behind. An author who redirected the jar elsewhere keeps
    /// that location: removing a directory they chose would take whatever else they keep in it.
    /// The result may include paths that do not exist; the caller skips those rather than treating a
    /// never-built project as an error.
    ///
    /// A root `classes-dir` is rejected rather than returned. `DirKey::parse("")` resolves to the
    /// project root, and the caller removes each key recursively, so returning it would delete the
    /// whole project — including files `jals` never generated. [`Manifest::validate`] rejects the
    /// same value up front; this check keeps the destructive half safe on its own.
    pub fn keys(manifest: &Manifest) -> Result<Vec<DirKey>, jals_storage::PathError> {
        let classes_dir = DirKey::parse(&manifest.build.classes_dir)?;
        if classes_dir.path().is_root() {
            return Err(jals_storage::PathError::DirectoryIsRoot);
        }
        let mut keys = vec![classes_dir];
        // The test run's output, on the same terms: it is jals-owned build output, it is removed
        // recursively, and so the root is refused here as well rather than trusted from the
        // manifest.
        let test_classes_dir = DirKey::parse(&manifest.test.classes_dir)?;
        if test_classes_dir.path().is_root() {
            return Err(jals_storage::PathError::DirectoryIsRoot);
        }
        if !keys.contains(&test_classes_dir) {
            keys.push(test_classes_dir);
        }
        let build_root = DirKey::parse("target/jals/build")?;
        if !keys.contains(&build_root) {
            keys.push(build_root);
        }
        // The two roots a target run writes under, then whatever a target redirected its classes
        // to. The roots come first so that the usual case — a target keeping the default — adds
        // nothing: its directory is already inside one of them.
        Self::add(
            &mut keys,
            DirKey::parse(jals_config::testing::MANAGED_TARGET_CLASSES_ROOT)?,
        );
        Self::add(
            &mut keys,
            DirKey::parse(jals_config::testing::MANAGED_TEST_ROOT)?,
        );
        for target in &manifest.test_target {
            let target_classes = DirKey::parse(&target.classes_dir())?;
            if target_classes.path().is_root() {
                return Err(jals_storage::PathError::DirectoryIsRoot);
            }
            Self::add(&mut keys, target_classes);
        }
        if manifest.build.remap.is_some() {
            // Declared, not *active*: a `[build] remap` whose mapping set no selection activates
            // still packages its jar into this root, so the question here is whether the step
            // exists and never which mapping it resolved to.
            //
            // The managed remap root, and only it — never the directory an author redirected the
            // jar into. `jals clean` removes a key recursively, so taking the parent of an
            // arbitrary `jar` path would delete whatever else the author keeps beside it. A
            // redirected jar is theirs to place and theirs to remove; what jals owns is the root it
            // chose itself, which holds nothing else by construction.
            let remap_root = DirKey::parse(jals_config::MANAGED_REMAP_ROOT)?;
            if !keys.contains(&remap_root) {
                keys.push(remap_root);
            }
        }
        Ok(keys)
    }

    /// Record `key` unless a key already held would remove it anyway.
    ///
    /// The caller removes each key recursively, so a directory *inside* one already listed is not a
    /// second target — it is the same removal named twice. Containment rather than equality because
    /// a target's `classes-dir` usually sits under the managed root that follows it, and a clean set
    /// that listed both would describe one deletion as two.
    fn add(keys: &mut Vec<DirKey>, key: DirKey) {
        if !keys.iter().any(|held| key.path().starts_with(held.path())) {
            keys.push(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use jals_storage::FileKey;

    use super::*;

    #[test]
    fn removes_the_classes_dir_and_stale_build_script_outputs() {
        let m = Manifest::default();
        let paths = CleanTargets::keys(&m).unwrap();
        assert_eq!(
            paths,
            vec![
                DirKey::parse("target/classes").unwrap(),
                DirKey::parse("target/test-classes").unwrap(),
                DirKey::parse("target/jals/build").unwrap(),
                DirKey::parse("target/jals/test-target").unwrap(),
                DirKey::parse("target/jals/test").unwrap(),
            ]
        );
    }

    #[test]
    fn honors_a_custom_classes_dir() {
        let mut m = Manifest::default();
        m.build.classes_dir = "out".into();
        let paths = CleanTargets::keys(&m).unwrap();
        assert_eq!(
            paths,
            vec![
                DirKey::parse("out").unwrap(),
                DirKey::parse("target/test-classes").unwrap(),
                DirKey::parse("target/jals/build").unwrap(),
                DirKey::parse("target/jals/test-target").unwrap(),
                DirKey::parse("target/jals/test").unwrap(),
            ]
        );
    }

    /// `DirKey::parse("")` resolves to the project root, and the caller removes every returned key
    /// recursively. Returning it would make `jals clean` delete the whole project, including
    /// untracked user files, so a root `classes-dir` must be rejected rather than cleaned.
    #[test]
    fn rejects_a_root_classes_dir() {
        let mut m = Manifest::default();
        m.build.classes_dir = String::new();
        assert_eq!(
            CleanTargets::keys(&m),
            Err(jals_storage::PathError::DirectoryIsRoot)
        );

        // `.` never reaches the root check: `Name` rejects it outright. Pin that too, so neither
        // spelling of "the project root" can become a clean target.
        m.build.classes_dir = ".".into();
        assert!(CleanTargets::keys(&m).is_err());
    }

    /// The test output directory is removed recursively too, so the root is refused here for the
    /// same reason `classes-dir`'s is — and independently of it, because `Manifest::validate` is
    /// not what makes the destructive half safe.
    #[test]
    fn rejects_a_root_test_classes_dir() {
        let mut m = Manifest::default();
        m.test.classes_dir = String::new();
        assert_eq!(
            CleanTargets::keys(&m),
            Err(jals_storage::PathError::DirectoryIsRoot)
        );
    }

    #[test]
    fn does_not_duplicate_the_build_script_root() {
        let mut m = Manifest::default();
        m.build.classes_dir = "target/jals/build".into();
        assert_eq!(
            CleanTargets::keys(&m).unwrap(),
            vec![
                DirKey::parse("target/jals/build").unwrap(),
                DirKey::parse("target/test-classes").unwrap(),
                DirKey::parse("target/jals/test-target").unwrap(),
                DirKey::parse("target/jals/test").unwrap(),
            ]
        );
    }

    #[test]
    fn validated_script_path_is_outside_every_clean_target() {
        let manifest: Manifest =
            "[build]\nscript = { type = \"rhai\", file = \"scripts/build.rhai\" }\n"
                .parse()
                .unwrap();
        let script = FileKey::parse("scripts/build.rhai").unwrap();

        assert!(
            CleanTargets::keys(&manifest)
                .unwrap()
                .iter()
                .all(|target| !script.path().starts_with(target.path()))
        );
    }

    /// A target keeping the default `classes-dir` adds nothing of its own: the directory is inside
    /// the managed root, which is already listed, and the caller removes a key recursively — so
    /// naming both would describe one deletion as two.
    #[test]
    fn a_target_on_the_default_root_adds_no_key_of_its_own() {
        let manifest: Manifest = "[[test-target]]\nname = \"client-e2e\"\nmain-class = \"E\"\n"
            .parse()
            .unwrap();
        assert_eq!(
            manifest.test_target[0].classes_dir(),
            "target/jals/test-target/client-e2e"
        );
        assert_eq!(
            CleanTargets::keys(&manifest).unwrap(),
            CleanTargets::keys(&Manifest::default()).unwrap()
        );
    }

    /// The managed roots are unconditional, and that is what reaps a target after it is renamed or
    /// deleted: nothing in the manifest names its old output any more. Same argument as the build
    /// root's stale-output sweep.
    #[test]
    fn the_managed_target_roots_are_removed_without_a_target_declared() {
        let keys = CleanTargets::keys(&Manifest::default()).unwrap();
        assert!(
            keys.contains(
                &DirKey::parse(jals_config::testing::MANAGED_TARGET_CLASSES_ROOT).unwrap()
            )
        );
        assert!(keys.contains(&DirKey::parse(jals_config::testing::MANAGED_TEST_ROOT).unwrap()));
    }

    /// A redirected target `classes-dir` is a `javac -d` destination like every other one here, so
    /// it is removed where the author put it — unlike a redirected remap *jar*, whose directory is
    /// left alone because it holds a file rather than being an output tree.
    #[test]
    fn a_redirected_target_classes_dir_is_removed_where_it_was_put() {
        let manifest: Manifest =
            "[[test-target]]\nname = \"e2e\"\nmain-class = \"E\"\nclasses-dir = \"out/e2e\"\n"
                .parse()
                .unwrap();
        assert!(
            CleanTargets::keys(&manifest)
                .unwrap()
                .contains(&DirKey::parse("out/e2e").unwrap())
        );
    }

    /// Every key is removed recursively, so a target naming the project root would delete the whole
    /// checkout. Refused here rather than trusted from `Manifest::validate`, for the same reason
    /// `classes-dir`'s is.
    #[test]
    fn rejects_a_root_target_classes_dir() {
        let mut m = Manifest::default();
        m.test_target.push(jals_config::testing::TestTarget {
            name: "e2e".to_owned(),
            classes_dir: ".".to_owned(),
            ..Default::default()
        });
        assert!(CleanTargets::keys(&m).is_err());
    }

    /// The archive `--update-golden` writes lands under a root the clean set holds, so a stale
    /// golden never survives a clean and never becomes the reference a later run is judged against.
    #[test]
    fn the_golden_update_archive_is_inside_a_cleaned_root() {
        let archive =
            FileKey::parse("target/jals/test/golden-update/client-e2e-1.21.11.zip").unwrap();
        assert!(
            CleanTargets::keys(&Manifest::default())
                .unwrap()
                .iter()
                .any(|target| archive.path().starts_with(target.path()))
        );
    }

    #[test]
    fn a_declared_remap_contributes_its_managed_root() {
        let mut m = Manifest::default();
        assert!(
            !CleanTargets::keys(&m)
                .unwrap()
                .contains(&DirKey::parse(jals_config::MANAGED_REMAP_ROOT).unwrap())
        );

        m.build.remap = Some(jals_config::BuildRemap {
            with: "mojmap".to_owned(),
            jar: None,
        });
        assert!(
            CleanTargets::keys(&m)
                .unwrap()
                .contains(&DirKey::parse(jals_config::MANAGED_REMAP_ROOT).unwrap())
        );
    }

    #[test]
    fn a_redirected_remap_jar_leaves_its_directory_alone() {
        // `jals clean` removes a key recursively. Taking the parent of a path the author chose
        // would delete whatever else they keep beside the jar, which is not jals' to remove.
        let mut m = Manifest::default();
        m.build.remap = Some(jals_config::BuildRemap {
            with: "mojmap".to_owned(),
            jar: Some("dist/mod.jar".to_owned()),
        });
        let keys = CleanTargets::keys(&m).unwrap();
        assert!(!keys.contains(&DirKey::parse("dist").unwrap()), "{keys:?}");
        assert!(keys.contains(&DirKey::parse(jals_config::MANAGED_REMAP_ROOT).unwrap()));
    }
}
