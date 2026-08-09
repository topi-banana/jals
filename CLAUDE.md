# CLAUDE.md

Guidance for agents working in this repository.

## Architecture

`jals` is a Rust workspace for Java parsing, formatting, linting, semantic analysis, LSP, and a
Cargo-like build frontend. The lossless `jals-syntax` CST is shared by `jals-fmt`, `jals-hir`,
`jals-lint`, `jals-editor`, the CLI, the LSP, and the browser playground.

The workspace is fully async on a deliberately `!Send` execution model:

- Every runtime is current-thread (native: tokio current-thread + `LocalSet`; browser:
  `spawn_local`). Futures never cross threads, so `future_not_send` is allowed workspace-wide.
- `jals-exec` is the unified execution context. `Exec` is a cheap-clone handle over a sealed
  runtime core (`inline` / `tokio` / `wasm`); hosts construct it at the top
  (`jals_exec::tokio_rt::run` natively, `Exec::wasm()` in the browser, `Exec::inline()` for
  tests and pure in-memory use) and thread it down. Portable code never names a runtime.
- Multi-core parallelism exists only as `Exec::fan_out`: `Send` inputs and a `Send` closure are
  distributed to dedicated worker threads that each build and drive a `!Send` future locally;
  results always return in input order, so output is identical at any parallelism. Chunked
  fan-outs must use fixed chunk-size constants, never worker-count-derived ones.
- Cooperative yielding is runtime-free: `jals_exec::yield_now()` and the amortized
  `jals_exec::Yielder` are free functions, so CPU crates (parsing, inference, formatting) take
  no execution parameter at all. Recursion over input is broken with `Box::pin` only at cycle
  back-edges/choke points, never on hot straight-line calls.
- Blocking syscalls live in native adapters only, wrapped in
  `jals_exec::tokio_rt::on_blocking_pool` (blocking pool on a runtime, inline off-runtime —
  fan-out worker threads are blocking-legal by design). tokio is used only by crates whose
  `std`/`native` features permit it; portable crates write runtime-agnostic async.

Project data is owned by `jals-storage`. It is not a generic VFS:

- `Name`, `RelativePath`, `FileKey`, and `DirKey` are the only portable logical locations.
- `CodeTree` is an immutable, ordered snapshot containing directories and file bytes.
- `ProjectStorage<S, C>` owns the base snapshot, editor overlay, artifact cache, and `Revision`.
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
  adapters. Only `native.rs` may use `std::path`/`std::fs`.
- `jals-config`: pure schemas and revision-aware config discovery over `ProjectView`, plus the
  shared severity vocabulary — the configured `Severity` and the presented `DiagnosticSeverity` —
  so a crate that produces diagnostics states how they present without depending on an editor.
  `jals-editor` and `jals-project` both assemble diagnostics, neither depends on the other, and
  this is the only crate they share; before it lived here, a terminal and a browser had each
  written the same `ProjectDiagnosticSeverity` table two crates apart. `jals-editor` re-exports
  the name, so a host still spells it `jals_editor::DiagnosticSeverity`.
- `jals-classpath`: resolution over project bytes and cache artifacts. The in-house zip reader is
  isolated in `zip.rs` behind `archive` (portable, `no_std`, over the async io seam; also a
  stored-only writer for jar remap/merge; the `zip` crate is a dev-only fixture oracle). `jar.rs`
  is the only public surface over that writer: `JarPackage::write` packages compiled classes,
  generating the `META-INF/MANIFEST.MF` a jar needs (first member, CRLF, 72-byte wrapped) and
  keeping `StoredZip`/`WriteMember` sealed.
  Mojang/ProGuard mappings parsing, hierarchy-aware jar remapping, and compile-oriented jar
  decompilation into source trees live under `archive` too. HTTP/local locator lowering is in its
  native adapter. A manifest's `[build]` section is lowered into `ProjectInputPlan` by exactly two
  siblings — portable `MemoryProjectPlan` and host-path `NativeProjectPlan` — and there must never
  be a third: a host that lowers `[build] classpath` itself is a second rule that will drift.
  `MemoryProjectPlan` has no external fallback because an in-memory project has one address space;
  an entry reaching outside it is a warning, not a host path. A `Warning` carries its subject in
  `origin`, not in `message` — several messages name no location at all — so a host reports one by
  rendering the whole `Warning` through its `Display`, never `warning.message` alone.
  `NetworkPolicy` is part of the `Fetcher`, not a value travelling beside it: a host that must not
  fetch constructs one that refuses, and every step it is handed inherits the refusal. That is why
  `ReqwestFetcher::for_project` takes the policy and has no `Default`, and why nothing downstream
  re-derives it — a phase that built its own fetcher is how `--offline`, `jals lint`, and the
  language server all used to issue live HTTP GETs for uncached dependency jars. Implementors
  supply `fetch_admitted`/`fetch_bounded_admitted`, whose precondition is that the gate already
  admitted the locator; `io.rs`'s `Fetch` is the only caller, and `no-ungated-fetch.yml` keeps it
  that way because a trait method cannot be `pub(crate)`. The gate refuses **network** locators
  only (`ExternalLocator::is_remote`, never `is_url`): the same seam carries `file://` and the host
  paths `NativeProjectPlan::classify` lowers an out-of-project `jar = "../lib/x.jar"` to, and
  refusing those offline breaks a build that never wanted the network.
- `jals-project`: transitive path/Git/JAR project-graph discovery, stable node identity,
  dependency-first preprocessing, and artifact-only projection into `jals-classpath`. The portable
  memory graph operates on one captured `CodeTree`; only the `native` adapter may acquire host path
  trees or temporary Git checkouts. Dependency snapshots are immutable and must never receive
  generated output: a dependency's build tasks run under `BuildTaskHost::Snapshot`, so their JARs
  and declared source trees are projected into the *consumer's* artifact cache instead of being
  published to the project they were declared in. Which channel a published tree lands in is the
  `intent` its `tasks.publish_tree` declared, and a host never infers it: `navigation` becomes
  `library_source_artifacts` (a *view* of types the classpath defines, never a compile input),
  `compile` joins the node's authored sources through its own frontend and becomes
  `source_dependency_artifacts`. It is a routing and never a fan-out — a tree in both channels is
  one type mounted twice. A `replace-root` destination is owned by its publication in a dependency
  too, so an authored source captured under one is residue of a previous run and not an input; that
  is what keeps a consumer's compile set independent of whether anyone ever built the dependency in
  place. The premise the `navigation` routing rests on is not enforced by the task graph, so
  preprocessing folds the node's own classpath into a `jals_classpath::ClasspathCoverage` and warns
  against the declaration when nothing defines a class under a published package — a *consumer-side*
  check, since discovery gives the root project no node. That answer is memoized in
  `CacheNamespace::PublicationCoverage`, deliberately not in `BuildTaskState`: `[build] classpath`
  is an input to one and not the other. Each task execution is memoized in
  `CacheNamespace::BuildTaskState` under the node identity, plan digest, and resolved features, and
  re-verified before reuse.
  `ProjectAssembly` owns the **order and preconditions** of the whole procedure, and a host
  **cannot sequence the steps itself**: it calls `ProjectAssembly::script` for the root build
  script and its task plan, then `ProjectScript::resolve_memory` / `resolve_native` for discovery,
  preprocessing, projection, and input resolution. Those are the crate's *only* public entries into
  the procedure — `discover`, `preprocess`, `assemble`, `execute_root`, and the two projections are
  crate-internal, and `ProjectGraphAssembly`/`ResolvedProjectGraph`/`PreprocessedProjectGraph` are
  not exported at all, so the intermediate states cannot be held outside and re-ordered. Keep it
  that way: a step that becomes `pub` for one caller is the hand-sequencing this seam removed, and
  the crate's own tests live in `src` (`graph_tests.rs`) precisely so exercising a single step never
  requires publishing it. `ProjectScript` is the only way from the first phase into the second
  (`skipped()` for a host that deliberately runs no script, such as `jals lint`). It is deliberately
  *two* calls rather than one: the aggregate hand-over point belongs to the host — `jals-cli`
  reopens storage under narrower scopes for the graph phase, and the playground releases its
  workspace lock so a jar download never blocks the editor. The policy each phase takes is the whole
  difference between hosts (`BuildTaskHost`/`SourcePublication`/`blocked_files` on the first,
  `GraphPreprocess` plus `ProjectInputOptions` on the second); the steps between them exist once.
- `jals-exec`: the execution context (`Exec`, fan-out, yields, runtime adapters). Only its
  `tokio`-feature module may name tokio; the portable core is `no_std`.
- `jals-editor`: protocol-neutral workspace and query facade over `ProjectStorage`; file identity is
  `FileKey`, and source/config invalidation follows storage revisions. All three hosts index
  through `Workspace`, so `FileId`'s three-space partition (`workspace/file_id.rs`), `#[cfg]`
  evaluation, and path identity exist once — a host that built its own `jals_hir::ProjectIndex`
  re-derived all three, and spelled the id space with a bare counter. **Positional** queries need
  an `EditorHost` to decode a cursor and stay behind `Editor`; `Workspace::diagnostics` is the one
  query that takes a `FileKey` and no position, so it is `pub` and answers in the neutral
  `FileDiagnostic` — which is how `jals lint` joins the seam without implementing nine host
  methods, eight of which it would have to fabricate. `ProjectLayout::with_classpath` lowers the
  `.class` files a host resolved, so describing a project needs no `jals-hir` symbol.
- `jals-frontend`: the compile frontend seam — project sources lowered to the Java sources a backend
  compiles. Featureless and portable in every configuration. `[build.frontend]` selects the
  lowering, and the dialect features in `[package] features` (`grouped-imports`, `attributes`)
  override it onto `DialectFrontend`; a host **never matches on `[build.frontend]` itself**, exactly
  as it never matches on `[build] backend`: it calls `FrontendSelection::for_manifest` once —
  `vanilla()` for a source tree with no manifest — so the decision table lives in one place, and
  with it the two rules that are cache identity rather than style (no dialect feature selects
  `VanillaFrontend` and not a flagless dialect, and `build_features` is folded in only when
  `attributes` is on). `FrontendSelection::lower` is the only way to run one: it imposes
  `FrontendKey::canonical_order` itself, so the driver that publishes into the artifact cache — and
  the ordering its digests depend on — is crate-internal rather than a precondition each host
  remembers. The `Frontend` trait is the seam for implementors; the flag sets they take are plain
  data, and `selection.rs` is the only module here that reads `jals-config`.
- `jals-build`: portable target/scaffold planning plus native JDK/process adapters. OS arguments,
  environment variables, and classpath separators stay in native/host code. `[build] backend`
  selects what compiles the lowered tree: `javac` (a host process), `jals` (in-process, one class
  file per type), or `jals-wasm` (in-process, one WebAssembly module). Only the `javac` adapter is
  host-gated — `JalsBackend` is portable, and builds for `wasm32` like the contract it implements.
  All three are reached through `Backend`, and a host **never matches on `[build] backend` itself**:
  it calls `BackendSelection` once — `in_process` where there is no process to spawn (the browser),
  `for_host` where there is — so the decision table lives in one place and absence is a value
  carrying a `BackendAbsence` rather than a failure raised later. `Compiler`/`CompileRequest` are the
  crate-internal `javac` invocation layer *beneath* that seam, which `JavacBackend` drives once
  `StagedTree` has materialized the tree; `[toolchain] compiler` still chooses which tool runs, and
  `[toolchain] runtime` is selected independently for `jals run`'s run step.
  A `BuildScriptDiagnostic`'s fields are sealed and it renders as `<severity>: <message>` through its
  own `Display`; `BuildScriptError::ReportedErrors` renders every diagnostic it carries, in emission
  order. A `build.warning` and a `build.error` read identically once the severity is dropped, so a
  host that prints `message()` into a plain string either restates a severity it re-derived or shows
  a warning as an error — which is why the rendering lives here. `message()` is for a destination
  that already has a severity channel: an LSP `DiagnosticSeverity`, a Monaco marker, the `warning:`
  lead of a CLI line, a `GraphWarning`. Filling one of those from a *documented invariant* is not a
  re-derivation — `BuildScriptOutput::diagnostics` is warnings-only because a run that produced an
  error diverts the whole collection into `ReportedErrors` before an output exists, so promoting it
  needs no severity test, and writing one there erases an error rather than surfacing it.
- `jals-cli`: the host boundary from clap `PathBuf` values to `NativeStorage` and typed keys. It
  also owns native-formatter-config **detection** (`migrate.rs`): portable crates cannot look at a
  filesystem, so the host decides which config file is there and reads its bytes through a
  `ProjectView`, then hands the text to `jals_fmt::import` and the result to
  `jals_fmt::generate`. What it keeps of project assembly is only what a host path forces:
  `NativeScope` selection, `materialize_file`/`materialize_tree`, `to_host_path`, and promoting a
  structured failure to `anyhow`. It opens the aggregate itself — `App::project_inputs` takes one
  rather than owning one — because `jals lint` keeps the revision the graph phase read and indexes
  it through `jals_editor::Workspace`, while `build`/`run` drop it once their artifacts are
  materialized. A reported file the snapshot does not capture (outside every scope, outside the
  root, or stdin, which is not even a `Name`) is mounted as an in-memory overlay under
  `.jals/lint/<n>/`, so it is a project file with the project's own index behind it rather than a
  detached one. This crate names no `jals-hir` symbol.
- `jals-lsp`: the only URI↔native-root adapter; watched-file notifications call `refresh()`. What it
  keeps of project assembly is diagnostic shaping, overlay mounting of navigation sources, the watch
  policy, and its own root-only fallback (a second `resolve_native` call, deliberately not folded in
  — it has one consumer).
- `jals-playground`: one `MemoryStorage` aggregate backs sidebar, editor overlays, and dependency
  artifacts. `compile.rs` is the *Build* pipeline — frontend seam, then `JalsBackend`, then
  `JarPackage` — taking sources as `(path, text)` and returning bytes, so it is host-testable and
  cannot reach the DOM; `download.rs` is the browser-only shim that saves those bytes. It honours
  `[build] backend`, and passes an empty classpath exactly as `jals-cli` does. The script phase runs
  under the workspace lock in `workspace.rs` and the graph phase off a detached snapshot in
  `app.rs`; the `ProjectScript` crossing between them is what keeps that split from also splitting
  the procedure.
- `jals-javac`: the compiler. Java source to executable code, for two targets off one front end
  (the CST plus `jals-hir`'s resolution, with no compiler IR between): JVM class files per declared
  type, and a single WasmGC module for a whole project. The two lowerings are separate because the
  JVM's control flow is a `goto` stream and wasm's is structured, so the wasm side lowers from the
  syntax tree and needs no relooper. It **never checks** — diagnostics are `jals-lint`'s job over
  `jals-hir` — but it does *resolve*, because emitting one `invokevirtual` needs the selected
  overload, its descriptor, and whether the owner is a class or an interface. Library signatures
  come from `jals-hir`'s embedded stubs, not from a host `ct.sym`, so the crate stays portable; a
  dev-only oracle checks those stubs against a real JDK. `jvm::Assembler` owns the derivations
  `jals-classfile` deliberately refuses (that crate keeps branch offsets verbatim): label
  resolution with the widening fixpoint, `max_stack`/`max_locals`, and the `StackMapTable`, which
  is emitted as `full_frame` only. On the wasm side the host's collector owns every object —
  `struct.new_default`, declared subtyping, no `memory` section, and no allocator or collector of
  its own. Portable and featureless; no host filesystem APIs.
- `jals-hir`: the semantic analysis. Its three layers have one order — resolve a file, index the
  project, infer types against both — and that order lives in `FileAnalysis` / `FileSemantics` /
  `TypedFile` rather than in each consumer. `FileAnalysis` is index-independent, so it is the half a
  host caches per file and the half a project-wide find-references reads without inferring anything;
  `FileAnalysis::in_project` binds it to a `ProjectIndex` and is where the file's inference is
  memoized, so one lint pass, one editor request, or one file's compile runs it **once** instead of
  once per question. `Resolved` and `TypeInference` are the intermediate states and are **not
  exported**, exactly as `jals-project` withholds `ResolvedProjectGraph`: holding one would be
  holding a step, and the steps are not separately orderable. `TypedFile` is the witness that the
  inference has run, and therefore the only place types are readable without an `await` — which is
  what keeps `jals-javac`'s lowering synchronous. `jals-hir` states *facts* (`DeadIf`,
  `UnreportedException`, `TypeMismatch` with its `MismatchKind`, `UnresolvedType`); the **wording**
  of every semantic diagnostic belongs to the `jals-lint` rule that reports it, alongside the rule
  name and the `jalslint.toml` key.
- `jals-classfile`, `jals-hir`, `jals-syntax`, `jals-fmt`, `jals-lint`, `jals-decompile`: portable
  domain crates; do not add host filesystem APIs. `jals-fmt` has **one layout engine** — a port of
  google-java-format's greedy `computeBreaks` over a GJF-shaped `Doc`/`Level`/`Break` IR — and
  every style target is reached by tuning `jals_config::fmt::Config` on top of it, never by
  swapping engines (`jals-fmt/DESIGN.md`). Do not add an engine trait, a second renderer, or a
  Wadler/prettier `fits`. Its `import` and `generate` modules lower a native Eclipse / IntelliJ /
  google-java-format / Palantir / Spotless config onto that `Config` and render it back out as a
  `jalsfmt.toml`. All of it is pure and stays portable.
- Tests, `xtask`, and `editors/zed` may use host paths for fixtures and tooling.

The `.ast-grep/rules/no-portable-host-path.yml` allowlist enforces the host boundary. Add a narrow
adapter ignore only when OS identity is genuinely required.

## `no_std` and features

Portable crates use `core + alloc`. Each such crate declares `extern crate alloc;` exactly once, in
its `lib.rs`; every other module imports with `use alloc::...`. The
`.ast-grep/rules/no-extern-crate-alloc.yml` rule rejects the declaration anywhere else.

- `jals-exec --no-default-features` is `no_std + alloc`; `tokio` adds the native runtime adapter
  (current-thread bootstrap, worker pool, `on_blocking_pool`), `wasm` the browser adapter.
- `jals-storage --no-default-features` is `no_std + alloc`; `std-io` adds only the `StdReader`
  bridge (wasm-safe, no host paths), and `std` adds the native adapters and implies `std-io` —
  `std` is also this crate's tokio feature (native adapters need `spawn_blocking`).
- `jals-classpath --no-default-features` is `no_std + alloc`; `archive` adds only `miniz_oxide` +
  `crc32fast` (still `no_std`/wasm-safe; parallel decode rides `Exec::fan_out`, entry-ordered at
  any worker count), and `native` introduces HTTP plus `jals-storage/std` and `jals-exec/tokio`.
- `jals-project --no-default-features` is `no_std + alloc`; it includes the portable in-memory
  graph, Rhai dependency preprocessing, and archive projection. `native` adds host path/Git
  acquisition plus the native classpath, execution, and storage adapters.
- `jals-build --no-default-features` must remain a genuine portable core.
- rayon is workspace-banned except in `jals-tests`' host-only harness; product fan-out goes
  through `jals-exec`.
- `jals-cli` enables `jals-fmt/std`, which adds only `quick-xml` for the two XML-backed config
  importers. The wasm playground resolves separately and never sees it.
- `serde` stays `default-features = false, features = ["derive", "alloc"]`.
- `toml` stays `default-features = false, features = ["parse", "serde"]`.

## Invariants

- Parsing is lossless and never panics on malformed input.
- Formatting is idempotent. It preserves the significant token multiset except where a **declared
  token-changing operation** applies. The operations are enumerated as data in
  `jals_fmt::passes::token_license::OPERATIONS` — `jals-fmt/DESIGN.md` §20's table — and the
  fail-safe reads that table rather than reconstructing the list from config keys. Seven rows are
  configured and every one is off (or `preserve`) by default: import ordering, unused-import
  removal, modifier ordering, long-string rewrapping, text-block re-indentation, the literal
  normalizations, and brace forcing. The eighth is **unconditional** — the jals dialect drops a
  grouped import's trailing comma — so "explicitly configured" is not a complete qualifier, and a
  new token-changing pass belongs in the table, not in prose. Long-string rewrapping *adds* `+`
  tokens when it splits a lone literal; what it preserves is what each concatenation spells.
- All project and artifact enumeration is deterministic.
- File/directory collisions, duplicate entries, file ancestors, root escape, unsafe archive
  members, and cache digest mismatches must be rejected or diagnosed structurally.
- Permission/I/O failures are not equivalent to missing data.
- Do not generate fallback file URIs for paths that cannot be represented.
- Preserve unrelated and untracked user files.

## Commands

```sh
cargo fmt --all --check
ast-grep test --skip-snapshot-tests
ast-grep scan --error
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings

cargo check -p jals-exec --no-default-features
cargo check -p jals-exec --features tokio
cargo check -p jals-storage --no-default-features
cargo check -p jals-storage --no-default-features --features std-io
cargo check -p jals-classpath --no-default-features
cargo check -p jals-project --no-default-features
cargo check -p jals-project --all-features
cargo check -p jals-build --no-default-features
cargo check -p jals-frontend
cargo check -p jals-javac --no-default-features
cargo check -p jals-classpath --no-default-features --target wasm32-unknown-unknown
cargo check -p jals-classpath --no-default-features --features archive --target wasm32-unknown-unknown
cargo check -p jals-project --no-default-features --target wasm32-unknown-unknown
cargo check -p jals-frontend --target wasm32-unknown-unknown
cargo check -p jals-javac --no-default-features --target wasm32-unknown-unknown
cargo build -p jals-playground --target wasm32-unknown-unknown
```

CI runs clippy, test, build, and build-release on linux, macOS, Windows **and** wasm; hawk runs on
linux and macOS only, because the tool publishes no Windows build and the from-source one answers
wrong (see the job's comment in `.github/workflows/ci.yml`). The three host platforms take
`--workspace`; the wasm cells take a package set, and the sets are defined once in
`.github/workflows/ci.yml`'s `env` block (`WASM_PACKAGES`, `WASM_CORE_PACKAGES`,
`WASM_TEST_PACKAGES`) rather than per job. Two consequences for local work:

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

Every project under `examples/` is a CI cell of its own (`example (<name>)`), running what its README
tells a reader to run: `jals build`, then `jals fmt --check` and `jals lint` over the example's
**tracked** `.java` files. Tracked is what separates authored source from published output — a build
script's publication into a source root is untracked by construction (that is what each example's
`.gitignore` reserves), so the gate never scores a decompiled skeleton as something someone wrote.
Two consequences for an example:

- A `tasks.project_jar` example needs its JAR, and a JAR is a binary, so none is committed:
  `examples/scripts/gen-vendor-jars.sh` writes the two the `task_dependency` and
  `task_source_archive` examples read, and CI runs it before every cell.
- `minecraft` is the one example whose `jals build` is *not* required to succeed, because its
  published skeleton tree is documented not to compile (`examples/minecraft/README.md`
  §Compile-safety). That cell asserts the pipeline instead — fetch → nested extract → remap →
  decompile → publish — by requiring all three publication roots to come out non-empty, which is a
  statement only a run that reached the last step can make.

Run `cargo run -p xtask -- codegen` after changing `jals-syntax/java.ungram`, and commit generated
AST changes with the grammar change.
