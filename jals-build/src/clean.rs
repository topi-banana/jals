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
    /// This is the compiler output directory (`classes-dir`) and the dedicated `target/jals/build`
    /// script-artifact root. Returning a `Vec` leaves room for future artifacts (a packaged jar, a
    /// dependency cache) without changing the signature.
    /// The result may include paths that do not exist; the caller skips those rather than treating a
    /// never-built project as an error.
    pub fn keys(manifest: &Manifest) -> Result<Vec<DirKey>, jals_storage::PathError> {
        let mut keys = vec![DirKey::parse(&manifest.build.classes_dir)?];
        let build_root = DirKey::parse("target/jals/build")?;
        if !keys.contains(&build_root) {
            keys.push(build_root);
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
}
