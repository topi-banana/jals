//! Pure resolution of the build artifacts that `jals clean` removes.
//!
//! [`clean_paths`] turns a [`Manifest`] plus a project root into the set of paths whose removal
//! constitutes a clean — the Java analogue of `cargo clean` deleting `target/`. Like the rest of
//! this crate it is pure: it computes paths but never touches the filesystem. `jals-cli` owns the
//! removal, which keeps this logic deterministic, unit-testable, and `wasm32`-compatible.

use std::path::{Path, PathBuf};

use crate::manifest::Manifest;

/// Resolve the build-output paths that `jals clean` should remove for `manifest`, each resolved
/// against `project_root`.
///
/// This is the compiler output directory (`classes-dir`) — exactly what `javac -d` writes during a
/// [`build`](crate::build_invocation), and nothing the user authored. Returning a `Vec` leaves room
/// for future artifacts (a packaged jar, a dependency cache) without changing the signature. The
/// result may include paths that do not exist; the caller skips those rather than treating a
/// never-built project as an error.
pub fn clean_paths(manifest: &Manifest, project_root: &Path) -> Vec<PathBuf> {
    vec![project_root.join(&manifest.build.classes_dir)]
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/proj";

    #[test]
    fn removes_the_classes_dir() {
        let m = Manifest::default();
        let paths = clean_paths(&m, Path::new(ROOT));
        assert_eq!(paths, vec![PathBuf::from("/proj/target/classes")]);
    }

    #[test]
    fn honors_a_custom_classes_dir() {
        let mut m = Manifest::default();
        m.build.classes_dir = "out".to_string();
        let paths = clean_paths(&m, Path::new(ROOT));
        assert_eq!(paths, vec![PathBuf::from("/proj/out")]);
    }
}
