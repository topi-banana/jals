# CLAUDE.md

Guidance for agents working in this repository.

`README.md` says what `jals` is and how it is used. This file is what a change has to respect: the
seams, the prohibitions, and the gates. Deeper per-area detail lives in the area's own document —
`jals-fmt/DESIGN.md`, `jals-lint/README.md`, `jals-build/README.md`, `jals-decompile/README.md`,
`jals-lsp/README.md`, `jals-tests/README.md`, `jals-playground/DESIGN.md` — and the reasoning
behind an enforced rule lives in that rule's `note:` block under `.ast-grep/rules/`. Prefer
following the pointer to restating it here.

## Architecture

`jals` is a Rust workspace for Java parsing, formatting, linting, semantic analysis, compilation,
LSP, and a Cargo-like build frontend. The lossless `jals-syntax` CST is shared by `jals-fmt`,
`jals-hir`, `jals-lint`, `jals-javac`, `jals-editor`, the CLI, the LSP, and the browser playground.

### Execution: async, current-thread, deliberately `!Send`

- Every runtime is current-thread (native: tokio current-thread + `LocalSet`; browser:
  `spawn_local`). Futures never cross threads, so `future_not_send` is allowed workspace-wide.
- `jals-exec` is the unified execution context. `Exec` is a cheap-clone handle over a sealed
  runtime core (`inline` / `tokio` / `wasm`); hosts construct it at the top
  (`jals_exec::tokio_rt::run` natively, `Exec::wasm()` in the browser, `Exec::inline()` for tests
  and pure in-memory use) and thread it down. Portable code never names a runtime.
- Multi-core parallelism exists only as `Exec::fan_out`: `Send` inputs and a `Send` closure are
  distributed to dedicated worker threads that each build and drive a `!Send` future locally;
  results always return in input order, so output is identical at any parallelism. Chunked
  fan-outs must use fixed chunk-size constants, never worker-count-derived ones.
- Cooperative yielding is runtime-free: `jals_exec::yield_now()` and the amortized
  `jals_exec::Yielder` need no `Exec` at all, so CPU crates (parsing, inference, formatting) take
  no execution parameter. Recursion over input is broken with `Box::pin` only at cycle
  back-edges/choke points, never on hot straight-line calls.
- Blocking syscalls live in native adapters only, wrapped in
  `jals_exec::tokio_rt::on_blocking_pool` (blocking pool on a runtime, inline off-runtime —
  fan-out worker threads are blocking-legal by design). tokio is named only by crates whose
  `std`/`native` features permit it; portable crates write runtime-agnostic async.

### Storage

Project data is owned by `jals-storage`. It is not a generic VFS.

- `Name`, `RelativePath`, `FileKey`, and `DirKey` are the only portable logical locations.
  `CodeTree` is an immutable, ordered snapshot of directories and file bytes. `ProjectStorage<S, C>`
  owns the base snapshot, editor overlay, artifact cache, and `Revision`.
- `MemorySource`/`MemoryCache` and `NativeSource`/`NativeCache` are sealed adapters implementing
  the same contract. Do not add consumer-defined backends.
- Native filesystem changes become visible only through `refresh()`. Existing `ProjectView`s must
  remain unchanged.
- Mutations use `transaction(expected_revision)` or overlay operations. A stale revision is an
  error, and a failed persistence operation must not publish a logical revision.
- `ArtifactCache` uses SHA-256 `ContentDigest` and typed `CacheKey` namespaces. Use the verified
  reads — whole-buffer `lookup` or streaming `open_verified` (one digest pass over the backend
  reader, rewind, then hand it out; native readers are buffered, pin the opened file, and every
  clone reads at an independent position) — and write-once `publish`; never implement `contains`
  followed by `write`. The advisory locator index (`indexed_key`/`record_index`,
  last-writer-wins) only recovers the content half of a key from its provenance; bytes are still
  read through the verified reads.
- `jals_storage::io` is the portable byte-stream seam (async `Read`/`Seek`, `Cursor`/`Buffered`;
  `std-io` bridges the sync-to-async `StdReader` newtype). In-memory sources complete every read
  immediately; only host-backed readers suspend. `jals-classfile` and the classpath zip reader
  parse through it; never blanket-impl its traits over `std::io` types (coherence with the
  slice/cursor impls) — bridge with newtypes. A sync view of an async reader is forbidden (it
  deadlocks a single-threaded runtime), which is why `ToStd` no longer exists.
- Backends are sealed and async. `CacheBackend` readers are `Clone + Send` (every clone reads at
  an independent position) so fan-out workers can consume owned clones; the backend itself is
  main-task-only. `SourceBackend::apply` runs its whole precondition/mutation/undo-journal batch
  as one uncancellable blocking task.

Do not reintroduce `jals-fs`, `FileTree`, arbitrary string paths, path predicates, or live
filesystem reads into portable interfaces.

## Crate boundaries

- `jals-storage`: typed keys, immutable revisions, transactions, overlays, cache, memory/native
  adapters (see *Storage* above). Only `native.rs` may use `std::path`/`std::fs`.
- `jals-config`: pure schemas and revision-aware config discovery over `ProjectView`, plus the
  shared severity vocabulary — the configured `LintLevel` and the presented `DiagnosticSeverity`.

  `[dependencies]` and `[dev-dependencies]` hold the same `Dependency` and differ only in *when* an
  entry is resolved, so which of them a resolution reads is a `DependencyScope` a host **states**
  (`Build` / `Test`) and never infers. `Test` is additive — the test run still needs the ordinary
  dependencies, exactly as `[test] source-dirs` adds to `[build] source-dirs`. `active_dependencies`
  and `declared_dependencies` are the only two spellings of "which entries", the second for callers
  that must see an entry a selection did not activate (discovery, the LSP watch set). A name in both
  tables is rejected rather than overridden as Cargo does: one name denotes one entry wherever it is
  read — `dep:<name>`, `<name>/<feature>`, one discovery edge.
  A crate that produces diagnostics states how they present without depending on an editor, which
  is why the vocabulary lives here: `jals-editor` and `jals-project` both assemble diagnostics and
  neither depends on the other. `jals-editor` re-exports the name, so a host still spells it
  `jals_editor::DiagnosticSeverity`.

  `lint::Config` is **sections of typed rule keys**, not a `BTreeMap<String, Severity>`: one section
  per rule `Category` — a *defect class*, so every rule is in exactly one — and one field per rule,
  declared by the `lint_section!` macro. Three consequences are load-bearing:
  - A rule's built-in level is the value its section's `Default` gives its key and lives **only**
    there, so `jals-lint`'s `RuleMeta` carries `fn(&Config) -> LintLevel` rather than a copy.
  - A rule key deserializes as a *patch* (`LintPatch`), so `[naming.naming-convention] fields =
    "any"` keeps the built-in level instead of restating it — which is why `Deserialize` is written
    out rather than derived.
  - An unrecognized key is recorded in `UnknownKeys` rather than rejected, because
    `deny_unknown_fields` would let one stale name stop every *other* rule in the file from
    loading; `Config::unknown_keys` is how a host reports what it kept.

  An option is always a value with every reachable state named — never an `Option<bool>`, and never
  two exclusive rules a config could ask for both of (clippy's `print_stdout`/`print_stderr` are one
  `streams` key). `Lint<O>`'s serialized *shape* follows the options **type**
  (`LintOptions::HAS_KEYS`) and not the values, which is what lets `jals-lint/tests/registry.rs`
  find every option by walking one serialized config.
- `jals-classpath`: resolution over project bytes and cache artifacts.
  - The in-house zip reader is isolated in `zip.rs` behind `archive` (portable, `no_std`, over the
    async io seam; also a stored-only writer for jar remap/merge; the `zip` crate is a dev-only
    fixture oracle). `jar.rs` is the **only** route to that writer — `JarPackage::write` packages
    compiled classes and `JarPackage::write_members` serializes a union somebody else assembled —
    and both put the manifest first, because `JarInputStream::getManifest` reads the first member
    and no other. `StoredZip`/`WriteMember` stay sealed, and a second caller reaching them is a
    second place that has to remember the ordering.
  - **What a manifest *is* lives in `manifest.rs`, and only there.** Two places write one — `jar.rs`
    packaging a fresh manifest, `remap.rs` editing one somebody else wrote — and they used to agree
    by writing the 72-byte fold rule and the `META-INF/` name matching down twice, which is two
    copies of a specification with one of them a release behind. `MetaInf` owns the member names a
    JVM matches case-insensitively (manifest, signature block, `META-INF/versions/<n>/`) and
    `Manifest` owns the bytes: attributes folded across continuation lines, main-section semantics,
    the digest strip. Three rules travel with it. A manifest is **edited, not re-rendered** — every
    transform returns the bytes it was given when it changes nothing, and writes an untouched
    attribute back verbatim, because normalizing a manifest nobody asked about is a diff in an
    artifact whose determinism is a stated invariant. A **main** attribute is not an individual one:
    `Multi-Release` and `Main-Class` are read and written in the main section and nowhere else. And
    a **member name is matched the way the JVM matches it** in both components, since matching one
    of the two loosely and the other exactly is what leaves half a claim standing.
  - **A manifest attribute that describes the archive survives a merge; one that describes the
    manifest's own side does not.** Only one `META-INF/MANIFEST.MF` can win a path conflict, and the
    overlay's does — but `Multi-Release` says the union's `META-INF/versions/<n>/` entries are live,
    and a union carries both sides' entries, so it is re-declared whenever *either* input declared
    it. Signature digests are the opposite case and go from both sides. Getting this wrong is not
    visible in a build: it is a class loaded from the wrong multi-release variant at run time.
  - **A transform's output version folds itself into whatever memoizes around it.** `remap.rs`'s
    `REMAP_OUTPUT_VERSION` / `MERGE_OUTPUT_VERSION` say what this crate *writes*, and a consumer
    that records a task's artifacts and replays them — `jals-project`'s `BuildTaskState` — names the
    transform's inputs in its key and nothing about the transform, so a bump here would be served
    the old bytes out of a warm cache. That happened twice, both times invisibly. `JarTransforms` is
    the fold that ends it: the consumer folds it into its key once, and a transform added or bumped
    here moves every such key with no edit on the consumer's side. Do not reintroduce a version
    number a consumer has to copy.
  - Mappings parsing, hierarchy-aware jar remapping, and compile-oriented jar decompilation into
    source trees live under `archive` too. Two grammars are read into one `Mappings` index —
    Mojang/ProGuard and Fabric's tiny v2 — and a format that names more than two namespaces carries
    the pair it is read through *inside* its `MappingFormat` variant, so the selection reaches the
    remap's provenance fold with it: `official→named` and `official→intermediary` over one tiny
    file are two jars.
  - A manifest is lowered into `ProjectInputPlan` by exactly two siblings — portable
    `MemoryProjectPlan` and host-path `NativeProjectPlan` — and there must never be a third: a host
    that lowers `[build] classpath` itself is a second rule that will drift. They differ only where
    a host path forces it: `MemoryProjectPlan` has no external fallback, because an in-memory
    project has one address space and an entry reaching outside it is a warning rather than a host
    path. **Everything else they must answer the same way**, `[test] source-dirs` included — a
    source root is the *shape of the project* an index walks and is captured unconditionally, so a
    sibling that lowered it and one that did not would be one project changing shape when it moves
    in-memory. What a command compiles is decided where its sources are gathered, never here.
  - A `Warning` carries its subject in `origin`, not in `message` — several messages name no
    location at all — so a host reports one by rendering the whole `Warning` through its `Display`,
    never `warning.message` alone.
  - `NetworkPolicy` is part of the `Fetcher`, not a value travelling beside it: a host that must
    not fetch constructs one that refuses, and every step it is handed inherits the refusal. That
    is why `ReqwestFetcher::for_project` takes the policy and has no `Default`, and why nothing
    downstream re-derives it. Implementors supply `fetch_admitted`/`fetch_bounded_admitted`, whose
    precondition is that the gate already admitted the locator; `io.rs`'s `Fetch` is the only
    caller, and `no-ungated-fetch.yml` keeps it that way. The gate refuses **network** locators
    only (`ExternalLocator::is_remote`, never `is_url`): the same seam carries `file://` and the
    host paths `NativeProjectPlan::classify` lowers an out-of-project `jar = "../lib/x.jar"` to,
    and refusing those offline breaks a build that never wanted the network.
  - **`RetrySchedule` rides the `Fetcher` for the same reason, and `Fetch` owns the loop.** A
    transient HTTP failure is retried with exponential backoff plus per-locator jitter — the jitter
    is derived from the locator rather than drawn, because `DependencyResolver::resolve` fetches
    concurrently and one shared schedule sends the whole fan-out back at the origin in one wave.
    `Fetch::admit` runs **outside** the loop, which is what makes an offline refusal structurally
    unretryable. The transient/permanent split is a `FetchError` an implementor states, because the
    only place it is knowable is where the `reqwest::Error` still exists; it stops at `Fetch`, so
    every layer above still sees the same `String` it always did. `ReqwestFetcher` carries the
    `connect`/`read` timeouts that make a retry reachable at all — a hung connection never becomes
    a failure a loop can classify — and deliberately no whole-request timeout, which would fail a
    slow link downloading a jar it was making steady progress on.
- `jals-project`: transitive path/Git/JAR project-graph discovery, stable node identity,
  dependency-first preprocessing, and artifact-only projection into `jals-classpath`. The portable
  memory graph operates on one captured `CodeTree`; only the `native` adapter may acquire host path
  trees or temporary Git checkouts.
  - The `DependencyScope` a host states applies to the **root manifest alone**: `walk.rs`'s
    recursion is hard-coded to `Build`, because `[dev-dependencies]` are not transitive. That is the
    one place the rule is written and it is invisible in the signature, so it carries a test.
  - Two edges reaching one `path` dependency are one node — identity is the canonicalized directory
    — and the features every in-edge routed to it are unioned there. A test-support library and its
    consumer can therefore both depend on the same SDK without becoming two selections.
  - What `TASK_EXECUTION_VERSION` versions is the *record*, not the transforms it names the
    artifacts of: `jals-classpath`'s `JarTransforms::fold` goes into the same provenance, so a remap
    or merge that starts writing different bytes invalidates every memo without that number moving.
    It is structural because the discipline it replaces failed twice — a shipped fix stayed
    invisible behind a warm cache, once for a jar that kept its signature block and once for a
    merged jar that kept saying `Multi-Release: false`.
  - Dependency snapshots are immutable and must never receive generated output: a dependency's
    build tasks run under `BuildTaskHost::Snapshot`, so their JARs and declared source trees are
    projected into the *consumer's* artifact cache instead of being published to the project they
    were declared in.
  - Which channel a published tree lands in is the `intent` its `tasks.publish_tree` declared, and
    a host never infers it: `navigation` becomes `library_source_artifacts` (a *view* of types the
    classpath defines, never a compile input), `compile` joins the node's authored sources through
    its own frontend and becomes `source_dependency_artifacts`. It is a routing and never a
    fan-out — a tree in both channels is one type mounted twice. A `replace-root` destination is
    owned by its publication in a dependency too, so an authored source captured under one is
    residue of a previous run and not an input.
  - The premise the `navigation` routing rests on is not enforced by the task graph, so
    preprocessing folds the node's own classpath into a `jals_classpath::ClasspathCoverage` and
    warns against the declaration when nothing defines a class under a published package — a
    *consumer-side* check, since discovery gives the root project no node. That answer is memoized
    in `CacheNamespace::PublicationCoverage`, deliberately not in `BuildTaskState`: `[build]
    classpath` is an input to one and not the other. Each task execution is memoized in
    `CacheNamespace::BuildTaskState` under the node identity, plan digest, and resolved features,
    and re-verified before reuse.
  - `[build] resource-dirs` files reach the jar through `resource.rs`, which owns both halves of
    resource templating: the `ResourcePlan` that answers which files are rendered, and the
    Jinja-subset engine that renders them. Both are crate-internal because `RemapPlan` is their
    only consumer, and the engine is in-house because every template crate on crates.io needs
    `std` while this crate is `no_std + alloc`. Selection is by
    *declaration* and never by content, so a resource nobody named is never decoded — which is what
    keeps a PNG a PNG. The snapshot scope that captures them stays feature-independent
    (`jals-classpath`'s `snapshot_scopes`): capture is unconditional, rendering is where a feature
    selection applies.
  - `ProjectAssembly` owns the **order and preconditions** of the whole procedure, and a host
    **cannot sequence the steps itself**: it calls `ProjectAssembly::script` for the root build
    script and its task plan, then `ProjectScript::resolve_memory` / `resolve_native` for
    discovery, preprocessing, projection, and input resolution. Those are the crate's *only* public
    entries into the procedure — `discover`, `preprocess`, `assemble`, `execute_root`, and the two
    projections are crate-internal, and `ProjectGraphAssembly`/`ResolvedProjectGraph`/
    `PreprocessedProjectGraph` are not exported at all, so the intermediate states cannot be held
    outside and re-ordered. Keep it that way; the crate's own tests live in `src`
    (`graph_tests.rs`) precisely so exercising a single step never requires publishing it.
  - `ProjectScript` is the only way from the first phase into the second (`skipped()` for a host
    that deliberately runs no script, such as `jals lint`). It is deliberately *two* calls rather
    than one: the aggregate hand-over point belongs to the host — `jals-cli` reopens storage under
    narrower scopes for the graph phase, and the playground releases its workspace lock so a jar
    download never blocks the editor. The policy each phase takes is the whole difference between
    hosts (`BuildTaskHost`/`SourcePublication`/`blocked_files` on the first, `GraphPreprocess` plus
    `ProjectInputOptions` on the second); the steps between them exist once.
- `jals-exec`: the execution context — `Exec`, fan-out, yields, runtime adapters (see *Execution*
  above). Only its `tokio`-feature module may name tokio; the portable core is `no_std`.
- `jals-progress`: what a run is doing, as data. `Activity` / `Outcome` / `Unit` / `Event` are
  **facts about work** — `Fetch`, never "Downloading"; `Fresh`, never a colour — and the verbs,
  colours, bars and templates belong to the consumer, exactly as `jals-hir` states a fact and the
  `jals-lint` rule that reports it owns the wording. Three properties are load-bearing.
  - **`Progress` is a value, not something hung off `Exec`.** `Exec` is `!Send`, so it cannot reach
    the fan-out workers this is most worth reporting from; and CPU crates here deliberately take no
    execution parameter at all, so tying reporting to `Exec` would deny it to the crates most likely
    to want it next. It rides an existing options struct where there is one — `TaskRuntime`,
    `GraphPreprocess`, `BackendRequest` — and is a parameter where there is not.
  - **A unit ends exactly once.** `Task` is RAII: `finish` states the outcome, `fresh` lets a step
    deep inside the work end its *caller's* unit from a memo hit, and `Drop` reports `Abandoned` for
    the error path that returned without saying anything. `Abandoned` means the emitter has a hole
    in it, not that the build failed, so an error path calls `finish(Failed)` explicitly.
    `Ticker` is the counting half a `fan_out` worker can hold — `Send + Sync`, cannot start or end
    a unit — and it exists because `JarRemap` remaps tens of thousands of classes across workers.
  - **No clock.** Portable code cannot read one, so a host stamps each event and hands the number to
    `Timeline::record`; `cargo --timings` records host-side for the same reason. `Timeline` renders
    itself as a self-contained HTML page or as JSON — a *document*, the way `jals_fmt::generate`
    renders a `jalsfmt.toml`, which is why it is here and not in a host.
- `jals-editor`: protocol-neutral workspace and query facade over `ProjectStorage`; file identity is
  `FileKey`, and source/config invalidation follows storage revisions. All three hosts index
  through `Workspace`, so `FileId`'s three-space partition (`workspace/file_id.rs`), `#[cfg]`
  evaluation, and path identity exist once. **Positional** queries need an `EditorHost` to decode a
  cursor and stay behind `Editor`; `Workspace::diagnostics` is the one query that takes a `FileKey`
  and no position, so it is `pub` and answers in the neutral `FileDiagnostic` — which is how
  `jals lint` joins the seam without implementing the positional host methods it has no cursor for.
  `ProjectQueries`/`QueryFile`/`FileRange` are crate-internal for the same reason `jals-hir` withholds
  `Resolved`: publishing them would publish a second, unrendered way to ask the same questions, and a
  host that took it would be re-implementing `Editor`.
  `ProjectLayout::with_classpath` lowers the `.class` files a host resolved, so describing a project
  needs no `jals-hir` symbol. A `FileKey` names a file inside a workspace and carries no address, so
  `EditorHost::location` is fallible: a host that cannot name a target does not offer it, rather than
  fabricating a URI for it.
- `jals-frontend`: the compile frontend seam — project sources lowered to the Java sources a backend
  compiles. `[build.frontend]` selects the lowering, and the dialect features in
  `[package] features` (`grouped-imports`, `attributes`) override it onto `DialectFrontend`; a host
  **never matches on `[build.frontend]` itself**, exactly as it never matches on `[build] backend`:
  it calls `FrontendSelection::for_manifest` once —
  `vanilla()` for a source tree with no manifest — so the decision table lives in one place, and
  with it the two rules that are cache identity rather than style (no dialect feature selects
  `VanillaFrontend` and not a flagless dialect, and `build_features` is folded in only when
  `attributes` is on). `FrontendSelection::lower` is the only way to run one: it imposes
  `FrontendKey::canonical_order` itself, so the driver that publishes into the artifact cache — and
  the ordering its digests depend on — is crate-internal rather than a precondition each host
  remembers. The `Frontend` trait is the seam for implementors; the flag sets they take are plain
  data, and `selection.rs` is the only module here that reads `jals-config`.
- `jals-build`: portable target/scaffold planning plus native JDK/process adapters. OS arguments,
  environment variables, and classpath separators stay in native/host code.
  - `[build] backend` selects what compiles the lowered tree: `javac` (a host process), `jals`
    (in-process, one class file per type), or `jals-wasm` (in-process, one WebAssembly module).
    Only the `javac` adapter is host-gated — `JalsBackend` is portable and builds for `wasm32` like
    the contract it implements. All three are reached through `Backend`, and a host **never matches
    on `[build] backend` itself**: it calls `BackendSelection` once — `in_process` where there is no
    process to spawn (the browser), `for_host` where there is — so the decision table lives in one
    place and absence is a value carrying a `BackendAbsence` rather than a failure raised later.
  - `Compiler`/`CompileRequest` are the crate-internal `javac` invocation layer *beneath* that
    seam, which `JavacBackend` drives once `StagedTree` has materialized the tree. `[toolchain]
    compiler` still chooses which tool runs, and `[toolchain] runtime` is selected independently
    for `jals run`'s run step.
  - A `BuildScriptDiagnostic`'s fields are sealed and it renders as `<severity>: <message>` through
    its own `Display`; `BuildScriptError::ReportedErrors` renders every diagnostic it carries, in
    emission order. A `build.warning` and a `build.error` read identically once the severity is
    dropped, so a host that prints `message()` into a plain string either restates a severity it
    re-derived or shows a warning as an error. `message()` is for a destination that already has a
    severity channel: an LSP `DiagnosticSeverity`, a Monaco marker, the `warning:` lead of a CLI
    line, a `GraphWarning`. Filling one of those from a *documented invariant* is not a
    re-derivation — `BuildScriptOutput::diagnostics` is warnings-only because a run that produced an
    error diverts the whole collection into `ReportedErrors` before an output exists.
- `jals-javac`: the compiler. Java source to executable code, for two targets off one front end
  (the CST plus `jals-hir`'s resolution, with no compiler IR between): JVM class files per declared
  type, and a single WasmGC module for a whole project. The two lowerings are separate because the
  JVM's control flow is a `goto` stream and wasm's is structured, so the wasm side lowers from the
  syntax tree and needs no relooper. It **never checks** — diagnostics are `jals-lint`'s job over
  `jals-hir` — but it does *resolve*, because emitting one `invokevirtual` needs the selected
  overload, its descriptor, and whether the owner is a class or an interface. Library signatures
  come from `jals-hir`'s embedded stubs, not from a host `ct.sym`, so the crate stays portable; a
  dev-only oracle checks those stubs against a real JDK.
  - `jvm::Assembler` owns the derivations `jals-classfile` deliberately refuses (that crate keeps
    branch offsets verbatim): label resolution with the widening fixpoint, `max_stack`/`max_locals`,
    and the `StackMapTable`, which is emitted as `full_frame` only. On the wasm side the host's
    collector owns every object — `struct.new_default`, declared subtyping, no `memory` section,
    and no allocator or collector of its own.
  - **Both backends publish the layer beneath their entry point, and neither materializes bytes
    before `finish`.** `jvm` exports `Assembler`, which records items and resolves them in
    `finish`; `wasm` exports `Insn`/`Instr` and `Module`, which hold a body as instructions until
    `Module::finish` encodes it, with `CompileWasm::module` handing back that module where
    `CompileWasm::project` hands back its bytes. The symmetry is what lets `tests/asm.rs`,
    `tests/wasm_asm.rs`, and `tests/wasm_lower.rs` assert an emission *without* a JVM or a wasm
    engine — which matters most on the wasm side, because CI's wasm cell runs this crate's tests
    under `wasm32-wasip1`, where a process cannot be spawned and every engine-backed assertion in
    `tests/wasm.rs` stands down. Keep the two seams exercised: `hawk::unnecessary_public` reports a
    published builder no test drives, so a widened vocabulary and the test that uses it land
    together.
  - The two lowerings share one layer and it has a name: `facts` answers what the *source* says —
    the span the inference memo is keyed on, the definition a name binds to, the locals a class
    captures, the constant a `case` label denotes (a full JLS §15.29 evaluator, `static final`
    constants included), the operator token run (`>>` is `[GT, GT]`, because the lexer never joins
    a `>` to what follows). It reads `TypedFile` and nothing else, so it names no instruction:
    `Layout`, `Slots`, `Descriptor`, and control flow stay with the backend that owns them.
    Crate-internal — a consumer wanting a fact about Java source asks `jals-hir`, not a compiler.
  - A fact both backends need goes in `facts`; one that names an instruction does not.
    `no-wasm-into-jvm-lowering` and its mirror `no-jvm-into-wasm-lowering` reject one backend naming
    the other, and `facts-names-no-instruction` rejects `Descriptor`/`ValType`/`Slots`/`Label`
    inside `facts`. All three are ratchets against one regression class and none catches a backend
    re-implementing a fact *inline*, so what makes a fact single-sourced is that there is one place
    to ask and it has a test. `facts` therefore carries its own `#[cfg(test)]` suites — the JLS
    §15.29 evaluator is verified with no JDK in reach, because the end-to-end tests stand down
    without one and CI's wasm cell never has one.
- `jals-hir`: the semantic analysis. Its three layers have one order — resolve a file, index the
  project, infer types against both — and that order lives in `FileAnalysis` / `FileSemantics` /
  `TypedFile` rather than in each consumer. `FileAnalysis` is index-independent, so it is the half a
  host caches per file and the half a project-wide find-references reads without inferring anything;
  `FileAnalysis::in_project` binds it to a `ProjectIndex` and is where the file's inference is
  memoized, so one lint pass, one editor request, or one file's compile runs it **once** instead of
  once per question. `Resolved` and `TypeInference` are the intermediate states and are **not
  exported**, exactly as `jals-project` withholds `ResolvedProjectGraph`. `TypedFile` is the witness
  that the inference has run, and therefore the only place types are readable without an `await` —
  which is what keeps `jals-javac`'s lowering synchronous.

  `jals-hir` states *facts* (`DeadIf`, `UnreportedException`, `TypeMismatch` with its
  `MismatchKind`, `UnresolvedType` and its value/method sibling `UnresolvedName`, `UnusedImport`,
  and the `unused_defs` a `Def`'s `is_private` / `is_annotated` let a consumer narrow); the
  **wording** of every semantic diagnostic belongs to the `jals-lint` rule that reports it. A
  negative fact — "nothing uses this" — over-approximates *use*: a member name spelled where the
  file-local pass cannot bind it (`this.x`, `Outer.Inner`, `X.class`, `@Anno`, the ambiguous-name
  qualifier of JLS §6.5.2, and anything inside a `cfg`-disabled host — which binds nothing but
  serves the *other* feature set) counts as a use, and a method's evidence is its **name** rather
  than its declaration, because the scope chain binds a call to *an* overload rather than to the one
  the arguments select. The mirror fact — "nothing *defines* this" — under-approximates definition
  for the same reason, so `UnresolvedName` stands down in each of those positions instead.

  `Member`/`Param` carry the **annotation types a declaration wrote**, qualified through the
  declaring file's own single-type imports — a fact about source, with no consumer's policy in it,
  which is what lets `jals-lint`'s `nullness-mismatch` read a contract another file wrote. Where an
  annotation *sits* and what a written `@Nullable` *denotes* both live in `jals_syntax::ast::Annotations`
  and are asked there by the index capture and by the linter alike; a second reader of either
  question is a reader that misses the direct-`ANNOTATION` shape or accepts anybody's `Nullable`.
  **An empty annotation list is not "the author wrote none".** A stub has no annotations to carry
  and a class file's are decoded by `jals-classfile` and not lowered here, so for those two it means
  *nobody looked* — `ItemOrigin::carries_annotations` is the question a consumer that reads silence
  as a claim must ask first, and getting it wrong reports every `null` the standard library accepts.
- `jals-lint`: the rule engine. A rule is a name, a `Category` (the `jalslint.toml` section it is
  configured under), a level accessor into `jals_config::lint`, and a checker; `RuleInfo::all()`
  publishes the registry so a consumer enumerates rules instead of restating them. **The rule name
  is the config key and the diagnostic's `rule` field**, unique across sections. The engine emits
  exactly one diagnostic outside the table — `cfg`, a structurally malformed attribute — and it is
  fixed at `error` because it is the failure the compile frontend rejects a build with, not a
  judgement.
  - In-source suppression is `@SuppressWarnings`, read from the CST and applied where a finding
    *becomes* a diagnostic rather than as a post-pass: a suppression names a rule **or the section
    it is configured under**, and a `Diagnostic` carries no `Category`, so filtering afterwards
    would recover one through a second path. The vocabulary (`all`, a rule name, a section name) is
    derived from the registry and `Category::ALL`, so a rule added later is suppressible the day it
    lands — which is also how javac's `@SuppressWarnings("unused")` silences the whole `[unused]`
    section for free. Running before the `cfg` errors are appended is what makes that one diagnostic
    unsuppressible *structurally* instead of by a rule-name test. Two limits are documented rather
    than solved: Java allows no annotation on an `import`, so `unused-imports` is config-only, and
    the name match is syntactic on the annotation's last segment, because resolving the annotation
    type would make the map depend on the analysis the rules have not run yet.
  - Two ledgers hold the crate's claims. `tests/registry.rs` joins the registry against the
    serialized schema in both directions, pins the default level set, and sweeps every schema option
    off its default requiring the linter to notice. `tests/inventory.rs` holds
    `jals-lint/inventory-rustc.tsv` / `inventory-clippy.tsv` — every rustc and clippy lint, in one
    of six buckets — against that same registry (`jals-lint/MAPPING-rustc-clippy.md` is the prose,
    `jals-lint/README.md` the roadmap). A new rule therefore lands in three places at once: the
    section that declares its key, the `RULES` table, and whichever ledger row now maps onto it.
- `jals test`: a test is a `#[test]` method, and the whole feature is three seams already in place
  rather than a fourth one beside them. `jals-syntax`'s `CfgMap` collects `TestHost`s — validating
  the shape a generated harness can call (`static void`, no parameters, not `private`, and every
  enclosing type nameable) so the failure is an edit-time diagnostic under the fixed `cfg` rule,
  not a build-time one. `jals-frontend` keeps those methods only when it lowers for a test run and
  synthesizes the Java that calls them: one shim per test *class* (per class, not per package —
  `String.equals` dispatch against a 64 KiB method cap), plus a root harness in the default
  package. `jals-build` owns the run: portable planning in `test_plan.rs` (filters, `--partition`)
  and the host half in `test_runner.rs` (one JVM per test over `Exec::fan_out`, output redirected
  to per-test scratch files rather than pipes, `-ea` prepended by the launcher). The contract
  between the two halves — the sentinel line, `--list`, `--quiet` — is owned by `jals-frontend` and
  travels to the runner as a `HarnessContract` value, so it is written once; the harness class is
  the fourth item and travels beside it as `RunRequest.main_class`. `[dev-dependencies]` is the
  fourth seam and the only new one: a test-support library is a project, resolved under
  `DependencyScope::Test` and therefore absent from everything that produces output. It is what
  `examples/minecraft_client_test` is — and what a dependency still cannot contribute is
  `build.add_jvm_arg` or `build.add_javac_arg`, both of which reach a compile or a test JVM from the
  root script only. A captured pass is the sentinel and never the exit status, which is also `1`
  for a missing main class and `0` for a body that called `System.exit(0)`; `--no-capture` gives up
  that reading along with the capture, and says so.
- `jals-cli`: the host boundary from clap `PathBuf` values to `NativeStorage` and typed keys. It
  owns the terminal: `shell::Shell` is the **only** thing in the crate that writes to a stream, and
  `no-raw-print.yml` keeps that structural rather than intended. Three rules live there and nowhere
  else — human output to stderr and machine output to stdout, `--color` answered once per stream,
  and a line written under a live bar suspending it — and `ui::Display` is where a
  `jals_progress::Activity` becomes a verb. `Session` wires one run's shell, sinks and `--timings`
  ledger together, so which sinks exist and what happens to them at the end is written once instead
  of once per command — including the two answers to **stdout having exactly one holder**.
  `Session::owns_stdout` is the command whose own machine output is the older contract there
  (`jals test`'s result objects, which a script parses) taking the event stream back;
  `Session::stdout_is_free` refuses the flags whose whole product is also stdout — `--dry-run`'s
  command line, `--diff`'s patch, a piped `jals fmt`'s formatted source. Both exist because a
  reader of stdout must never have to guess which of two schemas a line is, and dropping a product
  the user explicitly asked for is worse than saying the two do not go together. It also owns
  native-formatter-config **detection** (`migrate.rs`): portable
  crates cannot look at a
  filesystem, so the host decides which config file is there and reads its bytes through a
  `ProjectView`, then hands the text to `jals_fmt::import` and the result to `jals_fmt::generate`.
  What it keeps of project assembly is only what a host path forces: `NativeScope` selection,
  `materialize_file`/`materialize_tree`, `to_host_path`, and promoting a structured failure to
  `anyhow`. It opens the aggregate itself — `App::project_inputs` takes one rather than owning one —
  because `jals lint` keeps the revision the graph phase read and indexes it through
  `jals_editor::Workspace`, while `build`/`run` drop it once their artifacts are materialized. A
  reported file the snapshot does not capture (outside every scope, outside the root, or stdin,
  which is not even a `Name`) is mounted as an in-memory overlay under `.jals/lint/<n>/`, so it is a
  project file with the project's own index behind it rather than a detached one. This crate names
  no `jals-hir` symbol.
- `jals-lsp`: the only URI↔native-root adapter; watched-file notifications call `refresh()`. What it
  keeps of project assembly is diagnostic shaping, overlay mounting, the watch policy, and its own
  root-only fallback (a second `resolve_native` call, deliberately not folded in — it has one
  consumer).
  - It mounts two kinds of thing under `.jals/`: navigation sources materialized out of the artifact
    cache, and — under `.jals/lsp/` — every open document no project workspace owns. **Every** open
    Java document is answered by a `jals_editor::Workspace`; there is no second query surface, and
    `workspace_for` returning `None` means "no analysis for that URI" and nothing else. A
    project-less document is grouped by *parent directory* (same directory is same Java package),
    the directory is never walked, and one file is one key — which is how the "never index a whole
    checkout" property survives a routing rule that admits everything. A non-`file:` URI is its own
    group; `untitled:` is precisely why `LspHost` maps a key to a `Url` rather than to a host path.
  - A `FileKey` has no address of its own, so `EditorHost::location` is fallible and a key the host
    cannot name yields no target. That is the "do not generate fallback file URIs" invariant below,
    enforced by the type rather than by a comment — the rootless shared `LspHost` const renders no
    location at all, instead of panicking if one ever reached it.
- `jals-playground`: one `MemoryStorage` aggregate backs sidebar, editor overlays, and dependency
  artifacts. `compile.rs` is the *Build* pipeline — frontend seam, then `JalsBackend`, then
  `JarPackage` — taking sources as `(path, text)` and returning bytes, so it is host-testable and
  cannot reach the DOM; `download.rs` is the browser-only shim that saves those bytes. It honours
  `[build] backend`, and passes an empty classpath exactly as `jals-cli` does. The script phase runs
  under the workspace lock in `workspace.rs` and the graph phase off a detached snapshot in
  `app.rs`; the `ProjectScript` crossing between them is what keeps that split from also splitting
  the procedure.
- `jals-classfile`, `jals-syntax`, `jals-fmt`, `jals-decompile`: portable domain crates; do not add
  host filesystem APIs. `jals-fmt` has **one layout engine** — a port of google-java-format's greedy
  `computeBreaks` over a GJF-shaped `Doc`/`Level`/`Break` IR — and every style target is reached by
  tuning `jals_config::fmt::Config` on top of it, never by swapping engines
  (`jals-fmt/DESIGN.md`). Do not add an engine trait, a second renderer, or a Wadler/prettier
  `fits`. Its `import` and `generate` modules lower a native Eclipse / IntelliJ /
  google-java-format / Palantir / Spotless config onto that `Config` and render it back out as a
  `jalsfmt.toml`. All of it is pure and stays portable.
- Tests, `xtask`, and `editors/zed` may use host paths for fixtures and tooling.

## Code conventions

Five ast-grep rules under `.ast-grep/rules/` are `severity: error` and gate CI workspace-wide —
the four below plus `no-ungated-fetch`, described under *Crate boundaries*. Four more are scoped to
one crate by a `files:` key: `no-raw-print` below, and `jals-javac`'s three. Read the rule's own
`note:` before working around one.

- **`no-portable-host-path`** enforces the host boundary: `std::path`, `std::fs`, and `PathBuf` are
  allowed only in native, host, test, and tool adapters. The `ignores:` list in
  `.ast-grep/rules/no-portable-host-path.yml` is that allowlist; add a narrow adapter ignore only
  when OS identity is genuinely required.
- **`no-free-functions`**: a function lives on an `impl` block or a trait, so a `pub fn` at the top
  level of a module file is rejected. A genuinely free function is wrapped in an inline `mod` (see
  `jals-exec/src/yields.rs`'s `mod api`) or nested inside its only caller. `main` and the
  `#[test]`-family items are exempt, as are `build.rs`, `benches/`, `tests/`, and `examples/`; read
  the rule for the full list.
- **`no-extern-crate-alloc`** / **`no-extern-crate-core`**: `extern crate alloc;` is declared
  exactly once per portable crate, in its `lib.rs`; every other module writes `use alloc::...`.
  `extern crate core;` is never declared — write `use core::...`.
- **`no-raw-print`** (scoped to `jals-cli/src/**`): a print macro is `shell.rs`'s alone. Everything
  else goes through `Shell`, which is what makes the stream split, the colour decision and the
  bar-suspension answerable in one place. The failures a raw `println!` causes — a bar redrawn over
  a diagnostic, an escape in a redirected file — are exactly the ones a test that captures both
  streams into strings cannot see.

`jals-javac` additionally carries `facts-names-no-instruction`, `no-wasm-into-jvm-lowering`, and
`no-jvm-into-wasm-lowering`; see that crate's entry above.

Workspace lints are set once in the root `Cargo.toml` (`[workspace.lints]`, clippy
`all`/`pedantic`/`nursery` at `warn`, with `dbg_macro`/`todo`/`unimplemented` denied). A stale
`#[allow(...)]` is a hard CI failure via `cargo unused-allow`, so remove one as soon as it
suppresses nothing.

## `no_std` and features

Portable crates use `core + alloc`.

- `jals-exec --no-default-features` is `no_std + alloc`; `tokio` adds the native runtime adapter
  (current-thread bootstrap, worker pool, `on_blocking_pool`) and implies `std`, `wasm` adds the
  browser adapter.
- `jals-storage --no-default-features` is `no_std + alloc`; `std-io` adds only the `StdReader`
  bridge (wasm-safe, no host paths), and `std` adds the native adapters and implies `std-io` —
  `std` is also this crate's tokio feature (native adapters need `spawn_blocking`).
- `jals-classpath --no-default-features` is `no_std + alloc`; `archive` adds only `miniz_oxide` +
  `crc32fast` (still `no_std`/wasm-safe; parallel decode rides `Exec::fan_out`, entry-ordered at
  any worker count), and `native` implies `archive` and introduces HTTP plus `jals-storage/std` and
  `jals-exec/tokio`.
- `jals-project --no-default-features` is `no_std + alloc`; it includes the portable in-memory
  graph, Rhai dependency preprocessing (via `jals-build/rhai`, which it always enables), and
  archive projection. `native` adds host path/Git acquisition plus the native classpath, execution,
  and storage adapters.
- `jals-build --no-default-features` must remain a genuine portable core; its `rhai` feature stays
  portable too, and CI builds it for `wasm32`. `native` is the host half (JDK discovery, `javac`
  spawning, `native.rs`).
- `jals-frontend`, `jals-javac`, `jals-hir`, `jals-lint`, `jals-config`, `jals-syntax`,
  `jals-classfile`, `jals-decompile`, `jals-editor`, and `jals-progress` have no features at all, so
  a plain `cargo check` *is* the portability check — do not add one without a reason that survives
  review.
- `jals-fmt`'s `std` feature adds only `quick-xml` for the two XML-backed config importers.
  `jals-cli` enables it; the wasm playground resolves separately and never sees it.
- rayon is workspace-banned except in `jals-tests`' host-only harness; product fan-out goes
  through `jals-exec`.
- `serde` stays `default-features = false, features = ["derive", "alloc"]`.
- `toml` stays `default-features = false, features = ["parse", "serde"]`.

## Invariants

- Parsing is lossless and never panics on malformed input.
- Formatting is idempotent. It preserves the significant token multiset except where a **declared
  token-changing operation** applies. The operations are enumerated as data in
  `jals_fmt::passes::token_license::OPERATIONS` — `jals-fmt/DESIGN.md` §20's table — and the
  fail-safe reads that table rather than reconstructing the list from config keys.
  `Config::default()` licenses exactly: the unconditional dialect grouped-import trailing-comma
  drop; `[wrapping] remove-nested-parens`; and `[braces] force-*` (because `force-switch-arm =
  always`). `[imports] order = sort` is also on by default but is `Reorders` — sequence, not
  multiset. Every other configured row stays off/`preserve`. A new token-changing pass belongs in
  the table, and a new default-on row belongs in `the_default_config_enables_exactly_these_rows`.
  Long-string rewrapping *adds* `+` tokens when it splits a lone literal; what it preserves is
  what each concatenation spells.
- All project and artifact enumeration is deterministic.
- File/directory collisions, duplicate entries, file ancestors, root escape, unsafe archive
  members, and cache digest mismatches must be rejected or diagnosed structurally.
- Permission/I/O failures are not equivalent to missing data.
- Do not generate fallback file URIs for paths that cannot be represented.
- Preserve unrelated and untracked user files.

## Commands

The gates CI runs, cheapest first. Every one of them fails the build on its own.

```sh
cargo fmt --all --check
taplo fmt --check --diff
typos
ast-grep test --skip-snapshot-tests
ast-grep scan --error                    # plain `scan` exits 0 on findings; `--error` is the gate
cargo run -p xtask -- codegen --check
cargo machete
biome ci --error-on-warnings             # the JS sources scoped by biome.json
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo unused-allow --all-targets -- --workspace --all-features
cargo nextest run --workspace --all-features --no-fail-fast
cargo test --workspace --all-features --doc     # nextest does not run doctests
cargo hawk check -D warnings             # closed-world visibility over hawk.toml's roots
```

The portable-core and feature audit (CI's `portable core and feature audit` job) — run it whenever
a portable crate gains a dependency, a feature, or a `use`:

```sh
cargo check -p jals-storage --no-default-features
cargo check -p jals-classpath --no-default-features
cargo check -p jals-build --no-default-features
cargo check -p jals-project --no-default-features
cargo check -p jals-frontend
cargo check -p jals-progress
cargo check -p jals-project --all-features
cargo check -p jals-build --no-default-features --features rhai --target wasm32-unknown-unknown
cargo check -p jals-classpath --no-default-features --target wasm32-unknown-unknown
cargo check -p jals-project --no-default-features --target wasm32-unknown-unknown
cargo check -p jals-frontend --target wasm32-unknown-unknown
cargo check -p jals-progress --target wasm32-unknown-unknown
cargo build -p jals-playground --target wasm32-unknown-unknown
cargo tree -e features -p jals-classpath --no-default-features
cargo tree -e features -p jals-build --no-default-features
cargo tree -e features -p jals-project --no-default-features
```

Not gated by CI, but worth running when you touch one of these crates' feature seams — a
workspace build enables the union of features and so proves nothing about them:

```sh
cargo check -p jals-exec --no-default-features
cargo check -p jals-exec --features tokio
cargo check -p jals-storage --no-default-features --features std-io
cargo check -p jals-classpath --no-default-features --features archive --target wasm32-unknown-unknown
cargo check -p jals-javac --target wasm32-unknown-unknown
```

Run `cargo run -p xtask -- codegen` after changing `jals-syntax/java.ungram`, and commit generated
AST changes with the grammar change.

## What CI checks that local runs usually miss

CI runs clippy, test, build, and build-release on linux, macOS, Windows **and** wasm; hawk runs on
linux and macOS only, because the tool publishes no Windows build and the from-source one answers
wrong (see the job's comment in `.github/workflows/ci.yml`). The three host platforms take
`--workspace`; the wasm cells take a package set, and the sets are defined once in
`.github/workflows/ci.yml`'s `env` block (`WASM_PACKAGES`, `WASM_CORE_PACKAGES`,
`WASM_TEST_PACKAGES`) rather than per job. Three consequences for local work:

- A `dead_code` finding can exist in one configuration only. An item reachable solely from a
  `std`/`native`-gated module must carry that gate itself — the wasm clippy cells run `-D warnings`
  against the portable configuration, where such an item has no caller.
- Tests run as wasm under `wasm32-wasip1` (`wasm32-unknown-unknown` has no way to run a test
  harness), so a test in one of those crates must reach for no host: no process spawn, and no
  `tempfile` (`std::env::temp_dir` is unimplemented on wasi). Reproduce that cell with
  `CARGO_TARGET_WASM32_WASIP1_RUNNER="wasmtime --dir /::/" cargo test --target wasm32-wasip1 …`.
- A host-dependent test stands itself down loudly (`javac_available()` and its siblings) rather than
  failing where the host cannot supply what it needs — a filesystem that rejects a non-UTF-8 name,
  or a temporary directory reached through a symlink (macOS `/var` → `/private/var`, so compare
  canonicalized paths).

Every project under `examples/` is a CI cell of its own (`example (<name>)`), running what its
README tells a reader to run: `jals build`, then `jals fmt --check` and `jals lint` over the
example's **tracked** `.java` files. Tracked is what separates authored source from published
output — a build script's publication into a source root is untracked by construction — so the gate
never scores a decompiled skeleton as something someone wrote. The fmt/lint step runs under the
cell's own `dir`, so a project reached only through a dependency edge still needs a cell of its own
the moment it has a tracked `.java`. Seven consequences for an example:

- A `tasks.project_jar` example needs its JAR, and a JAR is a binary, so none is committed:
  `examples/scripts/gen-vendor-jars.sh` writes the two the `task_dependency` and
  `task_source_archive` examples read, and CI runs it before every cell.
- `minecraft` is the one example whose `jals build` is *not* required to succeed, because its
  published skeleton tree is documented not to compile (`examples/minecraft/README.md`
  §Compile-safety). That cell asserts the pipeline instead — fetch → nested extract → remap →
  decompile → publish — by requiring all three publication roots to come out non-empty, which is a
  statement only a run that reached the last step can make.
- `minecraft_mod (client)` is the one cell that *runs* Minecraft rather than compiling against it.
  It sets `headless_gl` (an apt install of `xvfb libgl1-mesa-dri libglx-mesa0`, and the test step
  under `xvfb-run` with Mesa's llvmpipe) and `test_flags: -j 1 --timeout 600`, because each test
  boots its own client and two at once want two GL contexts. A failed run uploads the client's
  `logs/` and `crash-reports/`. It is also the cell whose fmt/lint step depends on the *test* step
  having run: analysis is always offline, and the client's runtime jars are fetched by a
  `[dev-dependencies]` entry, which `jals build` does not resolve.
- `examples/scripts/gen-client-runtime.py` is a **generator, not a build step**: it rewrites the
  `const RUNTIME` table in `examples/minecraft_client_test/build.rhai` between two exact markers,
  and its output is committed. CI never runs it. It takes no arguments and writes **every** release
  — the list and each release's metadata digest come from `examples/minecraft/build.rhai`'s own
  `CATALOG`, so no release list is restated and no mutable version manifest is consulted — and it
  refuses to write at all when it cannot read one library of one release, because a table that is
  partly regenerated is a boot that dies in `SharedLibraryLoader` with its cause two files away.
- The client harness supports the same 43 releases the SDK does, and **one feature selects it** — a
  release (`minecraft/<version>` into the SDK, plus one threshold). There is deliberately no second
  feature asking whether the harness is wanted: being a `[dev-dependencies]` entry is already that
  answer, since `jals test` and the analysis hosts resolve one and nothing that produces output
  does. `client` is therefore on the dependency edge (`features = ["client"]`) rather than in a
  feature, and a consumer routes only `mc-client-test/<version>` from each of its own version
  features. The cost is stated rather than hidden: `jals test --features <version>` pulls the client
  jar and the ~60 runtime libraries even without the consumer's own `client-test`, so a consumer
  whose defaults route `minecraft/server` compiles its tests against the SDK's **merged** jar where
  its build used the server one — a second fetch and a second whole-game remap, which is why all
  three `minecraft_mod` cells now run `jals test` before their offline lint. `build.rhai` rejects a selection naming no release,
  because the SDK falls back to its newest while every threshold stays off. The `#[cfg]` in
  `GameClient.java` names a *threshold*, never a release, and the fourteen thresholds are that
  project's own: `examples/minecraft_mod` reads the same catalog through five of its own, because it
  branches on different things. Two of the fourteen boundaries are invisible in a mapping file,
  which carries no access flags — they were found by compiling, which is what the 43-cell matrix is
  for.
- The harness is **Java 8 source** and its `--release` follows the game's own
  `javaVersion.majorVersion` (8/16/17/21), because it is loaded by the JVM the release runs on. That
  is also the one place `jals build` and `jals test` want different JDKs, and `$JAVAC`/`$JAVA`
  resolve independently so one command can say both.
- `client harness (<release>)` is a 43-cell matrix modelled on `mod jar`, and its assertion is not
  the exit status: a green build says a selection resolved, not that a type came out, so the cell
  checks `GameClient.class` exists. Running the build script is also what verifies all 2287 pinned
  library digests.
