//! Native external-content adapter.
//!
//! Filesystem identity and persistence are intentionally owned by `jals-storage`; this module only
//! supplies the host HTTP capability. Blocking host work (filesystem reads, git subprocesses)
//! runs through [`on_blocking_pool`], so the current-thread executor keeps serving tasks.

use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_config::{Dependency, GitDependency, GitRef, Manifest, PathDependency};
use jals_exec::tokio_rt::on_blocking_pool;
use jals_storage::{
    CacheKey, CacheNamespace, ContentDigest, DirKey, EntryRef, FileKey, MemoryCache, Name,
    NativeScope, NativeSource, NativeStorage, ProjectStorage, ProjectView, RelativePath,
};

use crate::{
    ClasspathEntry, DependencyLocation, DependencyResolver, ExternalLocator, Fetcher,
    LibrarySource, ProjectInputOptions, ProjectInputPlan, ProjectInputs, Warning, WarningOrigin,
};

/// A fetcher backed by `reqwest`'s async client.
pub struct ReqwestFetcher {
    client: reqwest::Client,
    project_root: Option<PathBuf>,
}

impl ReqwestFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            project_root: None,
        }
    }

    /// Build the host fetch adapter for one project. Relative and `file://` locators are read by
    /// this adapter; HTTP remains the only network capability.
    pub fn for_project(project_root: PathBuf) -> Self {
        Self {
            client: reqwest::Client::new(),
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
            let path = path.to_owned();
            return on_blocking_pool(move || {
                fs::read(&path).map_err(|error| format!("reading {path}: {error}"))
            })
            .await;
        }
        if !ExternalLocator::is_url(locator) {
            let path = self
                .project_root
                .as_deref()
                .unwrap_or_else(|| Path::new("."))
                .join(locator);
            return on_blocking_pool(move || {
                fs::read(&path).map_err(|error| format!("reading {}: {error}", path.display()))
            })
            .await;
        }
        let response = self
            .client
            .get(locator)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| format!("reading response: {error}"))
    }

    async fn fetch_bounded(&self, locator: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        if let Some(path) = locator.strip_prefix("file://") {
            return Self::read_file_bounded(PathBuf::from(path), max_bytes).await;
        }
        if !ExternalLocator::is_url(locator) {
            let path = self
                .project_root
                .as_deref()
                .unwrap_or_else(|| Path::new("."))
                .join(locator);
            return Self::read_file_bounded(path, max_bytes).await;
        }
        let mut response = self
            .client
            .get(locator)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(format!("response exceeds the limit of {max_bytes} bytes"));
        }
        let mut bytes = Vec::with_capacity(
            response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or_default()
                .min(max_bytes),
        );
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("reading response: {error}"))?
        {
            if bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|length| length > max_bytes)
            {
                return Err(format!("response exceeds the limit of {max_bytes} bytes"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

impl ReqwestFetcher {
    async fn read_file_bounded(path: PathBuf, max_bytes: usize) -> Result<Vec<u8>, String> {
        on_blocking_pool(move || {
            let file = fs::File::open(&path)
                .map_err(|error| format!("opening {}: {error}", path.display()))?;
            let limit = u64::try_from(max_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let mut bytes = Vec::new();
            file.take(limit)
                .read_to_end(&mut bytes)
                .map_err(|error| format!("reading {}: {error}", path.display()))?;
            if bytes.len() > max_bytes {
                return Err(format!(
                    "{} exceeds the limit of {max_bytes} bytes",
                    path.display()
                ));
            }
            Ok(bytes)
        })
        .await
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
    external_source_roots: Vec<PathBuf>,
    external_classpath: Vec<PathBuf>,
}

impl NativeProjectPlan {
    /// Lower `manifest` and execute the whole native input assembly against one aggregate:
    /// materialize Git sources, fetch and resolve dependencies over async HTTP, and merge the
    /// lowering warnings into the result's. Fan-out and blocking host work run on the storage's
    /// own execution context. Returns the resolved inputs plus the manifest's source roots.
    pub async fn assemble_native(
        manifest: &Manifest,
        project_root: &Path,
        storage: &mut NativeStorage,
        options: ProjectInputOptions,
    ) -> (ProjectInputs, Vec<DirKey>) {
        let mut native = Self::from_manifest(manifest, project_root, &storage.view());
        native.materialize_external_sources(storage, options).await;
        native
            .materialize_external_classpath(storage, options)
            .await;
        native
            .materialize_git_sources(project_root, storage, options)
            .await;
        native
            .materialize_path_sources(project_root, storage, options)
            .await;
        let fetcher = ReqwestFetcher::for_project(project_root.to_path_buf());
        let mut inputs = ProjectInputs::assemble(&fetcher, storage, &native.plan, options).await;
        native.warnings.append(&mut inputs.warnings);
        inputs.warnings = native.warnings;
        (inputs, native.source_roots)
    }

    pub fn from_manifest(manifest: &Manifest, project_root: &Path, view: &ProjectView) -> Self {
        let mut result = Self {
            plan: ProjectInputPlan {
                feature_set: manifest.feature_set(),
                ..ProjectInputPlan::default()
            },
            source_roots: Vec::new(),
            warnings: Vec::new(),
            git_dependencies: Vec::new(),
            path_dependencies: Vec::new(),
            external_source_roots: Vec::new(),
            external_classpath: Vec::new(),
        };

        for source in &manifest.build.source_dirs {
            match Self::project_relative(project_root, source) {
                Some(path) => result.source_roots.push(DirKey::new(path)),
                None => result
                    .external_source_roots
                    .push(Self::resolve_host_path(project_root, source)),
            }
        }
        for classpath in &manifest.build.classpath {
            let Some(path) = Self::project_relative(project_root, classpath) else {
                result
                    .external_classpath
                    .push(Self::resolve_host_path(project_root, classpath));
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

        result.plan.add_jar_dependencies(
            manifest,
            |locator| Self::classify(project_root, locator),
            &mut result.warnings,
        );
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
                Dependency::Path(path) => match Self::project_path_root(path, project_root, view) {
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

    /// Native snapshot scopes required to lower this manifest. Source and dependency directories
    /// retain only Java bytes, classpath directories retain only class bytes, and explicit files
    /// are included exactly. External paths are handled through the artifact adapter instead.
    pub fn snapshot_scopes(manifest: &Manifest, project_root: &Path) -> Vec<NativeScope> {
        let mut scopes = Vec::new();
        for source in &manifest.build.source_dirs {
            if let Some(path) = Self::project_relative(project_root, source) {
                scopes.push(NativeScope::extension(path, "java"));
            }
        }
        for classpath in &manifest.build.classpath {
            if let Some(path) = Self::project_relative(project_root, classpath) {
                let host = path.to_host_path(project_root);
                scopes.push(if host.is_dir() {
                    NativeScope::extension(path, "class")
                } else {
                    NativeScope::all(path)
                });
            }
        }
        for dependency in manifest.dependencies.values() {
            match dependency {
                Dependency::Jar(jar) => {
                    for locator in core::iter::once(&jar.jar).chain(jar.sources.iter()) {
                        if !ExternalLocator::is_url(locator)
                            && let Some(path) = Self::project_relative(project_root, locator)
                        {
                            scopes.push(NativeScope::all(path));
                        }
                    }
                }
                Dependency::Path(path) => {
                    let base = Self::resolve_host_path(project_root, &path.path);
                    if let Ok(source) = Self::host_source_root(&base, path.dir.as_deref())
                        && let Some(relative) = Self::relative_to_project(project_root, &source)
                    {
                        scopes.push(NativeScope::extension(relative, "java"));
                    }
                }
                Dependency::Git(_) => {}
            }
        }
        scopes
    }

    async fn materialize_external_sources(
        &mut self,
        storage: &mut NativeStorage,
        options: ProjectInputOptions,
    ) {
        if !matches!(options, ProjectInputOptions::Editor) {
            return;
        }
        for (index, source_root) in self.external_source_roots.clone().into_iter().enumerate() {
            let name = Name::new(format!("external-source-{index}"))
                .expect("generated external source name is portable");
            let outcome = Self::publish_source_tree(
                storage,
                &name,
                CacheNamespace::PathSource,
                &format!("build-source\0{}", source_root.display()),
                &source_root,
            )
            .await;
            self.absorb_sources(source_root.display().to_string(), outcome);
        }
    }

    async fn materialize_external_classpath(
        &mut self,
        storage: &mut NativeStorage,
        options: ProjectInputOptions,
    ) {
        if matches!(options, ProjectInputOptions::Compile) {
            return;
        }
        for path in self.external_classpath.clone() {
            if let Err(message) = Self::publish_classpath_path(storage, &path, &mut self.plan).await
            {
                self.warnings.push(Warning::new(
                    WarningOrigin::External(ExternalLocator::new(path.display().to_string())),
                    message,
                ));
            }
        }
    }

    async fn publish_classpath_path(
        storage: &mut NativeStorage,
        path: &Path,
        plan: &mut ProjectInputPlan,
    ) -> Result<(), String> {
        if path.is_file() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    format!("classpath file name is not portable: {}", path.display())
                })?;
            let logical = RelativePath::new([Name::new(name)
                .map_err(|error| format!("invalid classpath file name: {error:?}"))?]);
            let entry = Self::publish_classpath_file(storage, path, logical).await?;
            plan.classpath.push(entry);
            return Ok(());
        }
        if !path.is_dir() {
            return Err(format!("classpath entry is missing: {}", path.display()));
        }
        let source = NativeSource::new(path.to_path_buf())
            .map_err(|error| error.to_string())?
            .scoped([NativeScope::extension(RelativePath::ROOT, "class")]);
        let snapshot = ProjectStorage::open(source, MemoryCache::default(), storage.exec().clone())
            .await
            .map_err(|error| error.to_string())?;
        for file in snapshot.view().tree().files() {
            let entry = Self::publish_classpath_file(
                storage,
                &file.key().path().to_host_path(path),
                file.key().path().clone(),
            )
            .await?;
            plan.classpath.push(entry);
        }
        Ok(())
    }

    async fn publish_classpath_file(
        storage: &mut NativeStorage,
        host: &Path,
        logical: RelativePath,
    ) -> Result<ClasspathEntry, String> {
        let bytes = {
            let host = host.to_path_buf();
            on_blocking_pool(move || {
                fs::read(&host)
                    .map_err(|error| format!("reading classpath file {}: {error}", host.display()))
            })
            .await?
        };
        let provenance = ContentDigest::of(host.display().to_string().as_bytes());
        let key = CacheKey::new(
            CacheNamespace::ExternalClasspath,
            provenance,
            ContentDigest::of(&bytes),
        );
        storage
            .artifacts_mut()
            .publish(&key, &bytes)
            .await
            .map_err(|error| format!("publishing classpath file {}: {error:?}", host.display()))?;
        Ok(ClasspathEntry::ArtifactFile { path: logical, key })
    }

    /// Clone native Git dependencies, scan their selected source roots through the same safe native
    /// source adapter, and publish every Java file as a verified `GitCheckout` artifact. Analysis
    /// deliberately skips source dependencies; compile/editor plans retain only typed cache keys.
    pub async fn materialize_git_sources(
        &mut self,
        project_root: &Path,
        storage: &mut NativeStorage,
        options: ProjectInputOptions,
    ) {
        if matches!(options, ProjectInputOptions::Analysis) {
            return;
        }
        for (name, git) in self.git_dependencies.clone() {
            let outcome = Self::clone_and_publish_git(project_root, storage, &name, &git).await;
            self.absorb_sources(git.git, outcome);
        }
    }

    /// Scan native `path` dependencies that live outside the project root through the same safe
    /// native source adapter, and publish every Java file as a verified `PathSource` artifact.
    /// In-project `path` dependencies stay portable source roots (see
    /// [`from_manifest`](Self::from_manifest)).
    pub async fn materialize_path_sources(
        &mut self,
        project_root: &Path,
        storage: &mut NativeStorage,
        options: ProjectInputOptions,
    ) {
        if matches!(options, ProjectInputOptions::Analysis) {
            return;
        }
        for (name, dependency) in self.path_dependencies.clone() {
            let outcome =
                Self::scan_and_publish_path(project_root, storage, &name, &dependency).await;
            self.absorb_sources(dependency.path, outcome);
        }
    }

    /// Fold one materialization outcome into the plan: the published sources on success, an
    /// external-locator warning on failure.
    fn absorb_sources(&mut self, locator: String, outcome: Result<Vec<LibrarySource>, String>) {
        match outcome {
            Ok(sources) => self.plan.source_dependency_artifacts.extend(sources),
            Err(message) => self.warnings.push(Warning::new(
                WarningOrigin::External(ExternalLocator::new(locator)),
                message,
            )),
        }
    }

    async fn clone_and_publish_git(
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
        let pinned = matches!(reference, GitRef::Tag(_) | GitRef::Rev(_));
        if pinned
            && let Some(sources) = Self::cached_git_sources(storage, name, git, &reference).await
        {
            return Ok(sources);
        }
        let temporary =
            tempfile::tempdir().map_err(|error| format!("creating checkout: {error}"))?;
        let checkout = temporary.path().join("checkout");
        let output = {
            let project_root = project_root.to_path_buf();
            let url = git.git.clone();
            let checkout = checkout.clone();
            on_blocking_pool(move || {
                Command::new("git")
                    .current_dir(project_root)
                    .arg("clone")
                    .arg("--quiet")
                    .arg(url)
                    .arg(checkout)
                    .output()
            })
            .await
            .map_err(|error| format!("failed to run git (is it installed?): {error}"))?
        };
        if !output.status.success() {
            return Err(format!(
                "git clone failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        if let Some(target) = reference.checkout_arg() {
            let output = {
                let checkout = checkout.clone();
                let target = target.to_owned();
                on_blocking_pool(move || {
                    Command::new("git")
                        .arg("-C")
                        .arg(checkout)
                        .arg("checkout")
                        .arg("--quiet")
                        .arg(target)
                        .output()
                })
                .await
                .map_err(|error| format!("failed to run git checkout: {error}"))?
            };
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
        )
        .await?;
        if pinned {
            Self::record_git_manifest(storage, git, &reference, &sources).await;
        }
        Ok(sources)
    }

    /// Rebuild a pinned checkout's published sources from its recorded file list, without
    /// cloning. `None` (a missing index, manifest, or file artifact) falls back to a clone.
    async fn cached_git_sources(
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
            .await
            .ok()??;
        let manifest = String::from_utf8(storage.artifacts().lookup(&key).await.ok()??).ok()?;
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
            if !matches!(storage.artifacts().open_verified(&key).await, Ok(Some(_))) {
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
    async fn record_git_manifest(
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
            .await
            .is_ok()
        {
            let _ = storage.artifacts_mut().record_index(&key).await;
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

    async fn scan_and_publish_path(
        project_root: &Path,
        storage: &mut NativeStorage,
        name: &Name,
        dependency: &PathDependency,
    ) -> Result<Vec<LibrarySource>, String> {
        let base = fs::canonicalize(Self::resolve_host_path(project_root, &dependency.path))
            .map_err(|error| format!("path dependency `{}`: {error}", dependency.path))?;
        let source_root = Self::host_source_root(&base, dependency.dir.as_deref())?;
        Self::publish_source_tree(
            storage,
            name,
            CacheNamespace::PathSource,
            &format!("path\0{}", source_root.display()),
            &source_root,
        )
        .await
    }

    /// The source root inside a host dependency tree: the configured `dir` under it, or the
    /// auto-detected conventional layout (see [`conventional_source_root`](Self::conventional_source_root)).
    fn host_source_root(root: &Path, configured: Option<&str>) -> Result<PathBuf, String> {
        if let Some(configured) = configured {
            let selected = Self::resolve_scoped_host_path(root, configured)?;
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

    /// Resolve the configured source `dir` under a dependency `root`, refusing a path that
    /// escapes it.
    fn resolve_scoped_host_path(root: &Path, dir: &str) -> Result<PathBuf, String> {
        let selected = Self::resolve_host_path(root, dir);
        if !selected.starts_with(Self::normalize_host_path(root)) {
            return Err(format!(
                "source directory `{dir}` escapes its dependency root"
            ));
        }
        Ok(selected)
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
    async fn publish_source_tree(
        storage: &mut NativeStorage,
        name: &Name,
        namespace: CacheNamespace,
        identity: &str,
        source_root: &Path,
    ) -> Result<Vec<LibrarySource>, String> {
        let source = NativeSource::new(source_root.to_path_buf())
            .map_err(|error| error.to_string())?
            .excluding(RelativePath::parse(".git").expect(".git is a portable segment"))
            .scoped([NativeScope::extension(RelativePath::ROOT, "java")]);
        let checkout = ProjectStorage::open(source, MemoryCache::default(), storage.exec().clone())
            .await
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
                .await
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
    fn classify(project_root: &Path, locator: &str) -> DependencyLocation {
        if !ExternalLocator::is_url(locator)
            && let Some(file) = Self::project_relative(project_root, locator)
                .and_then(|path| FileKey::new(path).ok())
        {
            return DependencyLocation::Project(file);
        }
        DependencyLocation::External {
            locator: ExternalLocator::new(locator),
            expected: None,
        }
    }

    /// Lexically normalize a host path without requiring it to exist. This preserves a leading
    /// root/prefix, removes `.` and redundant separators, and resolves `..` without allowing it to
    /// pop an absolute root.
    fn normalize_host_path(path: &Path) -> PathBuf {
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    if normalized
                        .components()
                        .next_back()
                        .is_some_and(|last| matches!(last, Component::Normal(_)))
                    {
                        normalized.pop();
                    } else if !normalized.has_root() {
                        normalized.push(component.as_os_str());
                    }
                }
                Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                    normalized.push(component.as_os_str());
                }
            }
        }
        normalized
    }

    fn resolve_host_path(root: &Path, raw: &str) -> PathBuf {
        let raw = Path::new(raw);
        if raw.is_absolute() {
            Self::normalize_host_path(raw)
        } else {
            Self::normalize_host_path(&root.join(raw))
        }
    }

    fn relative_to_project(project_root: &Path, path: &Path) -> Option<RelativePath> {
        let root = Self::normalize_host_path(project_root);
        let path = Self::normalize_host_path(path);
        let relative = path.strip_prefix(root).ok()?;
        let mut segments = Vec::new();
        for component in relative.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(name) => segments.push(Name::new(name.to_str()?).ok()?),
                Component::RootDir | Component::Prefix(_) | Component::ParentDir => return None,
            }
        }
        Some(RelativePath::new(segments))
    }

    /// Resolve a manifest host-path spelling against the manifest directory and retain it as a
    /// typed key only when the normalized result lies inside the project.
    fn project_relative(project_root: &Path, raw: &str) -> Option<RelativePath> {
        Self::relative_to_project(project_root, &Self::resolve_host_path(project_root, raw))
    }

    /// The in-project source root of a `path` dependency: the configured `dir` under it, or the
    /// auto-detected conventional layout (`src/main/java` → `src` → the directory itself).
    /// `Ok(None)` when the dependency lies outside the project root — the native
    /// materialization step scans those from the host filesystem instead.
    fn project_path_root(
        dependency: &PathDependency,
        project_root: &Path,
        view: &ProjectView,
    ) -> Result<Option<DirKey>, String> {
        let base_host = Self::resolve_host_path(project_root, &dependency.path);
        let Some(base) = Self::relative_to_project(project_root, &base_host) else {
            return Ok(None);
        };
        let root = if let Some(dir) = &dependency.dir {
            let selected = Self::resolve_scoped_host_path(&base_host, dir)?;
            Self::relative_to_project(project_root, &selected)
                .ok_or_else(|| format!("source directory `{dir}` leaves the project"))?
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
