//! Portable execution of typed build-task plans.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use jals_build::build_script::{
    BuildScriptCacheScope, BuildScriptEnvironment, BuildScriptError, BuildScriptLimits,
    BuildScriptOutput, BuildScriptSession, prepare_build_script, publish_prepared_build_script,
};
use jals_build::task::{
    TaskDigestAlgorithm, TaskFetchKind, TaskId, TaskNodeKind, TaskPlan, TaskTerminal,
};
use jals_classpath::{
    ExpectedDigest, ExternalArtifactResolver, ExternalArtifactSpec, ExternalLocator, Fetcher,
    NetworkPolicy, SourceTree, SourceTreeExtraction, SourceTreeLimits,
};
use jals_config::Manifest;
use jals_exec::Exec;
use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, Change, ContentDigest, DirKey, FileKey,
    ProjectStorage, ProjectView, RelativePath, SourceBackend,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Effects available to one task-plan host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildTaskHost {
    NoTerminals,
    ArtifactsOnly,
    Project,
}

/// One source tree ready for transactional publication by the root host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildTaskPublication {
    pub owner: String,
    pub destination: DirKey,
    pub tree: SourceTree,
}

/// Successfully evaluated terminal values. This type performs no project mutation itself.
#[derive(Debug, Default)]
pub struct BuildTaskExecution {
    pub classpath: Vec<CacheKey>,
    pub publications: Vec<BuildTaskPublication>,
}

/// Failure during capability preflight or task-node evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildTaskRunError {
    UnsupportedPublication,
    UnsupportedTerminal,
    PublicationBlocked(String),
    Node { id: TaskId, message: String },
    Terminal(String),
}

impl fmt::Display for BuildTaskRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPublication => {
                f.write_str("physical source-tree publication is not supported by this host")
            }
            Self::UnsupportedTerminal => {
                f.write_str("build-task terminals are not supported by this host")
            }
            Self::PublicationBlocked(path) => write!(
                f,
                "physical source-tree publication is deferred while `{path}` is open"
            ),
            Self::Node { id, message } => {
                write!(f, "build-task node {} failed: {message}", id.index())
            }
            Self::Terminal(message) => write!(f, "build-task terminal failed: {message}"),
        }
    }
}

impl core::error::Error for BuildTaskRunError {}

/// Root build-script result after task execution and combined project publication.
#[derive(Debug)]
pub struct RootBuildScriptOutput {
    pub script: Option<BuildScriptOutput>,
    pub task_classpath: Vec<CacheKey>,
}

/// Whether a root run may apply the exclusive source-tree publications its plan declares.
///
/// Publications are the only part of a build script that writes *outside* `target/jals`. A
/// `replace-root` terminal owns its destination completely: applying it removes every existing
/// descendant, including files the user wrote by hand and never checked in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePublication {
    /// Apply publications, replacing whatever the owned roots currently contain.
    Apply,
    /// Evaluate the plan, but leave the project's source tree exactly as it is.
    ///
    /// Managed output under `target/jals/build` is still published, so callers that only preview a
    /// command still see the sources and flags a script contributes.
    Skip,
}

/// Immutable inputs controlling one root build-script task execution.
pub struct RootBuildScriptOptions<'a> {
    pub manifest: &'a Manifest,
    pub environment: &'a BuildScriptEnvironment,
    pub limits: &'a BuildScriptLimits,
    pub network: NetworkPolicy,
    pub host: BuildTaskHost,
    pub blocked_files: &'a [FileKey],
    /// Whether exclusive source-tree publications may touch the project. See
    /// [`SourcePublication`].
    pub publications: SourcePublication,
}

/// Root build-script preparation, task, or storage failure.
#[derive(Debug)]
pub enum RootBuildScriptError {
    BuildScript(BuildScriptError),
    Task(BuildTaskRunError),
    Storage(jals_storage::Error),
    InvalidSourceRoot(String),
}

impl fmt::Display for RootBuildScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BuildScript(error) => error.fmt(f),
            Self::Task(error) => error.fmt(f),
            Self::Storage(error) => write!(f, "build-task publication failed: {error}"),
            Self::InvalidSourceRoot(root) => write!(f, "invalid build source root `{root}`"),
        }
    }
}

impl core::error::Error for RootBuildScriptError {}

impl From<BuildScriptError> for RootBuildScriptError {
    fn from(error: BuildScriptError) -> Self {
        Self::BuildScript(error)
    }
}

impl From<BuildTaskRunError> for RootBuildScriptError {
    fn from(error: BuildTaskRunError) -> Self {
        Self::Task(error)
    }
}

impl From<jals_storage::Error> for RootBuildScriptError {
    fn from(error: jals_storage::Error) -> Self {
        Self::Storage(error)
    }
}

enum TaskValue {
    Url(String),
    Digest(ExpectedDigest),
    ByteCount(usize),
    Json(Value),
    Text(String),
    Jar(CacheKey),
    SourceTree(SourceTree),
}

/// Namespace for executing a validated build-task plan.
pub struct BuildTaskExecutor;

const OWNERSHIP_FILE: &str = "target/jals/build/tasks/ownership-v1.json";
const OWNERSHIP_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnershipState {
    version: u32,
    owners: Vec<OwnerState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerState {
    script: String,
    owner: String,
    destination: String,
    plan_fingerprint: String,
    files: Vec<OwnedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedFile {
    path: String,
    digest: String,
}

impl BuildTaskExecutor {
    /// Exclusive publication roots recorded by the current ownership state.
    pub fn owned_publication_roots(
        view: &ProjectView,
        source_roots: &[DirKey],
    ) -> Result<Vec<DirKey>, BuildTaskRunError> {
        let key = FileKey::parse(OWNERSHIP_FILE)
            .expect("build-task ownership path is a portable file key");
        let state = match view.file(&key) {
            Ok(file) => Self::decode_ownership(file.bytes())?,
            Err(jals_storage::Error::NotFoundFile(_)) => return Ok(Vec::new()),
            Err(error) => {
                return Err(BuildTaskRunError::Terminal(format!(
                    "could not read build-task ownership: {error}"
                )));
            }
        };
        let roots: Vec<_> = state
            .owners
            .into_iter()
            .map(|owner| {
                DirKey::parse(&owner.destination).map_err(|error| {
                    BuildTaskRunError::Terminal(format!(
                        "stored publication root is invalid: {error:?}"
                    ))
                })
            })
            .collect::<Result<_, _>>()?;
        if roots.iter().any(|root| {
            !source_roots.iter().any(|source| {
                root.path() != source.path() && root.path().starts_with(source.path())
            })
        }) {
            return Err(BuildTaskRunError::Terminal(
                "stored publication root is outside the configured source roots".to_owned(),
            ));
        }
        Ok(roots)
    }

    /// Prepare and execute one root build script, then atomically publish ordinary and task output.
    pub async fn execute_root<F, S, C>(
        exec: &Exec,
        fetcher: &F,
        storage: &mut ProjectStorage<S, C>,
        session: &mut BuildScriptSession,
        options: RootBuildScriptOptions<'_>,
    ) -> Result<RootBuildScriptOutput, RootBuildScriptError>
    where
        F: Fetcher,
        S: SourceBackend,
        C: CacheBackend,
    {
        let source_roots: Vec<_> = options
            .manifest
            .build
            .source_dirs
            .iter()
            .map(|root| {
                DirKey::parse(root)
                    .map_err(|_| RootBuildScriptError::InvalidSourceRoot(root.clone()))
            })
            .collect::<Result<_, _>>()?;
        let view = storage.view();
        let prepared = prepare_build_script(
            &view,
            storage.artifacts(),
            BuildScriptCacheScope::ROOT,
            options.manifest,
            options.environment,
            options.limits,
        )
        .await?;
        let Some(prepared) = prepared else {
            Self::reject_blocked_roots(
                &Self::owned_publication_roots(&view, &source_roots)?,
                options.blocked_files,
            )?;
            let changes = match options.publications {
                SourcePublication::Apply => {
                    Self::publication_changes(
                        &view,
                        storage.artifacts(),
                        &FileKey::parse("jals.toml").expect("manifest path is portable"),
                        &TaskPlan::new(),
                        &BuildTaskExecution::default(),
                        &source_roots,
                    )
                    .await?
                }
                // Dropping a script retires its owned roots, which is a removal like any other.
                SourcePublication::Skip => Vec::new(),
            };
            if !changes.is_empty() {
                let mut transaction = storage.transaction(view.revision())?;
                transaction.stage_changes(changes)?;
                transaction.commit().await?;
            }
            return Ok(RootBuildScriptOutput {
                script: None,
                task_classpath: Vec::new(),
            });
        };
        let plan = prepared.output(view.revision()).task_plan;
        if !Self::plan_publications_current(&view, prepared.script_path(), &plan) {
            Self::reject_blocked_roots(&Self::publication_roots(&plan)?, options.blocked_files)?;
        }
        let execution = Self::execute(
            exec,
            fetcher,
            &view,
            storage.artifacts_mut(),
            &plan,
            options.network,
            options.host,
        )
        .await?;
        let changes = match options.publications {
            SourcePublication::Apply => {
                Self::publication_changes(
                    &view,
                    storage.artifacts(),
                    prepared.script_path(),
                    &plan,
                    &execution,
                    &source_roots,
                )
                .await?
            }
            SourcePublication::Skip => Vec::new(),
        };
        let task_classpath = execution.classpath;
        let script =
            publish_prepared_build_script(storage, &view, &prepared, session, changes).await?;
        Ok(RootBuildScriptOutput {
            script,
            task_classpath,
        })
    }

    fn reject_blocked_roots(
        roots: &[DirKey],
        blocked_files: &[FileKey],
    ) -> Result<(), BuildTaskRunError> {
        if let Some(blocked) = blocked_files.iter().find(|file| {
            roots
                .iter()
                .any(|root| file.path().starts_with(root.path()))
        }) {
            return Err(BuildTaskRunError::PublicationBlocked(blocked.to_string()));
        }
        Ok(())
    }

    fn plan_publications_current(view: &ProjectView, script: &FileKey, plan: &TaskPlan) -> bool {
        let ownership = FileKey::parse(OWNERSHIP_FILE)
            .expect("build-task ownership path is a portable file key");
        let Some(state) = view
            .file(&ownership)
            .ok()
            .and_then(|file| Self::decode_ownership(file.bytes()).ok())
        else {
            return !plan
                .terminals
                .iter()
                .any(|terminal| matches!(terminal, TaskTerminal::PublishTree { .. }));
        };
        let Some(fingerprint) = serde_json::to_vec(plan)
            .ok()
            .map(|bytes| ContentDigest::of(&bytes).to_hex())
        else {
            return false;
        };
        let mut declared: Vec<_> = plan
            .terminals
            .iter()
            .filter_map(|terminal| match terminal {
                TaskTerminal::PublishTree {
                    owner, destination, ..
                } => Some((script.to_string(), owner.clone(), destination.clone())),
                TaskTerminal::AddClasspath { .. } | TaskTerminal::AddNestedClasspath { .. } => None,
            })
            .collect();
        declared.sort();
        let recorded: Vec<_> = state
            .owners
            .iter()
            .map(|owner| {
                (
                    owner.script.clone(),
                    owner.owner.clone(),
                    owner.destination.clone(),
                )
            })
            .collect();
        declared == recorded
            && state
                .owners
                .iter()
                .all(|owner| owner.plan_fingerprint == fingerprint)
            && Self::published_trees_match(view, &state)
    }

    pub async fn execute<F: Fetcher, C: CacheBackend>(
        exec: &Exec,
        fetcher: &F,
        view: &ProjectView,
        cache: &mut ArtifactCache<C>,
        plan: &TaskPlan,
        network: NetworkPolicy,
        host: BuildTaskHost,
    ) -> Result<BuildTaskExecution, BuildTaskRunError> {
        if host != BuildTaskHost::Project
            && plan
                .terminals
                .iter()
                .any(|terminal| matches!(terminal, TaskTerminal::PublishTree { .. }))
        {
            return Err(BuildTaskRunError::UnsupportedPublication);
        }
        if host == BuildTaskHost::NoTerminals && !plan.terminals.is_empty() {
            return Err(BuildTaskRunError::UnsupportedTerminal);
        }

        let reachable = Self::reachable(plan);
        let mut values: Vec<Option<TaskValue>> = (0..plan.nodes.len()).map(|_| None).collect();
        for node in &plan.nodes {
            if !reachable.contains(&node.id) {
                continue;
            }
            let value =
                Self::execute_node(exec, fetcher, view, cache, &values, &node.kind, network)
                    .await
                    .map_err(|message| BuildTaskRunError::Node {
                        id: node.id,
                        message,
                    })?;
            values[node.id.index()] = Some(value);
        }

        let mut output = BuildTaskExecution::default();
        for terminal in &plan.terminals {
            match terminal {
                TaskTerminal::AddClasspath { jar } => {
                    output.classpath.push(
                        Self::jar(&values, *jar)
                            .map_err(BuildTaskRunError::Terminal)?
                            .clone(),
                    );
                }
                TaskTerminal::AddNestedClasspath { jar } => {
                    let parent = Self::jar(&values, *jar)
                        .map_err(BuildTaskRunError::Terminal)?
                        .clone();
                    let nested = jals_classpath::NestedJar::extract_all(exec, cache, &parent)
                        .await
                        .map_err(BuildTaskRunError::Terminal)?;
                    output.classpath.extend(nested);
                }
                TaskTerminal::PublishTree {
                    owner,
                    tree,
                    destination,
                    ..
                } => {
                    let tree = Self::source_tree(&values, *tree)
                        .map_err(BuildTaskRunError::Terminal)?
                        .clone();
                    if tree.files.is_empty() {
                        return Err(BuildTaskRunError::Terminal(format!(
                            "publication owner `{owner}` produced an empty source tree"
                        )));
                    }
                    let destination = DirKey::parse(destination).map_err(|error| {
                        BuildTaskRunError::Terminal(format!(
                            "publication owner `{owner}` has invalid destination: {error:?}"
                        ))
                    })?;
                    output.publications.push(BuildTaskPublication {
                        owner: owner.clone(),
                        destination,
                        tree,
                    });
                }
            }
        }
        Ok(output)
    }

    /// Publication destinations declared by a plan, in terminal order.
    pub fn publication_roots(plan: &TaskPlan) -> Result<Vec<DirKey>, BuildTaskRunError> {
        plan.terminals
            .iter()
            .filter_map(|terminal| match terminal {
                TaskTerminal::PublishTree { destination, .. } => Some(destination),
                TaskTerminal::AddClasspath { .. } | TaskTerminal::AddNestedClasspath { .. } => None,
            })
            .map(|destination| {
                DirKey::parse(destination).map_err(|error| {
                    BuildTaskRunError::Terminal(format!(
                        "invalid publication destination `{destination}`: {error:?}"
                    ))
                })
            })
            .collect()
    }

    /// Prepare the complete project change set for exclusive-root publication and ownership.
    /// No project bytes are changed until the caller combines and commits these changes.
    pub async fn publication_changes<C: CacheBackend>(
        view: &ProjectView,
        cache: &ArtifactCache<C>,
        script: &FileKey,
        plan: &TaskPlan,
        execution: &BuildTaskExecution,
        source_roots: &[DirKey],
    ) -> Result<Vec<Change>, BuildTaskRunError> {
        let ownership_key = FileKey::parse(OWNERSHIP_FILE)
            .expect("build-task ownership path is a portable file key");
        let previous = match view.file(&ownership_key) {
            Ok(file) => Some(Self::decode_ownership(file.bytes())?),
            Err(jals_storage::Error::NotFoundFile(_)) => None,
            Err(error) => {
                return Err(BuildTaskRunError::Terminal(format!(
                    "could not read build-task ownership: {error}"
                )));
            }
        };

        let fingerprint = ContentDigest::of(&serde_json::to_vec(plan).map_err(|error| {
            BuildTaskRunError::Terminal(format!("could not fingerprint task plan: {error}"))
        })?)
        .to_hex();
        let mut owner_names = BTreeSet::new();
        let mut destinations = BTreeSet::new();
        let build_root =
            DirKey::parse("target/jals").expect("build-task managed root is a portable directory");
        for publication in &execution.publications {
            if !owner_names.insert(publication.owner.as_str()) {
                return Err(BuildTaskRunError::Terminal(format!(
                    "duplicate publication owner `{}`",
                    publication.owner
                )));
            }
            let destination = &publication.destination;
            if destination.path().starts_with(build_root.path())
                || !source_roots.iter().any(|root| {
                    destination.path() != root.path() && destination.path().starts_with(root.path())
                })
                || script.path().starts_with(destination.path())
            {
                return Err(BuildTaskRunError::Terminal(format!(
                    "publication destination `{destination}` must be a strict source-root descendant and must not contain managed inputs"
                )));
            }
            for existing in &destinations {
                if destination.path().starts_with(existing)
                    || existing.starts_with(destination.path())
                {
                    return Err(BuildTaskRunError::Terminal(format!(
                        "publication destination `{destination}` overlaps another exclusive root"
                    )));
                }
            }
            destinations.insert(destination.path().clone());
        }

        let mut remove_roots: BTreeSet<DirKey> = previous
            .as_ref()
            .into_iter()
            .flat_map(|state| &state.owners)
            .map(|owner| DirKey::parse(&owner.destination))
            .collect::<Result<_, _>>()
            .map_err(|error| {
                BuildTaskRunError::Terminal(format!(
                    "stored build-task ownership has an invalid destination: {error:?}"
                ))
            })?;
        for root in &remove_roots {
            if root.path().starts_with(build_root.path())
                || !source_roots.iter().any(|source| {
                    root.path() != source.path() && root.path().starts_with(source.path())
                })
            {
                return Err(BuildTaskRunError::Terminal(format!(
                    "stored exclusive publication root `{root}` is outside the configured source roots"
                )));
            }
        }
        remove_roots.extend(
            execution
                .publications
                .iter()
                .map(|publication| publication.destination.clone()),
        );

        let mut changes = Vec::new();
        for root in remove_roots {
            let root_file = FileKey::new(root.path().clone()).map_err(|error| {
                BuildTaskRunError::Terminal(format!(
                    "exclusive publication root `{root}` is invalid: {error:?}"
                ))
            })?;
            if view.tree().file(&root_file).is_some() {
                return Err(BuildTaskRunError::Terminal(format!(
                    "exclusive publication root `{root}` collides with a file"
                )));
            }
            if view.tree().directory(&root).is_some() {
                changes.push(Change::RemoveDirectory(root));
            }
        }

        let mut owners = Vec::new();
        for publication in &execution.publications {
            let mut owned_files = Vec::new();
            for source in &publication.tree.files {
                let bytes = cache
                    .lookup(&source.key)
                    .await
                    .map_err(|error| {
                        BuildTaskRunError::Terminal(format!(
                            "source artifact `{}` is invalid: {error:?}",
                            source.path
                        ))
                    })?
                    .ok_or_else(|| {
                        BuildTaskRunError::Terminal(format!(
                            "source artifact `{}` is missing",
                            source.path
                        ))
                    })?;
                let key = publication
                    .destination
                    .file_at(&source.path)
                    .map_err(|error| {
                        BuildTaskRunError::Terminal(format!(
                            "source path `{}` cannot be published: {error:?}",
                            source.path
                        ))
                    })?;
                owned_files.push(OwnedFile {
                    path: source.path.to_string(),
                    digest: ContentDigest::of(&bytes).to_hex(),
                });
                changes.push(Change::CreateFile(key, bytes.into()));
            }
            owners.push(OwnerState {
                script: script.to_string(),
                owner: publication.owner.clone(),
                destination: publication.destination.to_string(),
                plan_fingerprint: fingerprint.clone(),
                files: owned_files,
            });
        }
        owners
            .sort_by(|left, right| (&left.script, &left.owner).cmp(&(&right.script, &right.owner)));

        if owners.is_empty() {
            if previous.is_some() {
                changes.push(Change::RemoveFile(ownership_key));
            }
        } else {
            let state = OwnershipState {
                version: OWNERSHIP_VERSION,
                owners,
            };
            if previous.as_ref() == Some(&state) && Self::published_trees_match(view, &state) {
                return Ok(Vec::new());
            }
            let bytes = serde_json::to_vec(&state).map_err(|error| {
                BuildTaskRunError::Terminal(format!(
                    "could not serialize build-task ownership: {error}"
                ))
            })?;
            if previous.is_some() {
                changes.push(Change::ReplaceFile(ownership_key, bytes.into()));
            } else {
                changes.push(Change::CreateFile(ownership_key, bytes.into()));
            }
        }
        Ok(changes)
    }

    fn published_trees_match(view: &ProjectView, state: &OwnershipState) -> bool {
        for owner in &state.owners {
            let Ok(destination) = DirKey::parse(&owner.destination) else {
                return false;
            };
            let expected: BTreeMap<_, _> = owner
                .files
                .iter()
                .filter_map(|file| {
                    let relative = RelativePath::parse(&file.path).ok()?;
                    let key = destination.file_at(&relative).ok()?;
                    let digest = ContentDigest::from_hex(&file.digest)?;
                    Some((key, digest))
                })
                .collect();
            if expected.len() != owner.files.len() {
                return false;
            }
            let actual: Vec<_> = view.tree().files_under(&destination).collect();
            if actual.len() != expected.len()
                || actual.iter().any(|file| {
                    expected
                        .get(file.key())
                        .is_none_or(|digest| ContentDigest::of(file.bytes()) != *digest)
                })
            {
                return false;
            }
        }
        true
    }

    fn decode_ownership(bytes: &[u8]) -> Result<OwnershipState, BuildTaskRunError> {
        let state: OwnershipState = serde_json::from_slice(bytes).map_err(|error| {
            BuildTaskRunError::Terminal(format!("build-task ownership is corrupt: {error}"))
        })?;
        if state.version != OWNERSHIP_VERSION
            || serde_json::to_vec(&state).ok().as_deref() != Some(bytes)
            || state
                .owners
                .windows(2)
                .any(|pair| (&pair[0].script, &pair[0].owner) >= (&pair[1].script, &pair[1].owner))
        {
            return Err(BuildTaskRunError::Terminal(
                "build-task ownership is not canonical".to_owned(),
            ));
        }
        for owner in &state.owners {
            if owner.owner.is_empty()
                || DirKey::parse(&owner.destination).is_err()
                || ContentDigest::from_hex(&owner.plan_fingerprint).is_none()
                || owner
                    .files
                    .windows(2)
                    .any(|pair| pair[0].path >= pair[1].path)
                || owner.files.iter().any(|file| {
                    FileKey::parse(&file.path).is_err()
                        || ContentDigest::from_hex(&file.digest).is_none()
                })
            {
                return Err(BuildTaskRunError::Terminal(
                    "build-task ownership contains invalid entries".to_owned(),
                ));
            }
        }
        Ok(state)
    }

    async fn execute_node<F: Fetcher, C: CacheBackend>(
        exec: &Exec,
        fetcher: &F,
        view: &ProjectView,
        cache: &mut ArtifactCache<C>,
        values: &[Option<TaskValue>],
        node: &TaskNodeKind,
        network: NetworkPolicy,
    ) -> Result<TaskValue, String> {
        match node {
            TaskNodeKind::HttpsUrl { value } => {
                Self::validate_https(value)?;
                Ok(TaskValue::Url(value.clone()))
            }
            TaskNodeKind::ProjectJar { path } => {
                let key = FileKey::parse(path)
                    .map_err(|error| format!("invalid project JAR path: {error:?}"))?;
                let bytes = view
                    .file(&key)
                    .map_err(|error| format!("project JAR `{key}` cannot be read: {error}"))?
                    .bytes();
                let artifact = CacheKey::new(
                    CacheNamespace::BuildTaskArtifact,
                    ContentDigest::of(path.as_bytes()),
                    ContentDigest::of(bytes),
                );
                cache
                    .publish(&artifact, bytes)
                    .await
                    .map_err(|error| format!("project JAR cache publish failed: {error:?}"))?;
                Ok(TaskValue::Jar(artifact))
            }
            TaskNodeKind::Digest { algorithm, value } => {
                let digest = ExpectedDigest::from_hex(Self::algorithm(*algorithm), value)
                    .ok_or_else(|| "invalid canonical digest".to_owned())?;
                Ok(TaskValue::Digest(digest))
            }
            TaskNodeKind::ByteCount { value } => usize::try_from(*value)
                .map(TaskValue::ByteCount)
                .map_err(|_| "byte limit does not fit this host".to_owned()),
            TaskNodeKind::Fetch {
                kind,
                url,
                digest,
                max_bytes,
            } => {
                let url = Self::url(values, *url)?;
                Self::validate_https(url)?;
                let expected = Self::digest(values, *digest)?;
                let max_bytes = Self::byte_count(values, *max_bytes)?;
                let spec = ExternalArtifactSpec {
                    locator: ExternalLocator::new(url),
                    expected,
                    max_bytes,
                    namespace: CacheNamespace::BuildTaskArtifact,
                };
                let key = ExternalArtifactResolver::resolve(fetcher, cache, &spec, network).await?;
                match kind {
                    TaskFetchKind::Jar => Ok(TaskValue::Jar(key)),
                    TaskFetchKind::Json => {
                        let bytes = cache
                            .lookup_bounded(&key, max_bytes)
                            .await
                            .map_err(|error| format!("cached JSON is invalid: {error:?}"))?
                            .ok_or_else(|| "cached JSON disappeared".to_owned())?;
                        serde_json::from_slice(&bytes)
                            .map(TaskValue::Json)
                            .map_err(|error| format!("invalid JSON response: {error}"))
                    }
                    TaskFetchKind::Text => {
                        let bytes = cache
                            .lookup_bounded(&key, max_bytes)
                            .await
                            .map_err(|error| format!("cached text is invalid: {error:?}"))?
                            .ok_or_else(|| "cached text disappeared".to_owned())?;
                        String::from_utf8(bytes)
                            .map(TaskValue::Text)
                            .map_err(|error| format!("fetched text is not UTF-8: {error}"))
                    }
                }
            }
            TaskNodeKind::JsonAt { json, path } => Self::json_at(Self::json(values, *json)?, path)
                .cloned()
                .map(TaskValue::Json)
                .ok_or_else(|| format!("JSON path `{}` does not exist", path.join("/"))),
            TaskNodeKind::JsonFindString {
                json,
                path,
                field,
                value,
            } => {
                let array = Self::json_at(Self::json(values, *json)?, path)
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("JSON path `{}` is not an array", path.join("/")))?;
                array
                    .iter()
                    .find(|item| item.get(field).and_then(Value::as_str) == Some(value))
                    .cloned()
                    .map(TaskValue::Json)
                    .ok_or_else(|| format!("JSON array contains no `{field}` equal to `{value}`"))
            }
            TaskNodeKind::JsonUrl { json, path } => {
                let value = Self::json_scalar(Self::json(values, *json)?, path)?
                    .as_str()
                    .ok_or_else(|| "projected JSON URL is not a string".to_owned())?;
                Self::validate_https(value)?;
                Ok(TaskValue::Url(value.to_owned()))
            }
            TaskNodeKind::JsonDigest {
                json,
                path,
                algorithm,
            } => {
                let value = Self::json_scalar(Self::json(values, *json)?, path)?
                    .as_str()
                    .ok_or_else(|| "projected JSON digest is not a string".to_owned())?;
                ExpectedDigest::from_hex(Self::algorithm(*algorithm), value)
                    .map(TaskValue::Digest)
                    .ok_or_else(|| "projected JSON digest is not canonical".to_owned())
            }
            TaskNodeKind::JsonU64 { json, path } => {
                Self::json_scalar(Self::json(values, *json)?, path)?
                    .as_u64()
                    .ok_or_else(|| {
                        "projected JSON byte count is not an unsigned integer".to_owned()
                    })
                    .and_then(|value| {
                        usize::try_from(value)
                            .map(TaskValue::ByteCount)
                            .map_err(|_| {
                                "projected JSON byte count does not fit this host".to_owned()
                            })
                    })
            }
            TaskNodeKind::ExtractJava { jar, prefix } => {
                let jar = Self::jar(values, *jar)?.clone();
                let prefix = RelativePath::parse(prefix)
                    .map_err(|error| format!("invalid extraction prefix: {error:?}"))?;
                SourceTreeExtraction::java(
                    exec,
                    cache,
                    &jar,
                    &prefix,
                    SourceTreeLimits {
                        max_files: 100_000,
                        max_file_bytes: 16 * 1_048_576,
                        max_total_bytes: 1_024 * 1_048_576,
                    },
                )
                .await
                .map(TaskValue::SourceTree)
            }
            TaskNodeKind::NestedJar { jar, member } => {
                let jar = Self::jar(values, *jar)?.clone();
                jals_classpath::NestedJar::extract(exec, cache, &jar, member)
                    .await
                    .map(TaskValue::Jar)
            }
            TaskNodeKind::RemapJar { jar, mappings } => {
                let jar = Self::jar(values, *jar)?.clone();
                let mappings = Self::text(values, *mappings)?;
                jals_classpath::JarRemap::remap(exec, cache, &jar, mappings)
                    .await
                    .map(TaskValue::Jar)
            }
            TaskNodeKind::MergeJars { base, overlay } => {
                let base = Self::jar(values, *base)?.clone();
                let overlay = Self::jar(values, *overlay)?.clone();
                jals_classpath::JarMerge::merge(exec, cache, &base, &overlay)
                    .await
                    .map(TaskValue::Jar)
            }
            TaskNodeKind::DecompileJava { jar, prefix } => {
                let jar = Self::jar(values, *jar)?.clone();
                let prefix = RelativePath::parse(prefix)
                    .map_err(|error| format!("invalid decompile prefix: {error:?}"))?;
                SourceTreeExtraction::decompile(
                    exec,
                    cache,
                    &jar,
                    &prefix,
                    SourceTreeLimits {
                        max_files: 100_000,
                        max_file_bytes: 16 * 1_048_576,
                        max_total_bytes: 1_024 * 1_048_576,
                    },
                )
                .await
                .map(TaskValue::SourceTree)
            }
        }
    }

    fn reachable(plan: &TaskPlan) -> BTreeSet<TaskId> {
        let mut reachable = BTreeSet::new();
        let mut pending: Vec<_> = plan.terminals.iter().map(TaskTerminal::input_id).collect();
        while let Some(id) = pending.pop() {
            if !reachable.insert(id) {
                continue;
            }
            if let Some(node) = plan.node(id) {
                pending.extend(node.kind.input_ids());
            }
        }
        reachable
    }

    fn validate_https(value: &str) -> Result<(), String> {
        if value.starts_with("https://") && !value.bytes().any(|byte| byte.is_ascii_whitespace()) {
            Ok(())
        } else {
            Err("task URL must be an HTTPS URL without whitespace".to_owned())
        }
    }

    const fn algorithm(algorithm: TaskDigestAlgorithm) -> &'static str {
        match algorithm {
            TaskDigestAlgorithm::Sha1 => "sha1",
            TaskDigestAlgorithm::Sha256 => "sha256",
        }
    }

    fn value(values: &[Option<TaskValue>], id: TaskId) -> Result<&TaskValue, String> {
        values
            .get(id.index())
            .and_then(Option::as_ref)
            .ok_or_else(|| format!("input node {} has no value", id.index()))
    }

    fn url(values: &[Option<TaskValue>], id: TaskId) -> Result<&str, String> {
        match Self::value(values, id)? {
            TaskValue::Url(value) => Ok(value),
            _ => Err("task input is not a URL".to_owned()),
        }
    }

    fn digest(values: &[Option<TaskValue>], id: TaskId) -> Result<ExpectedDigest, String> {
        match Self::value(values, id)? {
            TaskValue::Digest(value) => Ok(*value),
            _ => Err("task input is not a digest".to_owned()),
        }
    }

    fn byte_count(values: &[Option<TaskValue>], id: TaskId) -> Result<usize, String> {
        match Self::value(values, id)? {
            TaskValue::ByteCount(value) => Ok(*value),
            _ => Err("task input is not a byte count".to_owned()),
        }
    }

    fn json(values: &[Option<TaskValue>], id: TaskId) -> Result<&Value, String> {
        match Self::value(values, id)? {
            TaskValue::Json(value) => Ok(value),
            _ => Err("task input is not JSON".to_owned()),
        }
    }

    fn text(values: &[Option<TaskValue>], id: TaskId) -> Result<&str, String> {
        match Self::value(values, id)? {
            TaskValue::Text(value) => Ok(value),
            _ => Err("task input is not text".to_owned()),
        }
    }

    fn jar(values: &[Option<TaskValue>], id: TaskId) -> Result<&CacheKey, String> {
        match Self::value(values, id)? {
            TaskValue::Jar(value) => Ok(value),
            _ => Err("task input is not a JAR".to_owned()),
        }
    }

    fn source_tree(values: &[Option<TaskValue>], id: TaskId) -> Result<&SourceTree, String> {
        match Self::value(values, id)? {
            TaskValue::SourceTree(value) => Ok(value),
            _ => Err("task input is not a source tree".to_owned()),
        }
    }

    fn json_at<'a>(mut value: &'a Value, path: &[String]) -> Option<&'a Value> {
        for segment in path {
            value = value.get(segment)?;
        }
        Some(value)
    }

    fn json_scalar<'a>(value: &'a Value, path: &[String]) -> Result<&'a Value, String> {
        Self::json_at(value, path)
            .ok_or_else(|| format!("JSON path `{}` does not exist", path.join("/")))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap as StdBTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use jals_config::BuildScript;
    use jals_exec::block_on_inline;
    use jals_storage::{CodeTree, Entry, MemoryStorage, Name};

    use super::*;

    struct MockFetcher {
        responses: StdBTreeMap<String, Vec<u8>>,
        calls: AtomicUsize,
    }

    impl Fetcher for MockFetcher {
        async fn fetch(&self, locator: &str) -> Result<Vec<u8>, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.responses
                .get(locator)
                .cloned()
                .ok_or_else(|| format!("unexpected fetch `{locator}`"))
        }
    }

    fn manifest() -> Manifest {
        let mut manifest = Manifest::default();
        manifest.build.script = Some(BuildScript::Rhai {
            file: "build.rhai".to_owned(),
        });
        manifest
    }

    fn storage(script: &str) -> MemoryStorage {
        MemoryStorage::memory(
            CodeTree::new([Entry::File(
                FileKey::parse("build.rhai").unwrap(),
                script.as_bytes().to_vec(),
            )])
            .unwrap(),
        )
    }

    #[test]
    fn dynamic_json_downloads_resolve_and_reuse_verified_cache_offline() {
        block_on_inline(async {
            let jar = b"jar bytes";
            let metadata = format!(
                "{{\"download\":{{\"url\":\"https://example.invalid/game.jar\",\"sha256\":\"{}\",\"size\":{}}}}}",
                ContentDigest::of(jar).to_hex(),
                jar.len()
            );
            let script = format!(
                r#"
                    let metadata = tasks.fetch_json(
                        tasks.https_url("https://example.invalid/version.json"),
                        tasks.sha256("{}"),
                        tasks.bytes(4096)
                    );
                    let row = tasks.json_at(metadata, ["download"]);
                    let jar = tasks.fetch_jar(
                        tasks.json_url(row, ["url"]),
                        tasks.json_sha256(row, ["sha256"]),
                        tasks.json_u64(row, ["size"])
                    );
                    tasks.add_classpath(jar);
                "#,
                ContentDigest::of(metadata.as_bytes()).to_hex()
            );
            let mut storage = storage(&script);
            let online = MockFetcher {
                responses: [
                    (
                        "https://example.invalid/version.json".to_owned(),
                        metadata.into_bytes(),
                    ),
                    ("https://example.invalid/game.jar".to_owned(), jar.to_vec()),
                ]
                .into_iter()
                .collect(),
                calls: AtomicUsize::new(0),
            };
            let first = BuildTaskExecutor::execute_root(
                &Exec::inline(),
                &online,
                &mut storage,
                &mut BuildScriptSession::new(),
                RootBuildScriptOptions {
                    manifest: &manifest(),
                    environment: &BuildScriptEnvironment::new(),
                    limits: &BuildScriptLimits::default(),
                    network: NetworkPolicy::Online,
                    host: BuildTaskHost::Project,
                    blocked_files: &[],
                    publications: SourcePublication::Apply,
                },
            )
            .await
            .unwrap();
            assert_eq!(online.calls.load(Ordering::Relaxed), 2);
            assert_eq!(first.task_classpath.len(), 1);

            let offline = MockFetcher {
                responses: StdBTreeMap::new(),
                calls: AtomicUsize::new(0),
            };
            let second = BuildTaskExecutor::execute_root(
                &Exec::inline(),
                &offline,
                &mut storage,
                &mut BuildScriptSession::new(),
                RootBuildScriptOptions {
                    manifest: &manifest(),
                    environment: &BuildScriptEnvironment::new(),
                    limits: &BuildScriptLimits::default(),
                    network: NetworkPolicy::Offline,
                    host: BuildTaskHost::Project,
                    blocked_files: &[],
                    publications: SourcePublication::Apply,
                },
            )
            .await
            .unwrap();
            assert_eq!(offline.calls.load(Ordering::Relaxed), 0);
            assert_eq!(second.task_classpath, first.task_classpath);
        });
    }

    #[test]
    fn unsupported_publication_is_rejected_before_fetch() {
        block_on_inline(async {
            let script = format!(
                r#"
                    let jar = tasks.fetch_jar(
                        tasks.https_url("https://example.invalid/source.jar"),
                        tasks.sha256("{}"),
                        tasks.bytes(1024)
                    );
                    let sources = tasks.extract_java(jar, "net/example");
                    tasks.publish_tree("sources", sources, "src/main/java/net/example", "replace-root");
                "#,
                ContentDigest::of(b"jar").to_hex()
            );
            let mut storage = storage(&script);
            let fetcher = MockFetcher {
                responses: StdBTreeMap::new(),
                calls: AtomicUsize::new(0),
            };
            let error = BuildTaskExecutor::execute_root(
                &Exec::inline(),
                &fetcher,
                &mut storage,
                &mut BuildScriptSession::new(),
                RootBuildScriptOptions {
                    manifest: &manifest(),
                    environment: &BuildScriptEnvironment::new(),
                    limits: &BuildScriptLimits::default(),
                    network: NetworkPolicy::Online,
                    host: BuildTaskHost::ArtifactsOnly,
                    blocked_files: &[],
                    publications: SourcePublication::Apply,
                },
            )
            .await
            .unwrap_err();
            assert!(matches!(
                error,
                RootBuildScriptError::Task(BuildTaskRunError::UnsupportedPublication)
            ));
            assert_eq!(fetcher.calls.load(Ordering::Relaxed), 0);
            assert_eq!(storage.revision(), jals_storage::Revision::INITIAL);
        });
    }

    #[test]
    fn exclusive_publication_is_noop_when_equal_and_replaces_the_whole_root_when_changed() {
        block_on_inline(async {
            let mut storage = MemoryStorage::memory(
                CodeTree::new([Entry::File(
                    FileKey::parse("build.rhai").unwrap(),
                    Vec::new(),
                )])
                .unwrap(),
            );
            let source_bytes = b"package net.example; class A {}";
            let source_key = CacheKey::new(
                CacheNamespace::BuildTaskSource,
                ContentDigest::of(b"source"),
                ContentDigest::of(source_bytes),
            );
            storage
                .artifacts_mut()
                .publish(&source_key, source_bytes)
                .await
                .unwrap();
            let execution = BuildTaskExecution {
                classpath: Vec::new(),
                publications: vec![BuildTaskPublication {
                    owner: "sources".to_owned(),
                    destination: DirKey::parse("src/main/java/net/example").unwrap(),
                    tree: SourceTree {
                        files: vec![jals_classpath::LibrarySource {
                            path: RelativePath::new([Name::new("A.java").unwrap()]),
                            key: source_key,
                        }],
                    },
                }],
            };
            let roots = [DirKey::parse("src/main/java").unwrap()];
            let script = FileKey::parse("build.rhai").unwrap();
            let plan = TaskPlan::new();

            let view = storage.view();
            let changes = BuildTaskExecutor::publication_changes(
                &view,
                storage.artifacts(),
                &script,
                &plan,
                &execution,
                &roots,
            )
            .await
            .unwrap();
            let mut transaction = storage.transaction(view.revision()).unwrap();
            transaction.stage_changes(changes).unwrap();
            transaction.commit().await.unwrap();

            let view = storage.view();
            assert!(
                BuildTaskExecutor::publication_changes(
                    &view,
                    storage.artifacts(),
                    &script,
                    &plan,
                    &execution,
                    &roots,
                )
                .await
                .unwrap()
                .is_empty()
            );

            let generated = FileKey::parse("src/main/java/net/example/A.java").unwrap();
            let manual = FileKey::parse("src/main/java/net/example/Manual.txt").unwrap();
            let mut transaction = storage.transaction(storage.revision()).unwrap();
            transaction
                .replace_file(generated.clone(), b"edited".to_vec())
                .unwrap();
            transaction
                .create_file(manual.clone(), b"manual".to_vec())
                .unwrap();
            transaction.commit().await.unwrap();

            let view = storage.view();
            let changes = BuildTaskExecutor::publication_changes(
                &view,
                storage.artifacts(),
                &script,
                &plan,
                &execution,
                &roots,
            )
            .await
            .unwrap();
            let mut transaction = storage.transaction(view.revision()).unwrap();
            transaction.stage_changes(changes).unwrap();
            transaction.commit().await.unwrap();
            assert_eq!(
                storage.view().file(&generated).unwrap().bytes(),
                source_bytes
            );
            assert!(storage.view().file(&manual).is_err());
        });
    }
}
