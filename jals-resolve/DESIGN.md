# Dependency resolution — design

This document is the target architecture for `jals.toml` dependency resolution, replacing the
locator-driven walk `jals-project` performs today. It is written as the spec the implementation
lands against; each section names the crate that owns it and how it is reached from today's code.

The one-sentence model: **dependencies are resolved into a package graph first, and everything
else — acquisition, node identity, the classpath, cache keys — consumes that graph instead of
re-deriving it from declarations.**

## 1. Goals

1. Cargo-grade dependency UX: `[workspace]`, version requirements, lockfile, feature selection,
   `default-features`, optional dependencies, rename via `package`, deterministic resolution.
2. One dependency model, no per-form islands. Every entry names a **source**, and the same
   feature/optional/lock/workspace machinery applies to every source:
   - `registry` — Maven coordinates (`group:artifact:version`), default Maven Central;
   - `git` — a repository checked out for `.java` sources;
   - `path` — a local directory project (the Cargo spelling; `dir` remains the within-source
     subdirectory selector);
   - `jar` — a precompiled jar, local or URL;
   - `wasm` — a precompiled WebAssembly module.
3. **Deduplication by package identity**, not by locator string. A diamond is one package; a
   cycle is a graph edge and never infinite recursion; one `group:artifact` resolves to one
   version across the workspace (Maven's single-version classpath reality), chosen to satisfy
   every requirement or reported as a conflict.
4. A generated `jals.lock` pinning versions, git commits, path identities and checksums, with
   `--locked` / `--frozen` and `jals update`.
5. Target-directory integration: the lock and resolved graph are inputs to node identity and
   cache keys, and resolved artifacts materialize under a stable `target/jals/deps` view.
6. Feature correctness under switching: **every** cache key over package-dependent output folds
   the package's unified feature set and the resolver fingerprint, so `--features` toggling can
   never serve stale bytes.

## 2. Where resolution sits

```
jals.toml (+ workspace members)
        │
        ▼
  jals-config        parse + validate; classify dependencies into resolver requests
        │
        ▼
  jals-resolve       PROVIDER-BACKED RESOLUTION  ── pure algorithm, portable
        │             • version requirements, conflict detection, cycles
        │             • feature unification (build/test scopes)
        │             • lockfile read/write model
        ▼
  ResolveGraph + jals.lock
        │
        ├──► jals-project graph: acquisition uses the pin (git commit, path identity,
        │    exact registry jar) and PackageId is the graph node identity
        ├──► jals-classpath: each resolved registry package/transitive POM artifact becomes a
        │    DependencySpec with its lock checksum, or the already-pinned cache artifact
        └──► cache keys: resolver fingerprint + per-package features + handoff versions
```

`jals-resolve` performs no I/O. It is generic over a `Provider`:

- **registry** provider: Maven `maven-metadata.xml` version listing, POM parsing (parents,
  `dependencyManagement`, BOM imports, properties, scopes, exclusions, optional), artifact URLs.
  Lives in `jals-resolve` (pure parsing) plus the fetch glue in `jals-classpath`/`native`.
- **git** provider: resolve a ref to a commit and read `jals.toml` at that commit. Reuses the
  clone/confinement logic in `jals-project`/`native`.
- **path** provider: canonical identity + manifest summary.
- **direct** provider: content digest over the jar/wasm bytes.

## 3. Manifest model (breaking)

### 3.1 `[dependencies]` / `[dev-dependencies]`

Every entry is classified by its **source key**. Common keys: `version`, `features`,
`default-features`, `optional`, `package` (rename). Structural errors (two source keys, none,
a key on the wrong source) stay *parse-time* errors, as today.

```toml
[dependencies]
# Registry. The key is the artifact id and `group` is required, or the key is the full
# `group:artifact` coordinate. `version` accepts Cargo-style requirements and Maven ranges.
commons-lang3 = { group = "org.apache.commons", version = "3.17.0" }
"com.google.guava:guava" = "33.4.0-jre"
internal = { group = "com.example", version = "1.2", registry = "internal" }

# Git: version is an optional consistency check.
sdk = { git = "https://github.com/example/sdk", tag = "v1.2.3", features = ["client"] }

# Path: the Cargo spelling. `dir` is the source root inside the project, as it is for git.
sibling = { path = "../sibling" }

# Direct binaries: no version, no features — they run no build script and are not packages.
legacy = { jar = "libs/legacy.jar", sources = "libs/legacy-sources.jar", remap = "mojmap" }
host = { wasm = "libs/host.wasm", foreign = true }

# Workspace inheritance.
guava = { workspace = true, features = ["listenable-future"] }
```

`[registries]` declares named Maven repositories; `maven-central` is implicit.

```toml
[registries]
internal = { url = "https://nexus.example/repository/maven-public" }
```

### 3.2 Version requirements

- A bare version is a **caret requirement** (`2.0.16` = `>=2.0.16, <3.0.0`; `0.2.3` = `<0.3.0`;
  `0.0.3` = `<0.0.4`) — Cargo's rule, kept even though the registry is Maven, because the
  manifest syntax is Cargo's.
- `=1.2.3` exact, `~1.2.3` tilde, `1.2.*` wildcard, `*` any.
- Maven range syntax is accepted verbatim and uses Maven semantics: `[1.0]`, `[1.0,2.0)`,
  `(,1.0]`, `[1.5,)`, `(1.0,2.0)`, and unions `[1.0,2.0),[3.0,)`.

### 3.3 `[workspace]`

```toml
[workspace]
members = ["app", "libs/*"]
exclude = ["libs/legacy"]
default-members = ["app"]

[workspace.package]
version = "1.2.0"

[workspace.dependencies]
guava = { group = "com.google.guava", version = "33.4.0-jre" }
```

- `[workspace.package]` values are inherited by members with `<field>.workspace = true`.
- `[workspace.dependencies]` specs are inherited with `{ workspace = true, ... }`; features merge
  additively and `optional`/`default-features` may be overridden only where Cargo allows it.
- One lockfile and one `target/` at the workspace root. Member-local `target/` directories are
  ignored.
- A virtual manifest (no `[package]`) is allowed at the root.

## 4. Resolution algorithm (`jals-resolve::Resolver`)

Input: one request per workspace member (its dependency tables, selected features, scope) plus
the previous lockfile. Output: `ResolveGraph` + warnings.

1. **Package identity** is `(name, version, source)`; `name` is `group:artifact` for registry
   packages and `[package] name` for source projects. Source identity is fully resolved before
   comparison: a git source is `(url, commit, dir)`, a path source is its canonical location, a
   direct source is `(kind, sha256)`.
2. **One version per name.** Every requirement on a name is collected, and the chosen version
   must satisfy all of them. A locked version satisfying the current requirements wins; otherwise
   the greatest available version satisfying the intersection wins. No version satisfies →
   `ResolveError::NoMatchingVersion` naming every requirement and who wrote it. Two different
   sources for one name → `ResolveError::ConflictingSources`.
3. **Fixpoint.** Resolution recomputes the whole graph until it is stable (versions chosen,
   activated optional edges, unified features, then again). Every step is monotone or pinned, and
   the outer loop is bounded; non-convergence is an error, never a hang.
4. **Cycles are legal graph edges.** Summaries are memoized by package id, so a cycle enqueues
   nothing new. `jals-project` still rejects a cycle through **source** projects at compile time
   (Java cannot compile a cycle), with the existing `GraphError::Cycle` diagnostics.
5. **Scopes.** `Build` resolves `[dependencies]`; `Test` adds `[dev-dependencies]`. Dev
   dependencies are resolved for the selected members only and never transitively, as today.
6. **Determinism.** Every collection that reaches output is ordered: packages by
   `(name, version, source)`, dependency edges by name, features by name. The rendered lockfile
   is byte-stable for the same inputs.

### 4.1 Feature unification

Features are Cargo's: additive per package, unified across every edge that reaches the package,
with `default` on unless every edge set `default-features = false`.

```
enabled(package) = closure of (Σ edge.features ∪ [default])
activated(package) = { optional dep d | `dep:d` in the closure, or d has no `dep:` mention }
```

`dep:<d>` and `<d>/<f>` route exactly as `jals-config` resolves them today; the resolver's
feature graph is the one implementation and `Manifest::expand_build_features` becomes a wrapper
over it. A package's unified set is per **scope**: a feature requested only by a dev-dependency
edges does not leak into the build scope (Cargo resolver v2 behavior, kept because it is what
makes `jals build` and `jals test` cache independently).

## 5. Lockfile (`jals.lock`)

Workspace root, committed, hand-rendered with a deterministic writer (the workspace's `toml`
dependency deliberately has no `display` feature).

```toml
# This file is automatically generated by jals.
version = 1

[[package]]
name = "org.slf4j:slf4j-api"
version = "2.0.16"
source = "registry+maven-central"
checksum = "sha256:..."
dependencies = [
    "org.slf4j:slf4j-api 2.0.16 (registry+maven-central)",
]

[[package]]
name = "example:sdk"
version = "1.2.3"
source = "git+https://github.com/example/sdk#<commit>"
dependencies = []
```

- `source` encoding: `registry+<name>`, `git+<url>#<commit>[?dir=<dir>]`,
  `path+<location>`, `jar+sha256:<hex>`, `wasm+sha256:<hex>`, with `#`/`%` percent-escaped in
  the URL and location.
- `checksum` is optional: Maven publishes checksums beside artifacts, so the registry provider
  fills it when available; otherwise the first verified download records it
  (trust-on-first-use, then pinned).
- `--locked` refuses to change the lock; `--frozen` additionally refuses network access;
  `jals update [<name>]` re-resolves ignoring lock preference.
- The lockfile is read before discovery and its pins flow into the resolver as preferences; a
  lock whose package set no longer covers the manifests is re-resolved and rewritten by any
  command that resolves (Cargo semantics).

## 6. Target directory integration

```
target/
  classes/                      # existing [build] classes-dir
  test-classes/                 # existing [test] classes-dir
  jals/
    cache/                      # existing content-addressed verified artifact cache (kept by clean)
    deps/                       # NEW: stable materialized view of resolved packages
      org/slf4j/slf4j-api/2.0.16/slf4j-api-2.0.16.jar
    build/                      # existing
    remap/                      # existing
```

- The cache stays the source of truth; `deps/` is a **view** materialized with hard links where
  the filesystem allows, a copy otherwise. `jals clean` removes `deps/` and leaves `cache/`.
- Materialized names come from the lock (`group`/`artifact`/`version`), so classpath entries in
  `--dry-run`, `jals tree` and error messages are readable.
- `BuildScriptCacheScope` and the build-script fingerprint already exclude `target/jals`; `deps/`
  inherits that exclusion.

## 7. Cache identity and feature switching

A feature toggle may never be served bytes produced under another selection. The keys that
consume resolved packages or package-derived output fold, in one place
(`ResolvedKey::fold`/the resolver fingerprint), the following:

1. the **resolver fingerprint**: a digest over the sorted locked package set (name, version,
   resolved source, checksum) — not over locator strings;
2. the **unified feature set** of the package being built;
3. the existing transform/tool versions (`JarTransforms::fold`, `TASK_EXECUTION_VERSION`,
   frontend/backend versions).

Audit list (each must be checked when the graph lands):

| Key | Owner | Status |
| --- | --- | --- |
| `BuildTaskState` provenance | `jals-project/src/task.rs` | folds node identity + features; identity moves to `PackageId` |
| `BuildScriptCacheScope` | `jals-build` | node digest; fold resolver fingerprint when identity moves |
| `FrontendOutput` / `BackendOutput` | `jals-frontend` | must fold features (currently not memoized) |
| `PublicationCoverage` | `jals-project/src/graph.rs` | folds plan + classpath; add resolver fingerprint |
| assembly artifact keys | `jals-project/src/assemble.rs` | fold `PackageId` instead of locator identity |
| `JarTransforms` remap/merge | `jals-classpath` | unchanged; already folded by consumers |

## 8. Crate responsibilities

| Crate | Owns |
| --- | --- |
| `jals-resolve` (new, portable) | version/requirement model, `PackageName`/`SourceId`/`PackageId`, `Summary`/`DependencyRequest`, `Provider` trait, resolver algorithm + feature unification, lockfile model/parse/render, Maven metadata/POM parsing (pure), resolver fingerprint |
| `jals-config` | manifest schema (sources, workspace, registries), validation, lowering manifest entries to `DependencyRequest`, workspace/member model (pure part) |
| `jals-classpath` | native providers (HTTP fetch of metadata/POMs/artifacts through `Fetcher`, git acquisition glue), resolution → `DependencySpec`, materializing `target/jals/deps` |
| `jals-project` | consumes `ResolveGraph`: graph nodes are packages; identity is the pin; compile-cycle rejection; unchanged preprocessing/assembly otherwise |
| `jals-cli` | lockfile read/write via storage, `--locked`/`--frozen`, `jals update`/`fetch`/`tree`, workspace selection (`-p`/`--workspace`), target materialization orchestration |

## 9. Migration plan

Each phase lands green (workspace compiles, tests pass) and is independently valuable.

1. **Foundation (this change).** `jals-resolve` crate: versions, ids, summaries, lockfile,
   resolver algorithm, feature unification, fingerprint — portable, provider-driven, unit tested
   with an in-memory provider. No existing behavior changes.
2. **Manifest schema.** `jals-config`: registry/workspace dependency variants + `[workspace]` +
   `[registries]`; lowering to `DependencyRequest`; keep jar/git/path/wasm working. Update
   consumers' exhaustiveness matches (jals-project, jals-classpath, jals-cli, jals-lsp).
3. **Registry provider.** Maven metadata/POM parsing in `jals-resolve`; HTTP glue + artifact
   download in `jals-classpath`; resolve `group:artifact` into classpath entries; populate
   checksums. `jals build` can consume a registry dependency end-to-end.
4. **Graph integration.** `jals-project` runs resolution before discovery, acquires per pin, and
   makes node identity `PackageId`-based; lockfile read/write in `jals-cli`; `target/jals/deps`
   materialization; `--locked`/`--frozen`; `jals update`.
5. **Workspaces.** Member discovery (glob, exclude, default-members), inheritance, `-p`,
   workspace scope resolution, one lock/target.
6. **Feature audit.** Fold features + fingerprint into every key in §7, with a regression test
   per key (toggle a feature, assert a cold path).
7. **Ergonomics.** `jals tree`, `jals metadata`, `jals add`/`remove`, `--dry-run` classpath
   rendering from `deps/`.

## 10. Decisions taken (and why)

- **One version per name, not Cargo's multi-version graphs.** The JVM classpath admits one class
  per name; allowing two versions produces an order-dependent runtime. Conflicting requirements
  are a resolution error with an override hook (`[patch]`-style) deferred.
- **Cargo caret semantics for bare versions even on Maven registries.** The manifest is modeled
  on Cargo; Maven ranges remain available verbatim where Maven semantics are wanted.
- **A single resolver crate, portable.** The playground must resolve in-memory projects in the
  browser; keeping the algorithm I/O-free behind `Provider` is what preserves that, exactly as
  the existing graph splits `GraphHost`.
- **Dev dependencies resolve only for selected members.** Transitivity of dev dependencies is
  what makes test dependency graphs explode; Cargo does the same.
- **No backtracking in phase 1.** Unification over the intersection + lock preference is
  complete for monotone requirement sets and never worse than Maven's nearest-wins; a PubGrub
  solver can replace the choice function behind the same output type if a real graph demands it.

## 11. Open questions

- Should `package = "..."` renames apply to `jar`/`wasm` (they are not packages)? Current
  decision: no.
- Maven `exclusions`/`dependencyManagement` need a manifest surface of their own or are read
  only from POMs? Phase 3 reads POM semantics; a jals-side `exclusions = [...]` key is deferred.
- `jals.lock` checksums for path/git sources: content digest over the selected tree is stronger
  but expensive; phase 1 records none for source projects and pins their resolved identity
  instead.

## 12. Status

Landed (each commit is green on its own):

1. **Foundation** — `jals-resolve` (versions, ids, summaries, lockfile, resolver algorithm,
   feature unification, fingerprint) with its own test suite.
2. **Parallel resolution** — provider batches (`candidates_batch`/`summaries_batch`) with
   cross-pass memoization; the native Maven provider overlaps metadata and POM reads with
   `jals_exec::join_ordered`, while version choice stays sequential and deterministic.
3. **Manifest schema (registry only so far)** — `registry` dependencies (table and
   `group:artifact` shorthand) and `[registries]`, validated and lowered to resolver requests.
   Version requirements, `[workspace]`, and inheritance remain.
4. **Maven provider** — `maven-metadata.xml`, POM parent chains, property interpolation,
   `dependencyManagement`, BOM imports, scope/optional filtering. Exclusions, classifiers, and
   checksum sidecars are documented next steps.
5. **Graph/classpath integration** — registry entries resolve inside
   `NativeProjectPlan::assemble_native`, their jars enter the ordinary verified-cache download
   path, and `jals-cli` reads/writes `jals.lock` (only when the rendered bytes changed). The
   language server resolves without persisting. Registry entries are deliberately not graph
   nodes; `root_only` keeps them for the classpath phase.
6. **Lock lifecycle** — `--locked`/`--frozen` refuse a lock rewrite, `jals update` re-resolves
   ignoring the pins, and the lock is feature-independent: `LockMode::Generate` runs a second
   resolution pass with every optional and dev registry entry forced active, so toggling
   `--features` never churns the file.
7. **Feature/version cache audit** — the frontend key already folds dialect flags and (when
   `attributes` is on) build features; the backend key folds the classpath digest, and jars are
   content-addressed, so a lock/version/feature change moves every dependent key. `BackendOutput`
   memoization is still unimplemented (pre-existing `hawk` override).
8. **Workspace (partial)** — `[workspace]` members/exclude/default-members with host discovery
   (nearest root wins, glob expansion, `default-members` validated), one lock at the root:
   `RegistryResolver::resolve_workspace` resolves every member as a root, so a shared transitive
   package is one package, and a member-scoped classpath pass pins against the workspace lock
   without rewriting it. **Not yet**: `[workspace.dependencies]` inheritance and
   `<field>.workspace = true`, `default-members` as a build selection, `-p`, and building several
   members in one command.

Remaining phases: workspace inheritance and member selection; `jals fetch`/`tree`; the
`target/jals/deps` view; POM exclusions, classifiers, and checksum sidecars.
