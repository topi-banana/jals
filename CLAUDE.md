# CLAUDE.md

Guidance for agents working in this repository.

## Architecture

`jals` is a Rust workspace for Java parsing, formatting, linting, semantic analysis, LSP, and a
Cargo-like build frontend. The lossless `jals-syntax` CST is shared by `jals-fmt`, `jals-hir`,
`jals-lint`, `jals-editor`, the CLI, the LSP, and the browser playground.

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
- `ArtifactCache` uses SHA-256 `ContentDigest` and typed `CacheKey` namespaces. Use verified
  `lookup` and write-once `publish`; never implement `contains` followed by `write`. The advisory
  locator index (`indexed_key`/`record_index`, last-writer-wins) only recovers the content half of
  a key from its provenance; bytes are still read through verified `lookup`.

Do not reintroduce `jals-fs`, `FileTree`, arbitrary string paths, path predicates, or live
filesystem reads into portable interfaces.

## Crate boundaries

- `jals-storage`: typed keys, immutable revisions, transactions, overlays, cache, memory/native
  adapters. Only `native.rs` may use `std::path`/`std::fs`.
- `jals-config`: pure schemas and revision-aware config discovery over `ProjectView`.
- `jals-classpath`: resolution over project bytes and cache artifacts. ZIP code is isolated behind
  `archive` and uses only `std::io`; HTTP/local locator lowering is in its native adapter.
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

Portable crates use `core + alloc`. Do not add source-level `extern crate alloc`; the workspace
supplies it through `.cargo/config.toml`.

- `jals-storage --no-default-features` is `no_std + alloc`.
- `jals-classpath --no-default-features` is `no_std + alloc`; `archive` introduces only `std::io`,
  and `native` introduces HTTP plus `jals-storage/std`.
- `jals-build --no-default-features` must remain a genuine portable core.
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

cargo check -p jals-storage --no-default-features
cargo check -p jals-classpath --no-default-features
cargo check -p jals-build --no-default-features
cargo check -p jals-classpath --no-default-features --target wasm32-unknown-unknown
cargo build -p jals-playground --target wasm32-unknown-unknown
```

Run `cargo run -p xtask -- codegen` after changing `jals-syntax/java.ungram`, and commit generated
AST changes with the grammar change.
