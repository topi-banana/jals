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
    /// script-artifact root, and — when `[build] remap` declares one — the directory holding the
    /// jar that step writes. Returning a `Vec` leaves room for future artifacts (a packaged jar, a
    /// dependency cache) without changing the signature.
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
        let build_root = DirKey::parse("target/jals/build")?;
        if !keys.contains(&build_root) {
            keys.push(build_root);
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
                DirKey::parse("target/jals/build").unwrap(),
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
                DirKey::parse("target/jals/build").unwrap(),
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

    #[test]
    fn does_not_duplicate_the_build_script_root() {
        let mut m = Manifest::default();
        m.build.classes_dir = "target/jals/build".into();
        assert_eq!(
            CleanTargets::keys(&m).unwrap(),
            vec![DirKey::parse("target/jals/build").unwrap()]
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
