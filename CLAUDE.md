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
  stored-only writer for jar remap/merge; the `zip` crate is a dev-only fixture oracle).
  Mojang/ProGuard mappings parsing, hierarchy-aware jar remapping, and compile-oriented jar
  decompilation into source trees live under `archive` too. HTTP/local locator lowering is in its
  native adapter.
- `jals-project`: transitive path/Git/JAR project-graph discovery, stable node identity,
  dependency-first preprocessing, and artifact-only projection into `jals-classpath`. The portable
  memory graph operates on one captured `CodeTree`; only the `native` adapter may acquire host path
  trees or temporary Git checkouts. Dependency snapshots are immutable and must never receive
  generated output.
- `jals-exec`: the execution context (`Exec`, fan-out, yields, runtime adapters). Only its
  `tokio`-feature module may name tokio; the portable core is `no_std`.
- `jals-editor`: protocol-neutral workspace and query facade over `ProjectStorage`; file identity is
  `FileKey`, and source/config invalidation follows storage revisions.
- `jals-build`: portable target/scaffold planning plus native JDK/process adapters. OS arguments,
  environment variables, and classpath separators stay in native/host code.
- `jals-cli`: the host boundary from clap `PathBuf` values to `NativeStorage` and typed keys.
- `jals-lsp`: the only URI↔native-root adapter; watched-file notifications call `refresh()`.
- `jals-playground`: one `MemoryStorage` aggregate backs sidebar, editor overlays, and dependency
  artifacts.
- `jals-classfile`, `jals-hir`, `jals-syntax`, `jals-fmt`, `jals-lint`, `jals-decompile`: portable
  domain crates; do not add host filesystem APIs.
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
- `serde` stays `default-features = false, features = ["derive", "alloc"]`.
- `toml` stays `default-features = false, features = ["parse", "serde"]`.

## Invariants

- Parsing is lossless and never panics on malformed input.
- Formatting is idempotent and preserves the significant token sequence unless an explicitly
  configured text-normalization rule applies.
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
cargo check -p jals-classpath --no-default-features --target wasm32-unknown-unknown
cargo check -p jals-classpath --no-default-features --features archive --target wasm32-unknown-unknown
cargo check -p jals-project --no-default-features --target wasm32-unknown-unknown
cargo build -p jals-playground --target wasm32-unknown-unknown
```

Run `cargo run -p xtask -- codegen` after changing `jals-syntax/java.ungram`, and commit generated
AST changes with the grammar change.
