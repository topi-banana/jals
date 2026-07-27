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
- `jals-config`: pure schemas and revision-aware config discovery over `ProjectView`.
- `jals-classpath`: resolution over project bytes and cache artifacts. The in-house zip reader is
  isolated in `zip.rs` behind `archive` (portable, `no_std`, over the async io seam; also a
  stored-only writer for jar remap/merge; the `zip` crate is a dev-only fixture oracle). `jar.rs`
  is the only public surface over that writer: `JarPackage::write` packages compiled classes,
  generating the `META-INF/MANIFEST.MF` a jar needs (first member, CRLF, 72-byte wrapped) and
  keeping `StoredZip`/`WriteMember` sealed.
  Mojang/ProGuard mappings parsing, hierarchy-aware jar remapping, and compile-oriented jar
  decompilation into source trees live under `archive` too. HTTP/local locator lowering is in its
  native adapter.
- `jals-project`: transitive path/Git/JAR project-graph discovery, stable node identity,
  dependency-first preprocessing, and artifact-only projection into `jals-classpath`. The portable
  memory graph operates on one captured `CodeTree`; only the `native` adapter may acquire host path
  trees or temporary Git checkouts. Dependency snapshots are immutable and must never receive
  generated output: a dependency's build tasks run under `BuildTaskHost::Snapshot`, so their JARs
  and declared source trees are projected into the *consumer's* artifact cache (classpath, and
  navigation-only `library_source_artifacts`) instead of being published to the project they were
  declared in. Each such execution is memoized in `CacheNamespace::BuildTaskState` under the node
  identity, plan digest, and resolved features, and re-verified before reuse.
- `jals-exec`: the execution context (`Exec`, fan-out, yields, runtime adapters). Only its
  `tokio`-feature module may name tokio; the portable core is `no_std`.
- `jals-editor`: protocol-neutral workspace and query facade over `ProjectStorage`; file identity is
  `FileKey`, and source/config invalidation follows storage revisions.
- `jals-build`: portable target/scaffold planning plus native JDK/process adapters. OS arguments,
  environment variables, and classpath separators stay in native/host code. `[build] backend`
  selects what compiles the lowered tree: `javac` (a host process), `jals` (in-process, one class
  file per type), or `jals-wasm` (in-process, one WebAssembly module). Only the `javac` adapter is
  host-gated — `JalsBackend` is portable, and builds for `wasm32` like the contract it implements.
- `jals-cli`: the host boundary from clap `PathBuf` values to `NativeStorage` and typed keys. It
  also owns native-formatter-config **detection** (`migrate.rs`): portable crates cannot look at a
  filesystem, so the host decides which config file is there and reads its bytes through a
  `ProjectView`, then hands the text to `jals_fmt::import` and the result to
  `jals_fmt::generate`.
- `jals-lsp`: the only URI↔native-root adapter; watched-file notifications call `refresh()`.
- `jals-playground`: one `MemoryStorage` aggregate backs sidebar, editor overlays, and dependency
  artifacts. `compile.rs` is the *Build* pipeline — frontend seam, then `JalsBackend`, then
  `JarPackage` — taking sources as `(path, text)` and returning bytes, so it is host-testable and
  cannot reach the DOM; `download.rs` is the browser-only shim that saves those bytes. It honours
  `[build] backend`, and passes an empty classpath exactly as `jals-cli` does.
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
- Formatting is idempotent. It preserves the significant token multiset except where an
  explicitly configured rule applies: the four token-changing passes — import ordering,
  unused-import removal, modifier ordering, and long-string rewrapping — plus the opt-in literal
  normalizations and brace forcing. Only unused-import removal removes tokens, and only brace
  forcing adds them.
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

Run `cargo run -p xtask -- codegen` after changing `jals-syntax/java.ungram`, and commit generated
AST changes with the grammar change.
