//! Native external-content adapter.
//!
//! Filesystem identity and persistence are intentionally owned by `jals-storage`; this module only
//! supplies the host HTTP capability.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_config::{Dependency, GitDependency, GitRef, Manifest, PathDependency};
use jals_storage::{
    CacheKey, CacheNamespace, ContentDigest, DirKey, EntryRef, FileKey, MemoryCache, Name,
    NativeSource, NativeStorage, ProjectStorage, ProjectView, RelativePath,
};

use crate::{
    ClasspathEntry, DependencyLocation, DependencyResolver, ExternalLocator, Fetcher,
    LibrarySource, ProjectInputOptions, ProjectInputPlan, ProjectInputs, Warning, WarningOrigin,
};

/// A fetcher backed by `reqwest`'s blocking client.
pub struct ReqwestFetcher {
    client: reqwest::blocking::Client,
    project_root: Option<PathBuf>,
}

impl ReqwestFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            project_root: None,
        }
    }

    /// Build the host fetch adapter for one project. Relative and `file://` locators are read by
    /// this adapter; HTTP remains the only network capability.
    pub fn for_project(project_root: PathBuf) -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            project_root: Some(project_root),
        }
    }
}

impl Default for ReqwestFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher for ReqwestFetcher {
    async fn fetch(&self, locator: &str) -> Result<Vec<u8>, String> {
        if let Some(path) = locator.strip_prefix("file://") {
            return fs::read(path).map_err(|error| format!("reading {path}: {error}"));
        }
        if !ExternalLocator::is_url(locator) {
            let path = self
                .project_root
                .as_deref()
                .unwrap_or_else(|| Path::new("."))
                .join(locator);
            return fs::read(&path).map_err(|error| format!("reading {}: {error}", path.display()));
        }
        let response = self
            .client
            .get(locator)
            .send()
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        response
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|error| format!("reading response: {error}"))
    }
}

/// Native lowering of a manifest into the portable classpath plan. Host paths stop here.
#[derive(Debug)]
pub struct NativeProjectPlan {
    pub plan: ProjectInputPlan,
    pub source_roots: Vec<DirKey>,
    pub warnings: Vec<Warning>,
    git_dependencies: Vec<(Name, GitDependency)>,
    /// `path` dependencies outside the project root, resolved against the host filesystem by
    /// [`materialize_path_sources`](Self::materialize_path_sources).
    path_dependencies: Vec<(Name, PathDependency)>,
}

impl NativeProjectPlan {
    /// Lower `manifest` and execute the whole native input assembly against one aggregate:
    /// materialize Git sources, fetch and resolve dependencies over blocking HTTP, and merge the
    /// lowering warnings into the result's. Blocking — must not run inside an async runtime (the
    /// LSP calls it from a dedicated thread). Returns the resolved inputs plus the manifest's
    /// source roots.
    pub fn assemble_blocking(
        manifest: &Manifest,
        project_root: &Path,
        storage: &mut NativeStorage,
        options: ProjectInputOptions,
    ) -> (ProjectInputs, Vec<DirKey>) {
        let mut native = Self::from_manifest(manifest, &storage.view());
        native.materialize_git_sources(project_root, storage, options);
        native.materialize_path_sources(project_root, storage, options);
        let fetcher = ReqwestFetcher::for_project(project_root.to_path_buf());
        let mut inputs = futures::executor::block_on(ProjectInputs::assemble(
            &fetcher,
            storage,
            &native.plan,
            options,
        ));
        native.warnings.append(&mut inputs.warnings);
        inputs.warnings = native.warnings;
        (inputs, native.source_roots)
    }

    pub fn from_manifest(manifest: &Manifest, view: &ProjectView) -> Self {
        let mut result = Self {
            plan: ProjectInputPlan {
                feature_set: manifest.feature_set(),
                ..ProjectInputPlan::default()
            },
            source_roots: Vec::new(),
            warnings: Vec::new(),
            git_dependencies: Vec::new(),
            path_dependencies: Vec::new(),
        };

        for source in &manifest.build.source_dirs {
            match Self::project_relative(source) {
                Some(path) => result.source_roots.push(DirKey::new(path)),
                None => {
                    result.warn_path(
                        source,
                        "invalid source directory: outside the project or not a portable path"
                            .to_owned(),
                    );
                }
            }
        }
        for classpath in &manifest.build.classpath {
            let Some(path) = Self::project_relative(classpath) else {
                result.warn_path(
                    classpath,
                    "classpath entry is missing or invalid".to_owned(),
                );
                continue;
            };
            if let Ok(file) = FileKey::new(path.clone())
                && matches!(view.tree().lookup_file(&file), Some(EntryRef::File(_)))
            {
                result
                    .plan
                    .classpath
                    .push(ClasspathEntry::ProjectFile(file));
                continue;
            }
            let directory = DirKey::new(path);
            if matches!(
                view.tree().lookup_dir(&directory),
                Some(EntryRef::Directory(_))
            ) {
                result
                    .plan
                    .classpath
                    .push(ClasspathEntry::ProjectDirectory(directory));
                continue;
            }
            result.warn_path(
                classpath,
                "classpath entry is missing or invalid".to_owned(),
            );
        }

        result
            .plan
            .add_jar_dependencies(manifest, Self::classify, &mut result.warnings);
        for (raw_name, dependency) in &manifest.dependencies {
            if matches!(dependency, Dependency::Jar(_)) {
                continue;
            }
            let name = match Name::new(raw_name) {
                Ok(name) => name,
                Err(error) => {
                    result.warn_path(
                        raw_name,
                        format!("dependency name is not a portable name: {error:?}"),
                    );
                    continue;
                }
            };
            match dependency {
                // Lowered by `add_jar_dependencies` above.
                Dependency::Jar(_) => {}
                Dependency::Path(path) => match Self::project_path_root(path, view) {
                    Ok(Some(key)) => result.plan.source_dependency_roots.push(key),
                    // Outside the project root: scanned from the host filesystem by
                    // `materialize_path_sources`.
                    Ok(None) => result.path_dependencies.push((name, path.clone())),
                    Err(message) => result.warn_path(&path.path, message),
                },
                Dependency::Git(git) => result.git_dependencies.push((name, git.clone())),
            }
        }
        result.source_roots.sort();
        result.source_roots.dedup();
        result
    }

    /// Clone native Git dependencies, scan their selected source roots through the same safe native
    /// source adapter, and publish every Java file as a verified `GitCheckout` artifact. Analysis
    /// deliberately skips source dependencies; compile/editor plans retain only typed cache keys.
    pub fn materialize_git_sources(
        &mut self,
        project_root: &Path,
        storage: &mut NativeStorage,
        options: ProjectInputOptions,
    ) {
        if matches!(options, ProjectInputOptions::Analysis) {
            return;
        }
        for (name, git) in self.git_dependencies.clone() {
            match Self::clone_and_publish_git(project_root, storage, &name, &git) {
                Ok(sources) => self.plan.source_dependency_artifacts.extend(sources),
                Err(message) => self.warnings.push(Warning::new(
                    WarningOrigin::External(ExternalLocator::new(git.git)),
                    message,
                )),
            }
        }
    }

    /// Scan native `path` dependencies that live outside the project root through the same safe
    /// native source adapter, and publish every Java file as a verified `PathSource` artifact.
    /// In-project `path` dependencies stay portable source roots (see
    /// [`from_manifest`](Self::from_manifest)).
    pub fn materialize_path_sources(
        &mut self,
        project_root: &Path,
        storage: &mut NativeStorage,
        options: ProjectInputOptions,
    ) {
        if matches!(options, ProjectInputOptions::Analysis) {
            return;
        }
        for (name, dependency) in self.path_dependencies.clone() {
            match Self::scan_and_publish_path(project_root, storage, &name, &dependency) {
                Ok(sources) => self.plan.source_dependency_artifacts.extend(sources),
                Err(message) => self.warnings.push(Warning::new(
                    WarningOrigin::External(ExternalLocator::new(dependency.path)),
                    message,
                )),
            }
        }
    }

    fn clone_and_publish_git(
        project_root: &Path,
        storage: &mut NativeStorage,
        name: &Name,
        git: &GitDependency,
    ) -> Result<Vec<LibrarySource>, String> {
        let reference = git
            .git_ref(name.as_str())
            .map_err(|error| format!("{error:?}"))?;
        // A tag or rev pins immutable content: recover the previous checkout's file list from
        // the cache and skip the clone while every artifact is still present and verified.
        if matches!(reference, GitRef::Tag(_) | GitRef::Rev(_))
            && let Some(sources) = Self::cached_git_sources(storage, name, git, &reference)
        {
            return Ok(sources);
        }
        let temporary =
            tempfile::tempdir().map_err(|error| format!("creating checkout: {error}"))?;
        let checkout = temporary.path().join("checkout");
        let output = Command::new("git")
            .current_dir(project_root)
            .arg("clone")
            .arg("--quiet")
            .arg(&git.git)
            .arg(&checkout)
            .output()
            .map_err(|error| format!("failed to run git (is it installed?): {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "git clone failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        if let Some(target) = reference.checkout_arg() {
            let output = Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .arg("checkout")
                .arg("--quiet")
                .arg(target)
                .output()
                .map_err(|error| format!("failed to run git checkout: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "git checkout failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
        }
        let source_root = Self::host_source_root(&checkout, git.dir.as_deref())?;
        let sources = Self::publish_source_tree(
            storage,
            name,
            CacheNamespace::GitCheckout,
            &Self::git_identity(git, &reference),
            &source_root,
        )?;
        if matches!(reference, GitRef::Tag(_) | GitRef::Rev(_)) {
            Self::record_git_manifest(storage, git, &reference, &sources);
        }
        Ok(sources)
    }

    /// Rebuild a pinned checkout's published sources from its recorded file list, without
    /// cloning. `None` (a missing index, manifest, or file artifact) falls back to a clone.
    fn cached_git_sources(
        storage: &NativeStorage,
        name: &Name,
        git: &GitDependency,
        reference: &GitRef,
    ) -> Option<Vec<LibrarySource>> {
        let identity = Self::git_identity(git, reference);
        let provenance =
            DependencyResolver::provenance_digest(b"git-manifest\0", identity.as_bytes());
        let key = storage
            .artifacts()
            .indexed_key(CacheNamespace::GitCheckout, provenance)
            .ok()??;
        let manifest = String::from_utf8(storage.artifacts().lookup(&key).ok()??).ok()?;
        let mut sources = Vec::new();
        let prefix = RelativePath::new([name.clone()]);
        for line in manifest.lines() {
            let (hex, in_checkout) = line.split_once(' ')?;
            let content = ContentDigest::from_hex(hex)?;
            let file = FileKey::parse(in_checkout).ok()?;
            let key = CacheKey::new(
                CacheNamespace::GitCheckout,
                ContentDigest::of(format!("{identity}\0{file}").as_bytes()),
                content,
            );
            if !matches!(storage.artifacts().lookup(&key), Ok(Some(_))) {
                return None;
            }
            sources.push(LibrarySource {
                path: prefix.concat(file.path()),
                key,
            });
        }
        Some(sources)
    }

    /// Record a pinned checkout's file list — `<content-hex> <in-checkout path>` per line — so
    /// the next run rebuilds the artifact keys without cloning. Best-effort advisory metadata:
    /// the reader re-verifies every artifact, so a failed write only costs a re-clone.
    fn record_git_manifest(
        storage: &mut NativeStorage,
        git: &GitDependency,
        reference: &GitRef,
        sources: &[LibrarySource],
    ) {
        let identity = Self::git_identity(git, reference);
        let mut manifest = String::new();
        for source in sources {
            // Strip the dependency-name prefix: the list must be name-independent, like the
            // per-file provenance.
            let in_checkout = RelativePath::new(source.path.segments().skip(1).cloned());
            manifest.push_str(&source.key.content().to_hex());
            manifest.push(' ');
            manifest.push_str(&in_checkout.to_string());
            manifest.push('\n');
        }
        let key = CacheKey::new(
            CacheNamespace::GitCheckout,
            DependencyResolver::provenance_digest(b"git-manifest\0", identity.as_bytes()),
            ContentDigest::of(manifest.as_bytes()),
        );
        if storage
            .artifacts_mut()
            .publish(&key, manifest.as_bytes())
            .is_ok()
        {
            let _ = storage.artifacts_mut().record_index(&key);
        }
    }

    /// The name-independent identity of a checkout: repository, pinned ref, and source dir.
    fn git_identity(git: &GitDependency, reference: &GitRef) -> String {
        format!(
            "git\0{}\0{}\0{}",
            git.git,
            Self::git_ref_label(reference),
            git.dir.as_deref().unwrap_or("")
        )
    }

    fn scan_and_publish_path(
        project_root: &Path,
        storage: &mut NativeStorage,
        name: &Name,
        dependency: &PathDependency,
    ) -> Result<Vec<LibrarySource>, String> {
        let base = fs::canonicalize(project_root.join(&dependency.path))
            .map_err(|error| format!("path dependency `{}`: {error}", dependency.path))?;
        let source_root = Self::host_source_root(&base, dependency.dir.as_deref())?;
        Self::publish_source_tree(
            storage,
            name,
            CacheNamespace::PathSource,
            &format!("path\0{}", source_root.display()),
            &source_root,
        )
    }

    /// The source root inside a host dependency tree: the configured `dir` under it, or the
    /// auto-detected conventional layout (see [`conventional_source_root`](Self::conventional_source_root)).
    fn host_source_root(root: &Path, configured: Option<&str>) -> Result<PathBuf, String> {
        if let Some(configured) = configured {
            let relative = RelativePath::parse(configured)
                .map_err(|error| format!("invalid source directory `{configured}`: {error:?}"))?;
            let selected = relative.to_host_path(root);
            return selected
                .is_dir()
                .then_some(selected)
                .ok_or_else(|| format!("source directory `{configured}` is missing"));
        }
        Ok(
            Self::conventional_source_root(|candidate| candidate.to_host_path(root).is_dir())
                .to_host_path(root),
        )
    }

    /// The single statement of the dependency layout convention, shared by the host-path and
    /// in-project arms: the first conventional candidate the probe confirms (`src/main/java` →
    /// `src`), or the tree root itself.
    fn conventional_source_root(exists: impl Fn(&RelativePath) -> bool) -> RelativePath {
        ["src/main/java", "src"]
            .iter()
            .map(|candidate| {
                RelativePath::parse(candidate).expect("conventional layout is portable")
            })
            .find(|candidate| exists(candidate))
            .unwrap_or(RelativePath::ROOT)
    }

    /// Scan `source_root` through the safe native source adapter and publish every Java file as
    /// a verified artifact under `namespace`, with a per-file provenance of
    /// `<identity>\0<in-tree path>`.
    fn publish_source_tree(
        storage: &mut NativeStorage,
        name: &Name,
        namespace: CacheNamespace,
        identity: &str,
        source_root: &Path,
    ) -> Result<Vec<LibrarySource>, String> {
        let source = NativeSource::new(source_root.to_path_buf())
            .map_err(|error| error.to_string())?
            .excluding(RelativePath::parse(".git").expect(".git is a portable segment"));
        let checkout = ProjectStorage::open(source, MemoryCache::default())
            .map_err(|error| error.to_string())?;
        let view = checkout.view();
        let mut sources = Vec::new();
        let prefix = RelativePath::new([name.clone()]);
        for file in view
            .tree()
            .files_under(&DirKey::ROOT)
            .filter(|file| file.key().has_extension("java"))
        {
            let path = prefix.concat(file.key().path());
            let provenance = format!("{identity}\0{}", file.key());
            let key = CacheKey::new(
                namespace,
                ContentDigest::of(provenance.as_bytes()),
                ContentDigest::of(file.bytes()),
            );
            storage
                .artifacts_mut()
                .publish(&key, file.bytes())
                .map_err(|error| format!("publishing dependency source `{path}`: {error:?}"))?;
            sources.push(LibrarySource { path, key });
        }
        Ok(sources)
    }

    fn git_ref_label(reference: &GitRef) -> String {
        match reference {
            GitRef::Default => "default".to_owned(),
            GitRef::Branch(value) => format!("branch:{value}"),
            GitRef::Tag(value) => format!("tag:{value}"),
            GitRef::Rev(value) => format!("rev:{value}"),
        }
    }

    /// How a jar locator's bytes are obtained: a URL-shaped locator is external; anything else
    /// that normalizes to a portable in-project key is read from the project revision, else left
    /// external for the fetcher's host path policy.
    fn classify(locator: &str) -> DependencyLocation {
        if !ExternalLocator::is_url(locator)
            && let Some(file) =
                Self::project_relative(locator).and_then(|path| FileKey::new(path).ok())
        {
            return DependencyLocation::Project(file);
        }
        DependencyLocation::External {
            locator: ExternalLocator::new(locator),
            expected: None,
        }
    }

    /// Lower one manifest host-path string to a portable in-project path. Host spellings such
    /// as `.`, `./src`, a trailing slash, or redundant separators normalize away; `None` when
    /// the path leaves the project root (absolute, a drive, `..`) or a component is not a
    /// portable name.
    fn project_relative(raw: &str) -> Option<RelativePath> {
        let mut segments = Vec::new();
        for component in Path::new(raw).components() {
            match component {
                Component::CurDir => {}
                Component::Normal(name) => segments.push(Name::new(name.to_str()?).ok()?),
                Component::RootDir | Component::Prefix(_) | Component::ParentDir => return None,
            }
        }
        Some(RelativePath::new(segments))
    }

    /// The in-project source root of a `path` dependency: the configured `dir` under it, or the
    /// auto-detected conventional layout (`src/main/java` → `src` → the directory itself).
    /// `Ok(None)` when the dependency lies outside the project root — the native
    /// materialization step scans those from the host filesystem instead.
    fn project_path_root(
        dependency: &PathDependency,
        view: &ProjectView,
    ) -> Result<Option<DirKey>, String> {
        let Some(base) = Self::project_relative(&dependency.path) else {
            return Ok(None);
        };
        let root = if let Some(dir) = &dependency.dir {
            let Some(sub) = Self::project_relative(dir) else {
                return Err(format!("invalid source directory `{dir}`"));
            };
            base.concat(&sub)
        } else {
            let conventional = Self::conventional_source_root(|candidate| {
                view.directory(&DirKey::new(base.concat(candidate))).is_ok()
            });
            base.concat(&conventional)
        };
        let key = DirKey::new(root);
        if view.directory(&key).is_ok() {
            Ok(Some(key))
        } else {
            Err("source dependency is missing".to_owned())
        }
    }

    fn warn_path(&mut self, path: &str, message: String) {
        self.warnings.push(Warning::new(
            WarningOrigin::External(ExternalLocator::new(path)),
            message,
        ));
    }
}
