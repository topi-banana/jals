//! The resolver: one version per name, unified features, deterministic output.
//!
//! The procedure is a fixpoint over *passes*, and a pass is a single deterministic walk:
//!
//! 1. roots are seeded with their selected features and their active edges;
//! 2. every edge records a requirement, a source, and requested features on its target;
//! 3. a package with no chosen version yet picks one — the lock pin when it satisfies everything
//!    seen so far, otherwise the greatest candidate that does;
//! 4. feature expansion closes each package's own graph, routes `<dep>/<feature>` forwards,
//!    activates `dep:`/implicit optional dependencies, and enqueues whatever that reveals;
//! 5. when the queues drain, the chosen versions are re-validated against *every* requirement a
//!    pass collected. A late requirement that invalidates an early choice becomes the next
//!    pass's pin, and the pass repeats.
//!
//! There is no backtracking. Requirements are intersected rather than arbitrated in discovery
//! order, every step is monotone (versions are only ever pinned after satisfying more), and the
//! round budget turns a pathological provider into an error instead of a hang — the same shape
//! Cargo's resolver reduces to once conflicting requirements are an error rather than a search.
//!
//! Cycles are ordinary edges. Summaries are memoized by [`PackageId`], so `a -> b -> a` enqueues
//! a package that is already chosen and terminates. `jals-project` still refuses a *source*
//! cycle at compile time, because Java cannot compile one; that is a build-graph question, not a
//! resolution question.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use sha2::{Digest, Sha256};

use crate::error::{RequirementOrigin, ResolveError, ResolveWarning};
use crate::id::{Checksum, PackageId, PackageName};
use crate::lock::{LOCK_VERSION, LockedPackage, Lockfile};
use crate::summary::{
    Candidate, DependencyKind, DependencyRequest, FeatureValue, RootRequest, SourceRequest, Summary,
};

/// How many passes a resolution may take before it is declared non-convergent.
const DEFAULT_MAX_ROUNDS: usize = 32;

/// One candidate fetch the resolver batches: a declaration plus, when a lock pinned it, the
/// locked package the provider must keep reachable (a git commit, a registry version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRequest {
    /// The dependency declaration being resolved.
    pub request: DependencyRequest,
    /// The locked package for this name, when the previous lock has one.
    pub locked: Option<LockedPackage>,
}

/// A source of package candidates and summaries.
///
/// One implementation exists per source kind, and each owns all of that kind's I/O: the Maven
/// provider lists versions from `maven-metadata.xml` and reads POMs; the git provider clones,
/// checks out, and reads a manifest; the path provider canonicalizes and reads; the direct
/// provider hashes bytes. The resolver never sees a URL, a path, or a byte.
///
/// **Batching is how resolution becomes parallel.** A resolver pass can always name a whole set
/// of packages whose candidate lists or summaries it needs before it has to know any of their
/// contents, and it asks for them through [`candidates_batch`](Provider::candidates_batch) /
/// [`summaries_batch`](Provider::summaries_batch). The default bodies call the single-item
/// methods in order — correct for a provider with nothing to overlap — while a provider with a
/// fetcher overrides them to issue the independent requests concurrently (the native Maven
/// provider overlaps its HTTP reads with `jals_exec::join_ordered`). Determinism does not move:
/// the resolver zips results back against the requests in input order, and version *choices*
/// stay sequential in the resolver itself.
///
/// `async fn` in the trait is deliberate, exactly as in `jals-storage`'s backends: every future
/// in this workspace is `!Send` (runtimes are current-thread), so the auto-trait bound the lint
/// warns about is one this crate must not have.
#[allow(async_fn_in_trait)]
pub trait Provider {
    /// The provider's own failure type.
    type Error: fmt::Display + fmt::Debug;

    /// Every candidate this source can provide for `request`.
    ///
    /// Implementations return *all* versions for the source and let the resolver intersect
    /// requirements; they never filter by [`DependencyRequest::version`]. When `locked` is
    /// compatible with the request and still reachable (a commit still in the repository, a
    /// registry that still lists the version), it must appear among the candidates even if it is
    /// not the greatest, so a lock pin survives `jals build`.
    async fn candidates(
        &mut self,
        request: &DependencyRequest,
        locked: Option<&LockedPackage>,
    ) -> Result<Vec<Candidate>, Self::Error>;

    /// The summary for one exact candidate id.
    async fn summary(&mut self, id: &PackageId) -> Result<Summary, Self::Error>;

    /// Fetch candidate lists for a batch of independent requests.
    ///
    /// Results are aligned with `requests` index for index. The default is sequential; a
    /// provider that can overlap work overrides this, and the resolver's output does not change
    /// because it never inspects the results out of order.
    async fn candidates_batch(
        &mut self,
        requests: Vec<CandidateRequest>,
    ) -> Vec<Result<Vec<Candidate>, Self::Error>> {
        let mut results = Vec::with_capacity(requests.len());
        for request in &requests {
            results.push(
                self.candidates(&request.request, request.locked.as_ref())
                    .await,
            );
        }
        results
    }

    /// Fetch summaries for a batch of exact ids, aligned index for index. Default sequential.
    async fn summaries_batch(&mut self, ids: &[PackageId]) -> Vec<Result<Summary, Self::Error>> {
        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            results.push(self.summary(id).await);
        }
        results
    }
}

/// One resolved dependency edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDependency {
    /// The label the declaring manifest used.
    pub name: PackageName,
    /// The resolved target.
    pub target: PackageId,
    /// Build or dev.
    pub kind: DependencyKind,
}

/// One resolved non-root package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPackage {
    /// The exact package identity.
    pub id: PackageId,
    /// The artifact checksum, when known.
    pub checksum: Option<Checksum>,
    /// The unified feature set this resolution enabled.
    pub features: BTreeSet<String>,
    /// The labels of optional dependencies this resolution activated.
    pub activated: BTreeSet<String>,
    /// The active edges, label-sorted.
    pub dependencies: Vec<ResolvedDependency>,
}

/// One resolved root workspace member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoot {
    /// The member's identity.
    pub id: PackageId,
    /// The unified feature set the selection enabled.
    pub features: BTreeSet<String>,
    /// The active edges, label-sorted.
    pub dependencies: Vec<ResolvedDependency>,
}

/// A digest over the exact locked package set.
///
/// Cache keys fold this rather than locator strings: two resolutions that pin the same bytes
/// produce the same fingerprint even if a path moved or a git checkout directory was temporary,
/// and a version or checksum moving always moves it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint(String);

impl Fingerprint {
    /// The lowercase hex digest.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Hash a canonical rendering.
    fn of(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut hex = String::with_capacity(digest.len() * 2);
        for byte in digest {
            let _ = core::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
        }
        Self(hex)
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The outcome of resolving a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveGraph {
    /// The roots, in the order they were requested.
    pub roots: Vec<ResolvedRoot>,
    /// Every non-root package, id-sorted.
    pub packages: Vec<ResolvedPackage>,
    /// Warnings that did not stop resolution.
    pub warnings: Vec<ResolveWarning>,
    fingerprint: Fingerprint,
}

impl ResolveGraph {
    /// The resolution fingerprint.
    pub const fn fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    /// The resolved package with `name`, if any.
    pub fn package(&self, name: &PackageName) -> Option<&ResolvedPackage> {
        self.packages
            .iter()
            .find(|package| &package.id.name == name)
    }

    /// The locked package set this graph represents.
    pub fn lockfile(&self) -> Lockfile {
        Lockfile {
            version: LOCK_VERSION,
            packages: self
                .packages
                .iter()
                .map(|package| {
                    let dependencies: BTreeSet<PackageId> = package
                        .dependencies
                        .iter()
                        .map(|dependency| dependency.target.clone())
                        .collect();
                    LockedPackage {
                        id: package.id.clone(),
                        checksum: package.checksum.clone(),
                        dependencies: dependencies.into_iter().collect(),
                    }
                })
                .collect(),
        }
    }
}

/// The resolver.
///
/// Holds the memoized provider answers across passes: a package that survived pass 1 keeps its
/// candidate list and summary in pass 2, so a resolution that needs two passes over a diamond
/// fetches each version list and each POM once. Hosts that need a *persistent* cache (the native
/// Maven provider keeps registry bytes in the artifact cache) layer their own under the provider.
pub struct Resolver<'p, P: Provider> {
    provider: &'p mut P,
    max_rounds: usize,
    candidates: BTreeMap<(SourceRequest, PackageName), Vec<Candidate>>,
    summaries: BTreeMap<PackageId, Summary>,
}

impl<'p, P: Provider> Resolver<'p, P> {
    /// A resolver over `provider`.
    pub const fn new(provider: &'p mut P) -> Self {
        Self {
            provider,
            max_rounds: DEFAULT_MAX_ROUNDS,
            candidates: BTreeMap::new(),
            summaries: BTreeMap::new(),
        }
    }

    /// Override the round budget. Unreachable in practice; a test drives it deliberately.
    #[must_use]
    pub const fn with_max_rounds(mut self, rounds: usize) -> Self {
        self.max_rounds = rounds;
        self
    }

    /// Resolve `roots` against an optional lockfile.
    ///
    /// # Errors
    /// [`ResolveError`] for provider failures, unsatisfiable requirements, a name requested
    /// from two sources, or a resolution that does not stabilize.
    pub async fn resolve(
        &mut self,
        roots: &[RootRequest],
        lock: Option<&Lockfile>,
    ) -> Result<ResolveGraph, ResolveError<P::Error>> {
        let locked: BTreeMap<PackageName, LockedPackage> = lock
            .map(|lock| {
                lock.packages
                    .iter()
                    .map(|package| (package.id.name.clone(), package.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let mut pins: BTreeMap<PackageName, PackageId> = locked
            .iter()
            .map(|(name, package)| (name.clone(), package.id.clone()))
            .collect();
        for _ in 0..self.max_rounds {
            let pass = Pass::new(
                self.provider,
                roots,
                &locked,
                &pins,
                &mut self.candidates,
                &mut self.summaries,
            );
            let outcome = pass.run().await?;
            if outcome.pins == pins {
                return Ok(Self::assemble(roots, &outcome, lock));
            }
            pins = outcome.pins;
        }
        Err(ResolveError::NotConverged {
            rounds: self.max_rounds,
        })
    }

    /// Build the public graph from a converged pass.
    fn assemble(
        roots: &[RootRequest],
        outcome: &PassOutcome,
        lock: Option<&Lockfile>,
    ) -> ResolveGraph {
        let root_ids: BTreeSet<&PackageName> =
            roots.iter().map(|root| &root.summary.id.name).collect();
        let mut packages: Vec<ResolvedPackage> = outcome
            .chosen
            .iter()
            .filter(|(name, _)| !root_ids.contains(name))
            .map(|(name, id)| ResolvedPackage {
                id: id.clone(),
                checksum: outcome.checksums.get(name).cloned().flatten(),
                features: outcome.enabled.get(name).cloned().unwrap_or_default(),
                activated: outcome.activated.get(name).cloned().unwrap_or_default(),
                dependencies: outcome.edges_for(name),
            })
            .collect();
        packages.sort_by(|left, right| left.id.cmp(&right.id));
        let resolved_roots = roots
            .iter()
            .map(|root| ResolvedRoot {
                id: root.summary.id.clone(),
                features: outcome
                    .enabled
                    .get(&root.summary.id.name)
                    .cloned()
                    .unwrap_or_default(),
                dependencies: outcome.edges_for(&root.summary.id.name),
            })
            .collect();
        let mut warnings = Vec::new();
        if let Some(lock) = lock {
            let present: BTreeSet<&PackageId> =
                packages.iter().map(|package| &package.id).collect();
            for package in &lock.packages {
                if !present.contains(&package.id) {
                    warnings.push(ResolveWarning::UnusedLockEntry {
                        package: package.id.render_edge(),
                    });
                }
            }
        }
        let graph = ResolveGraph {
            roots: resolved_roots,
            packages,
            warnings,
            fingerprint: Fingerprint::of(b""),
        };
        let fingerprint = Fingerprint::of(graph.lockfile().render().as_bytes());
        ResolveGraph {
            fingerprint,
            ..graph
        }
    }
}

/// The result one pass derived.
struct PassOutcome {
    chosen: BTreeMap<PackageName, PackageId>,
    checksums: BTreeMap<PackageName, Option<Checksum>>,
    enabled: BTreeMap<PackageName, BTreeSet<String>>,
    activated: BTreeMap<PackageName, BTreeSet<String>>,
    edges: BTreeMap<PackageName, Vec<DependencyRequest>>,
    pins: BTreeMap<PackageName, PackageId>,
}

impl PassOutcome {
    /// One package's active edge requests mapped onto the chosen targets.
    fn edges_for(&self, name: &PackageName) -> Vec<ResolvedDependency> {
        let mut edges: Vec<ResolvedDependency> = self
            .edges
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|request| {
                self.chosen
                    .get(&request.package)
                    .map(|target| ResolvedDependency {
                        name: request.name.clone(),
                        target: target.clone(),
                        kind: request.kind,
                    })
            })
            .collect();
        edges.sort_by(|left, right| left.name.cmp(&right.name));
        edges
    }
}

/// Features requested of one package, before expansion.
#[derive(Debug, Clone, Default)]
struct Requested {
    /// Features enabled directly (a selected feature or a dependency edge's list).
    selected: BTreeSet<String>,
    /// Features forwarded as `<dependency>/<feature>`.
    forwarded: BTreeSet<String>,
    /// Whether the package's own `default` list applies.
    defaults: bool,
}

/// The result of expanding one package's feature graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Expansion {
    /// The closed local feature set (without the reserved `default`).
    enabled: BTreeSet<String>,
    /// The labels of optional dependencies this expansion activates.
    activated: BTreeSet<String>,
    /// `<dependency>/<feature>` forwards contributed by the enabled features, by edge label.
    routes: BTreeMap<PackageName, BTreeSet<String>>,
}

/// One deterministic walk over the roots.
struct Pass<'a, P: Provider> {
    provider: &'a mut P,
    roots: &'a [RootRequest],
    locked: &'a BTreeMap<PackageName, LockedPackage>,
    pins: &'a BTreeMap<PackageName, PackageId>,
    /// Candidate lists memoized across passes; keyed by `(source, package)`.
    candidates: &'a mut BTreeMap<(SourceRequest, PackageName), Vec<Candidate>>,
    /// Summaries memoized across passes; keyed by exact id.
    summaries: &'a mut BTreeMap<PackageId, Summary>,
    root_ids: BTreeMap<PackageName, PackageId>,
    representatives: BTreeMap<PackageName, DependencyRequest>,
    sources: BTreeMap<PackageName, SourceRequest>,
    requirements: BTreeMap<PackageName, Vec<RequirementOrigin>>,
    requested: BTreeMap<PackageName, Requested>,
    chosen: BTreeMap<PackageName, PackageId>,
    checksums: BTreeMap<PackageName, Option<Checksum>>,
    expanded: BTreeMap<PackageName, Expansion>,
    edges: BTreeMap<PackageName, Vec<DependencyRequest>>,
    package_queue: VecDeque<PackageName>,
    feature_queue: VecDeque<PackageName>,
}

impl<'a, P: Provider> Pass<'a, P> {
    const fn new(
        provider: &'a mut P,
        roots: &'a [RootRequest],
        locked: &'a BTreeMap<PackageName, LockedPackage>,
        pins: &'a BTreeMap<PackageName, PackageId>,
        candidates: &'a mut BTreeMap<(SourceRequest, PackageName), Vec<Candidate>>,
        summaries: &'a mut BTreeMap<PackageId, Summary>,
    ) -> Self {
        Self {
            provider,
            roots,
            locked,
            pins,
            candidates,
            summaries,
            root_ids: BTreeMap::new(),
            representatives: BTreeMap::new(),
            sources: BTreeMap::new(),
            requirements: BTreeMap::new(),
            requested: BTreeMap::new(),
            chosen: BTreeMap::new(),
            checksums: BTreeMap::new(),
            expanded: BTreeMap::new(),
            edges: BTreeMap::new(),
            package_queue: VecDeque::new(),
            feature_queue: VecDeque::new(),
        }
    }

    async fn run(mut self) -> Result<PassOutcome, ResolveError<P::Error>> {
        self.seed_roots()?;
        while !self.package_queue.is_empty() || !self.feature_queue.is_empty() {
            // Packages first: their candidate lists and summaries are fetched as one batch, so
            // independent packages overlap inside a single provider call. Feature expansion can
            // enqueue more packages (optional activation, forwards), so the loop checks again.
            if !self.package_queue.is_empty() {
                self.process_packages().await?;
            } else if let Some(name) = self.feature_queue.pop_front() {
                self.expand_features(&name)?;
            }
        }
        let pins = self.validate()?;
        Ok(PassOutcome {
            chosen: self.chosen,
            checksums: self.checksums,
            enabled: self
                .expanded
                .iter()
                .map(|(name, expansion)| (name.clone(), expansion.enabled.clone()))
                .collect(),
            activated: self
                .expanded
                .iter()
                .map(|(name, expansion)| (name.clone(), expansion.activated.clone()))
                .collect(),
            edges: self.edges,
            pins,
        })
    }

    /// Seed the roots and their active edges.
    fn seed_roots(&mut self) -> Result<(), ResolveError<P::Error>> {
        for root in self.roots {
            let name = root.summary.id.name.clone();
            let id = root.summary.id.clone();
            self.root_ids.insert(name.clone(), id.clone());
            self.summaries.insert(id, root.summary.clone());
            self.chosen.insert(name.clone(), root.summary.id.clone());
            self.checksums.insert(name.clone(), None);
            let mut requested = Requested {
                defaults: root.default_features,
                ..Requested::default()
            };
            for entry in &root.features {
                match entry.split_once('/') {
                    Some((label, feature)) => {
                        self.forward(&root.summary, label, feature, &mut requested);
                    }
                    None => {
                        requested.selected.insert(entry.clone());
                    }
                }
            }
            self.requested.insert(name.clone(), requested);
            self.feature_queue.push_back(name.clone());
            let deps: Vec<DependencyRequest> = root
                .summary
                .dependencies
                .iter()
                // Optional entries wait for feature expansion: `expand_features` activates them
                // through `dep:`/implicit features and enqueues them then. Seeding them here
                // would resolve an entry no selection turned on.
                .filter(|dependency| {
                    (dependency.kind == DependencyKind::Normal || root.include_dev)
                        && !dependency.optional
                })
                .cloned()
                .collect();
            for dependency in deps {
                self.enqueue_dependency(&name, &dependency)?;
            }
        }
        // A forward may have named a package whose `requested` entry did not exist yet; make
        // sure every touched package is expanded once its summary arrives.
        let pending: Vec<PackageName> = self.requested.keys().cloned().collect();
        for name in pending {
            if self.chosen.contains_key(&name) {
                self.feature_queue.push_back(name);
            }
        }
        Ok(())
    }

    /// The summary chosen for `name`, from the pass's or a previous pass's fetch.
    fn summary(&self, name: &PackageName) -> Option<&Summary> {
        self.summaries.get(self.chosen.get(name)?)
    }

    /// Route one `<dependency>/<feature>` forward into the target's requested set.
    fn forward(&mut self, summary: &Summary, label: &str, feature: &str, _local: &mut Requested) {
        if let Some(dependency) = summary.dependency(label) {
            self.requested
                .entry(dependency.package.clone())
                .or_default()
                .forwarded
                .insert(feature.to_owned());
        }
    }

    /// Record one active edge and queue its target.
    fn enqueue_dependency(
        &mut self,
        from: &PackageName,
        request: &DependencyRequest,
    ) -> Result<(), ResolveError<P::Error>> {
        if let Some(existing) = self.sources.get(&request.package) {
            if existing != &request.source {
                return Err(ResolveError::ConflictingSources {
                    name: request.package.clone(),
                    first: Box::new(existing.clone()),
                    second: Box::new(request.source.clone()),
                });
            }
        } else {
            self.sources
                .insert(request.package.clone(), request.source.clone());
            self.representatives
                .insert(request.package.clone(), request.clone());
        }
        self.requirements
            .entry(request.package.clone())
            .or_default()
            .push(RequirementOrigin {
                requirement: request.version.clone(),
                requester: from.to_string(),
            });
        let requested = self.requested.entry(request.package.clone()).or_default();
        requested.selected.extend(request.features.iter().cloned());
        requested.defaults |= request.default_features;
        self.edges
            .entry(from.clone())
            .or_default()
            .push(request.clone());
        self.feature_queue.push_back(request.package.clone());
        self.package_queue.push_back(request.package.clone());
        Ok(())
    }

    /// Drain the queued packages: batch their candidate lists, batch their summaries, then
    /// choose versions in deterministic order and enqueue what their summaries revealed.
    async fn process_packages(&mut self) -> Result<(), ResolveError<P::Error>> {
        // 1. Classify the queue: roots have their requirements checked, every other name is a
        //    choice candidate, and names whose candidate list is not cached form the fetch batch.
        let mut to_choose: Vec<PackageName> = Vec::new();
        let mut misses: Vec<PackageName> = Vec::new();
        while let Some(name) = self.package_queue.pop_front() {
            if let Some(root_id) = self.root_ids.get(&name) {
                let requirements = self
                    .requirements
                    .get(&name)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                if !requirements
                    .iter()
                    .all(|origin| origin.requirement.matches(&root_id.version))
                {
                    return Err(ResolveError::NoMatchingVersion {
                        name,
                        requirements: requirements.to_vec(),
                    });
                }
                continue;
            }
            if self.chosen.contains_key(&name) {
                continue;
            }
            let Some(request) = self.representatives.get(&name) else {
                continue;
            };
            if !self
                .candidates
                .contains_key(&(request.source.clone(), name.clone()))
                && !misses.contains(&name)
            {
                misses.push(name.clone());
            }
            if !to_choose.contains(&name) {
                to_choose.push(name);
            }
        }

        // 2. One provider call for every missing candidate list. A provider with a fetcher
        //    overlaps them; the resolver zips the results back in this order and never observes
        //    a completion order.
        if !misses.is_empty() {
            let requests: Vec<CandidateRequest> = misses
                .iter()
                .map(|name| CandidateRequest {
                    request: self.representatives[name].clone(),
                    locked: self.locked.get(name).cloned(),
                })
                .collect();
            let results = self.provider.candidates_batch(requests).await;
            for (name, result) in misses.iter().zip(results) {
                let request = &self.representatives[name];
                let fetched = result.map_err(|source| ResolveError::Provider {
                    context: format!("listing candidates for `{}`", request.package),
                    source,
                })?;
                self.candidates
                    .insert((request.source.clone(), name.clone()), fetched);
            }
        }

        // 3. Choose sequentially: a later choice may see requirements an earlier edge added, and
        //    that order is what keeps a pass deterministic under any provider concurrency.
        let mut chosen_ids: Vec<PackageId> = Vec::new();
        for name in &to_choose {
            if self.chosen.contains_key(name) {
                continue;
            }
            let request = self.representatives[name].clone();
            let candidates = self
                .candidates
                .get(&(request.source.clone(), name.clone()))
                .cloned()
                .unwrap_or_default();
            let requirements = self
                .requirements
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let pin = self.pins.get(name);
            let pin_checksum = self
                .locked
                .get(name)
                .and_then(|package| package.checksum.as_ref());
            let Some((id, checksum)) = Self::select(
                &candidates,
                requirements,
                pin,
                pin_checksum,
                &request.source,
            ) else {
                return Err(ResolveError::NoMatchingVersion {
                    name: name.clone(),
                    requirements: requirements.to_vec(),
                });
            };
            chosen_ids.push(id.clone());
            self.chosen.insert(name.clone(), id);
            self.checksums.insert(name.clone(), checksum);
        }

        // 4. One provider call for every missing summary.
        let missing: Vec<PackageId> = chosen_ids
            .iter()
            .filter(|id| !self.summaries.contains_key(*id))
            .cloned()
            .collect();
        if !missing.is_empty() {
            let results = self.provider.summaries_batch(&missing).await;
            for (id, result) in missing.iter().zip(results) {
                let summary = result.map_err(|source| ResolveError::Provider {
                    context: format!("reading the summary of `{id}`"),
                    source,
                })?;
                if summary.id != *id {
                    return Err(ResolveError::SummaryMismatch {
                        requested: Box::new(id.clone()),
                        found: Box::new(summary.id),
                    });
                }
                self.summaries.insert(id.clone(), summary);
            }
        }

        // 5. Feature expansion and edge discovery per chosen package.
        for name in &to_choose {
            if !self.chosen.contains_key(name) {
                continue;
            }
            self.feature_queue.push_back(name.clone());
            let dependencies: Vec<DependencyRequest> = self
                .summary(name)
                .map(|summary| {
                    summary
                        .dependencies
                        .iter()
                        .filter(|dependency| dependency.kind == DependencyKind::Normal)
                        .filter(|dependency| !dependency.optional)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            for dependency in dependencies {
                self.enqueue_dependency(name, &dependency)?;
            }
        }
        Ok(())
    }

    /// Expand one package's feature graph and act on what it reveals.
    fn expand_features(&mut self, name: &PackageName) -> Result<(), ResolveError<P::Error>> {
        let Some(summary) = self.summary(name).cloned() else {
            return Ok(());
        };
        let Some(requested) = self.requested.get(name).cloned() else {
            return Ok(());
        };
        let mut seed = requested.selected.clone();
        seed.extend(requested.forwarded.iter().cloned());
        let expansion = Self::expand(&summary, &seed, requested.defaults);
        let previous = self.expanded.get(name).cloned();
        if previous.as_ref() == Some(&expansion) {
            return Ok(());
        }
        let newly_activated: Vec<String> = expansion
            .activated
            .iter()
            .filter(|label| {
                previous
                    .as_ref()
                    .is_none_or(|previous| !previous.activated.contains(*label))
            })
            .cloned()
            .collect();
        self.expanded.insert(name.clone(), expansion.clone());
        // Merge the forwards this expansion contributed, by edge label → target package.
        for (label, features) in &expansion.routes {
            let Some(target) = summary.dependency(label.as_str()) else {
                continue;
            };
            let entry = self.requested.entry(target.package.clone()).or_default();
            let before = entry.forwarded.len();
            entry.forwarded.extend(features.iter().cloned());
            if entry.forwarded.len() > before {
                self.feature_queue.push_back(target.package.clone());
            }
        }
        // Optional dependencies an activation turned on become real edges.
        for label in newly_activated {
            if let Some(dependency) = summary
                .dependencies
                .iter()
                .find(|dependency| dependency.name.as_str() == label)
                .cloned()
            {
                self.enqueue_dependency(name, &dependency)?;
            }
        }
        Ok(())
    }

    /// Close a package's feature graph: local closure plus implicit optional features.
    fn expand(summary: &Summary, seed: &BTreeSet<String>, defaults: bool) -> Expansion {
        let mut enabled = BTreeSet::new();
        let mut activated = BTreeSet::new();
        let mut routes: BTreeMap<PackageName, BTreeSet<String>> = BTreeMap::new();
        let mut pending: Vec<String> = seed.iter().cloned().collect();
        if defaults {
            pending.push("default".to_owned());
        }
        while let Some(feature) = pending.pop() {
            if !enabled.insert(feature.clone()) {
                continue;
            }
            for value in summary.feature_values(&feature) {
                match value {
                    FeatureValue::Feature(inner) => pending.push(inner.clone()),
                    FeatureValue::Dependency(dependency) => {
                        activated.insert(dependency.as_str().to_owned());
                    }
                    FeatureValue::DependencyFeature {
                        dependency,
                        feature,
                    } => {
                        routes
                            .entry(dependency.clone())
                            .or_default()
                            .insert(feature.clone());
                    }
                }
            }
        }
        enabled.remove("default");
        // An optional dependency no `dep:` mentions declares a feature of its own name; enabling
        // that name activates it. This is the same rule `jals-config` applies.
        for dependency in &summary.dependencies {
            if !dependency.optional {
                continue;
            }
            let label = dependency.name.as_str();
            let explicit = summary.features.values().flatten().any(
                |value| matches!(value, FeatureValue::Dependency(dep) if dep.as_str() == label),
            );
            if !explicit && enabled.contains(label) {
                activated.insert(label.to_owned());
            }
        }
        Expansion {
            enabled,
            activated,
            routes,
        }
    }

    /// Pick a candidate: the pin when it satisfies everything, otherwise the greatest that does.
    fn select(
        candidates: &[Candidate],
        requirements: &[RequirementOrigin],
        pin: Option<&PackageId>,
        pin_checksum: Option<&Checksum>,
        source: &SourceRequest,
    ) -> Option<(PackageId, Option<Checksum>)> {
        if let Some(pin) = pin
            && pin.source.satisfies(source)
            && requirements
                .iter()
                .all(|origin| origin.requirement.matches(&pin.version))
        {
            if let Some(candidate) = candidates.iter().find(|candidate| candidate.id == *pin) {
                return Some((pin.clone(), candidate.checksum.clone()));
            }
            return Some((pin.clone(), pin_checksum.cloned()));
        }
        candidates
            .iter()
            .filter(|candidate| {
                requirements
                    .iter()
                    .all(|origin| origin.requirement.matches(&candidate.id.version))
            })
            .max_by(|left, right| {
                left.id
                    .version
                    .cmp(&right.id.version)
                    .then_with(|| left.id.source.render().cmp(&right.id.source.render()))
            })
            .map(|candidate| (candidate.id.clone(), candidate.checksum.clone()))
    }

    /// Re-validate every chosen version against the pass's complete requirement set.
    fn validate(&self) -> Result<BTreeMap<PackageName, PackageId>, ResolveError<P::Error>> {
        let mut pins = BTreeMap::new();
        for (name, id) in &self.chosen {
            let requirements = self
                .requirements
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if requirements
                .iter()
                .all(|origin| origin.requirement.matches(&id.version))
            {
                pins.insert(name.clone(), id.clone());
                continue;
            }
            let Some(request) = self.representatives.get(name) else {
                pins.insert(name.clone(), id.clone());
                continue;
            };
            let key = (request.source.clone(), name.clone());
            let Some(candidates) = self.candidates.get(&key) else {
                return Err(ResolveError::NoMatchingVersion {
                    name: name.clone(),
                    requirements: requirements.to_vec(),
                });
            };
            let Some((selected, _)) =
                Self::select(candidates, requirements, None, None, &request.source)
            else {
                return Err(ResolveError::NoMatchingVersion {
                    name: name.clone(),
                    requirements: requirements.to_vec(),
                });
            };
            pins.insert(name.clone(), selected);
        }
        Ok(pins)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{DirectKind, RegistryId, SourceId};
    use crate::version::{Version, VersionReq};
    use alloc::vec;
    use core::cell::{Cell, RefCell};

    fn pkg(text: &str) -> PackageName {
        PackageName::new(text).unwrap()
    }

    fn ver(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn registry() -> SourceId {
        SourceId::Registry(RegistryId::default())
    }

    fn pkg_id(name: &str, version: &str) -> PackageId {
        PackageId::new(pkg(name), ver(version), registry())
    }

    fn request(name: &str, requirement: &str) -> DependencyRequest {
        DependencyRequest {
            name: pkg(name),
            package: pkg(name),
            source: SourceRequest::Registry {
                registry: RegistryId::default(),
            },
            version: VersionReq::parse(requirement).unwrap(),
            features: BTreeSet::new(),
            default_features: true,
            optional: false,
            kind: DependencyKind::Normal,
        }
    }

    /// A summary with no features and the given edges.
    fn summary(name: &str, version_text: &str, dependencies: Vec<DependencyRequest>) -> Summary {
        Summary {
            id: pkg_id(name, version_text),
            dependencies,
            features: BTreeMap::new(),
        }
    }

    /// A summary carrying a feature table.
    fn featured(
        name: &str,
        version_text: &str,
        dependencies: Vec<DependencyRequest>,
        features: &[(&str, Vec<FeatureValue>)],
    ) -> Summary {
        Summary {
            id: pkg_id(name, version_text),
            dependencies,
            features: features
                .iter()
                .map(|(key, values)| ((*key).to_owned(), values.clone()))
                .collect(),
        }
    }

    /// An in-memory provider: all packages are registry packages, versions by name.
    ///
    /// Records the batch sizes and single-call counts the resolver drove, so the tests can
    /// assert *how* work was requested without a network: candidate lists for independent
    /// packages arrive in one `candidates_batch` call, and memoized answers are never re-asked.
    struct MemoryProvider {
        packages: BTreeMap<PackageName, Vec<Summary>>,
        candidate_calls: Cell<usize>,
        summary_calls: Cell<usize>,
        candidate_batches: RefCell<Vec<usize>>,
        summary_batches: RefCell<Vec<usize>>,
    }

    impl MemoryProvider {
        fn new(summaries: Vec<Summary>) -> Self {
            let mut packages: BTreeMap<PackageName, Vec<Summary>> = BTreeMap::new();
            for summary in summaries {
                packages
                    .entry(summary.id.name.clone())
                    .or_default()
                    .push(summary);
            }
            for versions in packages.values_mut() {
                versions.sort_by(|left, right| left.id.version.cmp(&right.id.version));
            }
            Self {
                packages,
                candidate_calls: Cell::new(0),
                summary_calls: Cell::new(0),
                candidate_batches: RefCell::new(Vec::new()),
                summary_batches: RefCell::new(Vec::new()),
            }
        }

        fn candidate_calls(&self) -> usize {
            self.candidate_calls.get()
        }

        fn summary_calls(&self) -> usize {
            self.summary_calls.get()
        }
    }

    // A trait implementation must match a trait's async signatures even when a test body never
    // awaits; the futures here are driven inline by the test executor, so the lint is noise.
    #[allow(clippy::unused_async_trait_impl)]
    impl Provider for MemoryProvider {
        type Error = core::convert::Infallible;

        async fn candidates(
            &mut self,
            request: &DependencyRequest,
            locked: Option<&LockedPackage>,
        ) -> Result<Vec<Candidate>, Self::Error> {
            self.candidate_calls.set(self.candidate_calls.get() + 1);
            let mut candidates: Vec<Candidate> = self
                .packages
                .get(&request.package)
                .map(|versions| {
                    versions
                        .iter()
                        .map(|summary| Candidate::new(summary.id.clone()))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(locked) = locked
                && !candidates.iter().any(|candidate| candidate.id == locked.id)
            {
                candidates.push(Candidate::new(locked.id.clone()));
            }
            Ok(candidates)
        }

        async fn summary(&mut self, id: &PackageId) -> Result<Summary, Self::Error> {
            self.summary_calls.set(self.summary_calls.get() + 1);
            Ok(self
                .packages
                .get(&id.name)
                .and_then(|versions| versions.iter().find(|summary| summary.id == *id))
                .cloned()
                .expect("the test provider only asks for known summaries"))
        }

        async fn candidates_batch(
            &mut self,
            requests: Vec<CandidateRequest>,
        ) -> Vec<Result<Vec<Candidate>, Self::Error>> {
            self.candidate_batches.borrow_mut().push(requests.len());
            let mut results = Vec::with_capacity(requests.len());
            for request in &requests {
                results.push(
                    self.candidates(&request.request, request.locked.as_ref())
                        .await,
                );
            }
            results
        }

        async fn summaries_batch(
            &mut self,
            ids: &[PackageId],
        ) -> Vec<Result<Summary, Self::Error>> {
            self.summary_batches.borrow_mut().push(ids.len());
            let mut results = Vec::with_capacity(ids.len());
            for id in ids {
                results.push(self.summary(id).await);
            }
            results
        }
    }

    fn root(summary: Summary) -> RootRequest {
        RootRequest::new(summary)
    }

    fn run(provider: &mut MemoryProvider, roots: &[RootRequest]) -> ResolveGraph {
        jals_exec::block_on_inline(Resolver::new(provider).resolve(roots, None))
            .expect("resolution succeeds")
    }

    #[test]
    fn resolves_a_transitive_graph_and_picks_the_greatest_version() {
        let mut provider = MemoryProvider::new(vec![
            summary("a", "1.0.0", vec![request("b", "1")]),
            summary("b", "1.0.0", vec![]),
            summary("b", "1.5.0", vec![]),
            summary("b", "2.0.0", vec![]),
        ]);
        let roots = [root(summary("app", "0.0.0", vec![request("a", "1")]))];
        let graph = run(&mut provider, &roots);
        assert_eq!(graph.packages.len(), 2);
        assert_eq!(graph.package(&pkg("a")).unwrap().id.version, ver("1.0.0"));
        assert_eq!(graph.package(&pkg("b")).unwrap().id.version, ver("1.5.0"));
    }

    #[test]
    fn independent_packages_are_batched_into_one_provider_call() {
        let mut provider = MemoryProvider::new(vec![
            summary("a", "1.0.0", vec![]),
            summary("b", "1.0.0", vec![]),
            summary("c", "1.0.0", vec![]),
        ]);
        let roots = [root(summary(
            "app",
            "0.0.0",
            vec![request("a", "1"), request("b", "1"), request("c", "1")],
        ))];
        let graph = run(&mut provider, &roots);
        assert_eq!(graph.packages.len(), 3);
        assert_eq!(provider.candidate_batches.borrow().as_slice(), &[3]);
        assert_eq!(provider.summary_batches.borrow().as_slice(), &[3]);
    }

    #[test]
    fn provider_answers_are_memoized_across_passes() {
        // Pass 1 chooses `c = 1.5.0` before `a`'s edge narrows it to `=1.0.0`; pass 2 re-picks
        // from the same candidate list and only the newly selected summary is fetched.
        let mut provider = MemoryProvider::new(vec![
            summary("c", "1.0.0", vec![]),
            summary("c", "1.5.0", vec![]),
            summary("a", "1.0.0", vec![request("c", "=1.0.0")]),
        ]);
        let roots = [root(summary(
            "app",
            "0.0.0",
            vec![request("c", "1"), request("a", "1")],
        ))];
        let graph = run(&mut provider, &roots);
        assert_eq!(graph.package(&pkg("c")).unwrap().id.version, ver("1.0.0"));
        assert_eq!(provider.candidate_calls(), 2);
        assert_eq!(provider.summary_calls(), 3);
    }

    #[test]
    fn a_diamond_is_one_package_with_both_edges() {
        let mut provider = MemoryProvider::new(vec![
            summary("a", "1.0.0", vec![request("c", "1")]),
            summary("b", "1.0.0", vec![request("c", "1")]),
            summary("c", "1.2.0", vec![]),
        ]);
        let roots = [root(summary(
            "app",
            "0.0.0",
            vec![request("a", "1"), request("b", "1")],
        ))];
        let graph = run(&mut provider, &roots);
        assert_eq!(graph.packages.len(), 3);
        let c = graph.package(&pkg("c")).unwrap();
        assert_eq!(c.dependencies.len(), 0);
    }

    #[test]
    fn a_cycle_is_an_edge_not_recursion() {
        let mut provider = MemoryProvider::new(vec![
            summary("a", "1.0.0", vec![request("b", "1")]),
            summary("b", "1.0.0", vec![request("a", "1")]),
        ]);
        let roots = [root(summary("app", "0.0.0", vec![request("a", "1")]))];
        let graph = run(&mut provider, &roots);
        assert_eq!(graph.packages.len(), 2);
        let b = graph.package(&pkg("b")).unwrap();
        assert_eq!(b.dependencies[0].target.name, pkg("a"));
    }

    #[test]
    fn requirements_intersect_and_conflicts_are_reported() {
        let mut provider = MemoryProvider::new(vec![
            summary("a", "1.0.0", vec![]),
            summary("a", "1.5.0", vec![]),
        ]);
        let roots = [root(summary(
            "app",
            "0.0.0",
            vec![request("a", "1"), request("a", "=1.0.0")],
        ))];
        let graph = run(&mut provider, &roots);
        assert_eq!(graph.package(&pkg("a")).unwrap().id.version, ver("1.0.0"));

        let roots = [root(summary(
            "app",
            "0.0.0",
            vec![request("a", "=1.0.0"), request("a", "=1.5.0")],
        ))];
        let error = jals_exec::block_on_inline(Resolver::new(&mut provider).resolve(&roots, None))
            .expect_err("conflicting requirements fail");
        assert!(matches!(error, ResolveError::NoMatchingVersion { .. }));
    }

    #[test]
    fn one_name_from_two_sources_is_rejected() {
        let mut provider = MemoryProvider::new(vec![summary("a", "1.0.0", vec![])]);
        let mut path_request = request("a", "1");
        path_request.source = SourceRequest::Path {
            location: "../a".to_owned(),
            dir: None,
        };
        let roots = [
            root(summary("app", "0.0.0", vec![request("a", "1")])),
            root(summary("other", "0.0.0", vec![path_request])),
        ];
        let error = jals_exec::block_on_inline(Resolver::new(&mut provider).resolve(&roots, None))
            .expect_err("two sources for one name fail");
        assert!(matches!(error, ResolveError::ConflictingSources { .. }));
    }

    #[test]
    fn a_feature_forwards_to_a_dependency_only_when_enabled() {
        let summaries = vec![
            featured(
                "a",
                "1.0.0",
                vec![request("b", "1")],
                &[(
                    "client",
                    vec![FeatureValue::DependencyFeature {
                        dependency: pkg("b"),
                        feature: "fast".to_owned(),
                    }],
                )],
            ),
            featured("b", "1.0.0", vec![], &[("fast", vec![]), ("slow", vec![])]),
        ];
        let mut provider = MemoryProvider::new(summaries.clone());
        let roots = [root(summary("app", "0.0.0", vec![request("a", "1")]))];
        let graph = run(&mut provider, &roots);
        assert!(!graph.package(&pkg("b")).unwrap().features.contains("fast"));

        let mut provider = MemoryProvider::new(summaries);
        let mut enabled = request("a", "1");
        enabled.features.insert("client".to_owned());
        let roots = [root(summary("app", "0.0.0", vec![enabled]))];
        let graph = run(&mut provider, &roots);
        assert!(graph.package(&pkg("b")).unwrap().features.contains("fast"));
    }

    #[test]
    fn a_route_from_the_default_feature_still_forwards() {
        let mut provider = MemoryProvider::new(vec![
            featured(
                "a",
                "1.0.0",
                vec![request("b", "1")],
                &[(
                    "default",
                    vec![FeatureValue::DependencyFeature {
                        dependency: pkg("b"),
                        feature: "fast".to_owned(),
                    }],
                )],
            ),
            featured("b", "1.0.0", vec![], &[("fast", vec![])]),
        ]);
        let roots = [root(summary("app", "0.0.0", vec![request("a", "1")]))];
        let graph = run(&mut provider, &roots);
        assert!(graph.package(&pkg("b")).unwrap().features.contains("fast"));
    }

    #[test]
    fn features_unify_additively_across_edges() {
        let mut provider = MemoryProvider::new(vec![
            summary("a", "1.0.0", vec![]),
            summary("b", "1.0.0", vec![request("a", "1")]),
        ]);
        let mut with_x = request("a", "1");
        with_x.features.insert("x".to_owned());
        let mut with_y = request("a", "1");
        with_y.features.insert("y".to_owned());
        let roots = [
            root(summary("app", "0.0.0", vec![with_x])),
            root(summary("other", "0.0.0", vec![request("b", "1")])),
        ];
        // b's edge adds no features; the two roots' edges to a unify.
        provider.packages.get_mut(&pkg("b")).unwrap()[0].dependencies[0]
            .features
            .insert("y".to_owned());
        let graph = run(&mut provider, &roots);
        let a = graph.package(&pkg("a")).unwrap();
        assert!(a.features.contains("x"), "{:?}", a.features);
        assert!(a.features.contains("y"), "{:?}", a.features);
    }

    #[test]
    fn an_optional_dependency_needs_activation() {
        let mut optional = request("b", "1");
        optional.optional = true;
        let summaries = vec![
            featured(
                "a",
                "1.0.0",
                vec![optional],
                &[("on", vec![FeatureValue::Dependency(pkg("b"))])],
            ),
            summary("b", "1.0.0", vec![]),
        ];
        let mut provider = MemoryProvider::new(summaries.clone());
        let roots = [root(summary("app", "0.0.0", vec![request("a", "1")]))];
        let graph = run(&mut provider, &roots);
        assert!(graph.package(&pkg("b")).is_none());

        let mut provider = MemoryProvider::new(summaries);
        let mut enabled = request("a", "1");
        enabled.features.insert("on".to_owned());
        let selection = root(summary("app", "0.0.0", vec![enabled]));
        let graph = run(&mut provider, &[selection]);
        let b = graph.package(&pkg("b")).expect("b is activated");
        assert_eq!(b.id.version, ver("1.0.0"));
        let a = graph.package(&pkg("a")).unwrap();
        assert!(a.activated.contains("b"));
        assert!(a.features.contains("on"));
    }

    #[test]
    fn an_optional_root_dependency_needs_activation_too() {
        let unactivated = || {
            let mut request = request("b", "1");
            request.optional = true;
            request
        };
        let root_summary = || {
            featured(
                "app",
                "0.0.0",
                vec![unactivated()],
                &[("on", vec![FeatureValue::Dependency(pkg("b"))])],
            )
        };
        let b = summary("b", "1.0.0", vec![]);

        let mut provider = MemoryProvider::new(vec![root_summary(), b.clone()]);
        let roots = [root(root_summary())];
        let graph = run(&mut provider, &roots);
        assert!(graph.package(&pkg("b")).is_none());

        let mut provider = MemoryProvider::new(vec![root_summary(), b]);
        let mut selection = root(root_summary());
        selection.features.insert("on".to_owned());
        let graph = run(&mut provider, &[selection]);
        assert!(graph.package(&pkg("b")).is_some());
    }

    #[test]
    fn an_implicit_optional_feature_activates_by_its_own_name() {
        let mut optional = request("b", "1");
        optional.optional = true;
        let mut provider = MemoryProvider::new(vec![
            summary("a", "1.0.0", vec![optional]),
            summary("b", "1.0.0", vec![]),
        ]);
        let mut enabled = request("a", "1");
        enabled.features.insert("b".to_owned());
        let selection = root(summary("app", "0.0.0", vec![enabled]));
        let graph = run(&mut provider, &[selection]);
        assert!(graph.package(&pkg("b")).is_some());
    }

    #[test]
    fn default_features_unify_with_logical_or() {
        let mut default = request("b", "1");
        default.default_features = true;
        let mut no_default = request("b", "1");
        no_default.default_features = false;
        let b = featured(
            "b",
            "1.0.0",
            vec![],
            &[("default", vec![FeatureValue::Feature("base".to_owned())])],
        );
        let mut provider =
            MemoryProvider::new(vec![summary("a", "1.0.0", vec![no_default]), b.clone(), b]);
        // One root asks without defaults; the other asks with. The union wins.
        let roots = [root(summary(
            "app",
            "0.0.0",
            vec![request("a", "1"), default],
        ))];
        let graph = run(&mut provider, &roots);
        let b = graph.package(&pkg("b")).unwrap();
        assert!(b.features.contains("base"), "{:?}", b.features);
    }

    #[test]
    fn a_lock_pin_wins_over_a_newer_release() {
        let mut provider = MemoryProvider::new(vec![
            summary("a", "1.0.0", vec![]),
            summary("a", "1.5.0", vec![]),
        ]);
        let roots = [root(summary("app", "0.0.0", vec![request("a", "1")]))];
        let mut lock = Lockfile::new();
        lock.packages.push(LockedPackage {
            id: pkg_id("a", "1.0.0"),
            checksum: None,
            dependencies: Vec::new(),
        });
        let graph =
            jals_exec::block_on_inline(Resolver::new(&mut provider).resolve(&roots, Some(&lock)))
                .unwrap();
        assert_eq!(graph.package(&pkg("a")).unwrap().id.version, ver("1.0.0"));
    }

    #[test]
    fn dev_dependencies_resolve_only_from_roots() {
        let mut dev = request("dev", "1");
        dev.kind = DependencyKind::Development;
        let provider_summaries = vec![
            summary("a", "1.0.0", vec![dev]),
            summary("dev", "1.0.0", vec![]),
        ];
        let mut provider = MemoryProvider::new(provider_summaries.clone());
        let roots = [root(summary("app", "0.0.0", vec![request("a", "1")]))];
        let graph = run(&mut provider, &roots);
        assert!(graph.package(&pkg("dev")).is_none());

        let mut provider = MemoryProvider::new(provider_summaries);
        let mut dev_root = request("dev", "1");
        dev_root.kind = DependencyKind::Development;
        let mut selection = root(summary("app", "0.0.0", vec![dev_root]));
        selection.include_dev = true;
        let graph = run(&mut provider, &[selection]);
        assert!(graph.package(&pkg("dev")).is_some());
    }

    #[test]
    fn direct_binaries_resolve_as_packages() {
        let mut provider = MemoryProvider::new(vec![]);
        let mut direct = request("legacy", "0.0.0");
        direct.source = SourceRequest::Direct {
            kind: DirectKind::Jar,
            locator: "libs/legacy.jar".to_owned(),
        };
        direct.version = VersionReq::parse("*").unwrap();
        let roots = [root(summary("app", "0.0.0", vec![direct]))];
        let error = jals_exec::block_on_inline(Resolver::new(&mut provider).resolve(&roots, None))
            .expect_err("the memory provider cannot supply a direct binary");
        assert!(matches!(error, ResolveError::NoMatchingVersion { .. }));
    }

    #[test]
    fn lockfile_round_trips_through_the_graph() {
        let mut provider = MemoryProvider::new(vec![
            summary("a", "1.0.0", vec![request("b", "1")]),
            summary("b", "1.0.0", vec![]),
        ]);
        let roots = [root(summary("app", "0.0.0", vec![request("a", "1")]))];
        let graph = run(&mut provider, &roots);
        let lock = graph.lockfile();
        let parsed = Lockfile::parse(&lock.render()).unwrap();
        assert_eq!(parsed, lock);
        assert_eq!(parsed.packages.len(), 2);
        let fingerprint = graph.fingerprint().as_str().to_owned();
        assert_eq!(fingerprint.len(), 64);
        assert_eq!(fingerprint, graph.fingerprint().as_str());
    }

    #[test]
    fn unused_lock_entries_warn_without_failing() {
        let mut provider = MemoryProvider::new(vec![summary("a", "1.0.0", vec![])]);
        let roots = [root(summary("app", "0.0.0", vec![request("a", "1")]))];
        let mut lock = Lockfile::new();
        lock.packages.push(LockedPackage {
            id: pkg_id("gone", "1.0.0"),
            checksum: None,
            dependencies: Vec::new(),
        });
        let graph =
            jals_exec::block_on_inline(Resolver::new(&mut provider).resolve(&roots, Some(&lock)))
                .unwrap();
        assert_eq!(graph.warnings.len(), 1);
    }
}
