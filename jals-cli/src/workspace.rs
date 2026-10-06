//! Workspace discovery: the root manifest and the members its patterns select.
//!
//! A workspace is one `jals.lock` and one `target/` at the root, shared by every member. The
//! discovery here answers exactly two questions for the CLI: which directory is the workspace
//! root (so the lock has one home), and which member manifests exist (so a resolution can union
//! them). It does **not** yet answer "which member does this command build" — commands operate on
//! the member whose directory they were invoked in, and `default-members` is validated but not
//! yet a selection.
//!
//! Walking upward is what makes a nested `[workspace]` a *nested workspace* rather than an
//! extension: the nearest root wins, exactly as `jals.toml` discovery itself works for member
//! manifests.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use jals_build::ManifestExt as _;
use jals_config::{Manifest, ResourcePattern, Workspace};
use jals_storage::RelativePath;

/// How deep under the workspace root member discovery walks. A project tree deeper than this is
/// pathological, and an unbounded walk over a home directory is not a walk a build should start.
const MAX_MEMBER_DEPTH: usize = 24;

/// A discovered workspace root and its members.
#[derive(Debug)]
pub struct ProjectWorkspace {
    root: PathBuf,
    /// Member manifest paths, sorted and deduplicated.
    members: Vec<PathBuf>,
}

impl ProjectWorkspace {
    /// Discover the workspace governing the project at `start_dir`, walking upward.
    ///
    /// `start_dir` is the invoked project's manifest directory. The nearest ancestor (including
    /// `start_dir`) whose `jals.toml` carries `[workspace]` is the root. A malformed manifest on
    /// the way up is an error: silently skipping it is how a workspace with a typo in a pattern
    /// resolves as if it were not there.
    ///
    /// # Errors
    /// A manifest read/parse failure, a pattern that matches no project, `default-members` that
    /// names no discovered member, or `start_dir` sitting under a root that excludes it.
    pub fn discover(start_dir: &Path) -> Result<Option<Self>> {
        let mut current = Some(start_dir);
        while let Some(dir) = current {
            let manifest_path = dir.join("jals.toml");
            if manifest_path.is_file() {
                let text = fs::read_to_string(&manifest_path)
                    .with_context(|| format!("reading {}", manifest_path.display()))?;
                let manifest: Manifest = text
                    .parse()
                    .map_err(|error| anyhow!("{}: {error}", manifest_path.display()))?;
                if let Some(workspace) = &manifest.workspace {
                    return Self::expand(dir, &manifest, workspace).map(Some);
                }
            }
            current = dir.parent();
        }
        Ok(None)
    }

    /// The workspace root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every member's manifest path, sorted.
    pub fn members(&self) -> &[PathBuf] {
        &self.members
    }

    /// Load every member manifest.
    ///
    /// # Errors
    /// A member manifest that cannot be read or parsed. A workspace whose member list names a
    /// directory without a `jals.toml` never reaches here: discovery only selects directories
    /// that have one.
    pub async fn manifests(&self) -> Result<Vec<Manifest>> {
        let mut manifests = Vec::with_capacity(self.members.len());
        for path in &self.members {
            let manifest = Manifest::from_file(path)
                .await
                .with_context(|| format!("loading workspace member {}", path.display()))?;
            manifests.push(manifest);
        }
        Ok(manifests)
    }

    /// Expand the root's patterns against the filesystem under `root`.
    fn expand(root: &Path, root_manifest: &Manifest, workspace: &Workspace) -> Result<Self> {
        let mut patterns = Vec::with_capacity(workspace.members.len());
        for pattern in &workspace.members {
            patterns.push(Self::pattern(pattern)?);
        }
        let mut excludes = Vec::with_capacity(workspace.exclude.len());
        for pattern in &workspace.exclude {
            excludes.push(Self::pattern(pattern)?);
        }

        let mut directories = Vec::new();
        Self::collect(root, 0, &mut directories);
        directories.sort();

        let matches_any = |patterns: &[MemberPattern], path: &RelativePath| {
            patterns.iter().any(|pattern| pattern.matches(path))
        };
        let mut members: Vec<PathBuf> = Vec::new();
        // A root that is itself a package is always a member, whatever `members` lists — the
        // Cargo rule, and the reason a `[package] [workspace]` manifest does not have to write
        // `"."` to include itself.
        if root_manifest.package.name.is_some() {
            members.push(root.join("jals.toml"));
        }
        for directory in &directories {
            let Some(path) = Self::relative_path(root, directory) else {
                continue;
            };
            if matches_any(&patterns, &path) && !matches_any(&excludes, &path) {
                members.push(directory.join("jals.toml"));
            }
        }
        members.sort();
        members.dedup();
        if members.is_empty() {
            return Err(anyhow!(
                "workspace `members` matched no project under {}",
                root.display()
            ));
        }
        // `default-members` is validated here even though no command consumes the selection yet:
        // a name that matches nothing is a typo, and catching it now is what keeps the key from
        // becoming silently inert.
        for pattern in &workspace.default_members {
            let pattern = Self::pattern(pattern)?;
            let member_dirs: Vec<RelativePath> = members
                .iter()
                .filter_map(|manifest| manifest.parent())
                .filter_map(|dir| Self::relative_path(root, dir))
                .collect();
            if !member_dirs.iter().any(|path| pattern.matches(path)) {
                return Err(anyhow!(
                    "`[workspace] default-members` entry `{}` matches no member",
                    pattern.original
                ));
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            members,
        })
    }

    /// `dir` relative to `root` as a portable path, or `None` when it is outside `root`.
    ///
    /// The portability matters: `Path` renders with the host separator (a backslash on Windows),
    /// and a [`RelativePath`] is `/`-separated by contract. Building the path from components
    /// keeps member matching identical on every platform — which is what the CI matrix checks.
    fn relative_path(root: &Path, dir: &Path) -> Option<RelativePath> {
        let relative = dir.strip_prefix(root).ok()?;
        if relative.as_os_str().is_empty() {
            return Some(RelativePath::ROOT);
        }
        let mut segments = Vec::new();
        for component in relative.components() {
            match component {
                std::path::Component::Normal(name) => {
                    segments.push(name.to_string_lossy().into_owned());
                }
                _ => return None,
            }
        }
        RelativePath::resolve(&RelativePath::ROOT, &segments.join("/")).ok()
    }

    /// Collect directories containing a `jals.toml`, below `dir` (which is `root` at entry).
    ///
    /// Hidden directories, `target`, and the workspace cache are skipped: they cannot hold a
    /// member, and walking them is how discovery would descend into build output.
    fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > MAX_MEMBER_DEPTH {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "target" {
                continue;
            }
            if path.join("jals.toml").is_file() {
                out.push(path.clone());
            }
            Self::collect(&path, depth + 1, out);
        }
    }

    /// One member pattern: the resource glob, plus the `.` spelling for the root.
    fn pattern(raw: &str) -> Result<MemberPattern> {
        if raw == "." {
            return Ok(MemberPattern {
                root: true,
                glob: None,
                original: raw.to_owned(),
            });
        }
        let glob = ResourcePattern::parse(raw)
            .map_err(|error| anyhow!("workspace member pattern `{raw}`: {error}"))?;
        Ok(MemberPattern {
            root: false,
            glob: Some(glob),
            original: raw.to_owned(),
        })
    }
}

/// A parsed `[workspace]` pattern; `.` is the root, which the glob grammar rejects.
struct MemberPattern {
    root: bool,
    glob: Option<ResourcePattern>,
    original: String,
}

impl MemberPattern {
    /// Whether `path` (relative to the workspace root) is this pattern.
    fn matches(&self, path: &RelativePath) -> bool {
        if self.root {
            return path.segments().next().is_none();
        }
        self.glob.as_ref().is_some_and(|glob| glob.matches(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn workspace(root: &Path, members: &str) -> PathBuf {
        let manifest = format!("[workspace]\nmembers = {members}\n");
        write(&root.join("jals.toml"), &manifest);
        root.join("jals.toml")
    }

    fn member(root: &Path, name: &str) {
        write(
            &root.join(name).join("jals.toml"),
            &format!("[package]\nname = \"{}\"\n", name.replace('/', "-")),
        );
    }

    #[test]
    fn discovery_walks_up_to_the_nearest_root_and_expands_globs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        workspace(root, "[\"app\", \"libs/*\"]");
        member(root, "app");
        member(root, "libs/a");
        member(root, "libs/b");
        member(root, "unlisted");

        let discovered = ProjectWorkspace::discover(&root.join("libs/a")).unwrap();
        let workspace = discovered.expect("a workspace root exists");
        assert_eq!(workspace.root(), root);
        let members: Vec<String> = workspace
            .members()
            .iter()
            .map(|path| {
                ProjectWorkspace::relative_path(root, path.parent().unwrap())
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(members, vec!["app", "libs/a", "libs/b"]);
    }

    #[test]
    fn exclude_removes_matched_members_and_root_packages_join_implicitly() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("jals.toml"),
            "[package]\nname = \"root-pkg\"\n\n[workspace]\nmembers = [\"app\"]\nexclude = [\"app/legacy\"]\n",
        );
        member(root, "app");
        member(root, "app/legacy");

        let workspace = ProjectWorkspace::discover(root)
            .unwrap()
            .expect("a workspace root exists");
        let members: Vec<String> = workspace
            .members()
            .iter()
            .map(|path| {
                let relative =
                    ProjectWorkspace::relative_path(root, path.parent().unwrap()).unwrap();
                if relative.segments().next().is_none() {
                    ".".to_owned()
                } else {
                    relative.to_string()
                }
            })
            .collect();
        assert_eq!(members, vec!["app", "."]);
    }

    #[test]
    fn an_unmatched_pattern_and_an_unmatched_default_member_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        workspace(root, "[\"missing\"]");
        let error = ProjectWorkspace::discover(root).unwrap_err();
        assert!(error.to_string().contains("matched no project"), "{error}");

        workspace(root, "[\"app\"]");
        write(
            &root.join("jals.toml"),
            "[workspace]\nmembers = [\"app\"]\ndefault-members = [\"nowhere\"]\n",
        );
        member(root, "app");
        let error = ProjectWorkspace::discover(root).unwrap_err();
        assert!(error.to_string().contains("matches no member"), "{error}");
    }

    #[test]
    fn no_workspace_table_anywhere_is_not_a_workspace() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("jals.toml"),
            "[package]\nname = \"solo\"\n",
        );
        assert!(ProjectWorkspace::discover(dir.path()).unwrap().is_none());
    }
}
