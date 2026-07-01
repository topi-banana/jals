# CLAUDE.md

Guidance for Claude Code (and other agents) working in this repository.

## What this is

`jals` is a Rust workspace providing Java tooling built on a **lossless, error-resilient**
syntax tree. A hand-written lexer and `rowan` CST parser (`jals-syntax`) feed a Wadler/Prettier
pretty-printer (`jals-fmt`), exposed through the `jals` CLI (`jals-cli`). An LSP server
(`jals-lsp`, run via `jals lsp`) and a linter (`jals-lint`) are other consumers. Name resolution and
type analysis (`jals-hir`) are the foundation for semantic tooling — go-to-definition, unused-binding
lints, and type inference/checking.

- Edition 2024, resolver 3, workspace version `0.1.0`. Needs Rust 1.85+.
- Crate graph: `jals-cli` → `{jals-fmt, jals-lint, jals-lsp, jals-build, jals-hir, jals-classpath, jals-syntax}`;
  `jals-lsp` → `{jals-fmt, jals-lint, jals-hir, jals-classpath, jals-classfile, jals-syntax}`;
  `jals-lint` → `{jals-hir, jals-syntax}`; `jals-hir` → `{jals-syntax, jals-classfile}`; `jals-fmt` → `jals-syntax`.
  `jals-build` has no `jals-syntax` dependency (it only orchestrates `javac`/`java`).
  `jals-classfile` is a leaf (only `serde`): a complete, byte-exact read/write model of the JVM
  `.class` format (JVMS ch. 4), feeding `jals-hir`'s classpath bridge.
  `jals-classpath` is a **host-only** crate (`jals-build` + `jals-classfile` + `walkdir` + `zip` +
  `reqwest`, shells out to `git`): it reads a project's classpath `.class` files out of jars and
  directories and parses them, and **resolves `[dependencies]`** (`resolve_dependencies` — local
  `file://`/path jars, and remote `https://` jars downloaded with `reqwest` into a `target/jals/deps`
  cache), the I/O half of that bridge; it also **resolves + extracts each dependency's optional
  `sources` jar** (`resolve_project_sources` → `extract_sources`, the library `.java` for editor
  go-to-definition) **and resolves the `git`/`path` source forms** (`resolve_project_source_deps` —
  clones each `git` repo into `target/jals/deps/git`, checks out its `branch`/`tag`/`rev`, reads each
  `path` in place, and returns the located `.java`) — consumed by `jals-cli` (`jals lint`/`build`/`run`,
  jars only) and `jals-lsp` (the `sources` jars **and** the `git`/`path` source `.java` are LSP-only).
  `jals-playground` is a separate
  Yew/Trunk browser app that runs `jals-fmt`/`jals-syntax` in the browser. It targets `wasm32`
  but also compiles on the host, so `--workspace` build/clippy/test all include it.

## Architecture map

| Area | Where | Notes |
| --- | --- | --- |
| Lexer | `jals-syntax/src/lexer.rs`, `token.rs` | Hand-written, lossless; trivia are real tokens. Context-sensitive keywords lexed as `IDENT`, promoted later. |
| Token/node kinds | `jals-syntax/src/syntax_kind.rs` | Unified `SyntaxKind` (u16) for `rowan`; `TokenKind` is terminals only. |
| Parser | `jals-syntax/src/parser/` | Recursive descent. `grammar.rs` is the rules; `mod.rs` the core; `event.rs`/`sink.rs` build the green tree. Error-resilient. |
| Typed AST | `jals-syntax/java.ungram`, `jals-syntax/src/ast/` | Zero-cost newtype views over the CST. `ast/generated.rs` is rendered from `java.ungram` by `cargo run -p xtask -- codegen` (committed; CI checks drift); bespoke accessors live in `ast/ext.rs`. Accessors return `Option`/iterators, never panic. |
| Formatter pipeline | `jals-fmt/src/lower.rs` → `doc.rs` → `render.rs` | CST → `Doc` IR → text. |
| Import layout | `jals-fmt/src/imports.rs` | Pure ordering/grouping of the leading import run (`reorder-imports` / `group-imports`) + its `Doc` emission. |
| Modifier layout | `jals-fmt/src/modifiers.rs` | Pure canonical reordering of a `MODIFIERS` node's keyword modifiers (`reorder-modifiers`), annotations hoisted to the front, + its `Doc` emission. |
| Comment attachment | `jals-fmt/src/comments.rs` | Anchors each comment to a significant token exactly once. |
| Config | `jals-fmt/src/config.rs` | `jalsfmt.toml`, kebab-case keys, all optional. |
| Name resolution + types (HIR) | `jals-hir/src/` | Three layers, all pure/wasm-compatible/never-panic. **File-local resolution** (`resolve`/`resolve_node` → `Resolved`: `defs`, `scopes`, `references`): two-pass — `resolve/build.rs` builds scopes and registers defs (recording each reference with its scope), then each reference is looked up its scope chain; value/method/type namespaces, sequential (block/for/resources) vs. hoisting scopes. **Project index** (`ProjectIndex::builder(…).build()` over many `(FileId, SyntaxNode)`, or `.with_stdlib()` to also fold in embedded `java.lang`/`java.util` signature stubs as `ItemOrigin::Stdlib` items — see `stdlib.rs`): cross-file type-name + member resolution. **Type inference/checking** (`infer`/`infer_node` → `TypeInference`; `type_mismatches`): a structural `Ty` per expr/decl and assignment-conversion checks (`Ty::is_assignable_to`, return/initializer/call-argument, overload resolution). Conservative — un-inferable types are `Ty::Unknown` and never flagged. Generics **are** modelled: a class type carries its type arguments and member access substitutes them down the supertype chain (`List<String>.get(0)` → `String`), and assignment enforces same-nominal type-argument invariance (`List<String>` ↛ `List<Object>`). Standard-library types resolve through the stubs (precise for inference/hover) but are treated leniently in type checking — a stub `Ty` is demoted to external for assignability and counted as an incomplete method set — so the deliberately-partial stubs (autoboxing, an omitted supertype) never yield a false mismatch. **Classpath bridge** (`classpath.rs`, `ProjectIndex::builder().with_classpath()`): external-library types can be folded in from their compiled `.class` files — each `jals_classfile::ClassFile` is lowered to an `ItemOrigin::Classpath` item with its members, supertypes, and generic signatures decoded through `jals-classfile`, so e.g. a loaded `java/util/List` resolves `List<String>.get(0)` to `String`. Unlike a stub, a classpath type's member set is complete, so it is treated precisely (not demoted). The host supplies the already-parsed class files (it owns the JAR/file I/O); `jals-classpath` reads them from a project's `[build] classpath` jars/dirs **and from `[dependencies]`** (local `file://` jars and remote `https://` jars it downloads), wired into `jals lint`/`build`/`run` and the LSP. **Source-location overlay** (`SourceLocations`, `index_source_locations`, `.with_source_locations()`): a classpath item/member normally has no host-openable source (reserved `FileId`, `0..0` range), but when a dependency's `sources` jar is available the host feeds the extracted library `.java` trees in, and each `.class`-derived `Item`/`Member` gets a `source_location: Option<(FileId, Range)>` pointing at the matching `.java` declaration (types by FQN, members by `(fqn, name, param-count)` with a name-only fallback). Typing stays authoritative from the `.class` (`file`/`name_range` — the member-resolution context — are untouched); the overlay only adds a real go-to-definition target, so `definition_at` navigates a classpath type into its source (and the LSP navigates members likewise). **Source-dependency folding** (`.with_source_deps()`): the `.java` of a `git`/`path` `[dependencies]` entry is indexed as `ItemOrigin::Source` items — typed from real source (complete member set, treated precisely like a classpath type, *not* leniently like a stub) and navigable at their own `file`/`name_range` (no overlay needed), so the project resolves their types for inference/hover/completion and go-to-definition lands in the real source; on a fully-qualified-name clash a project type wins over a source type wins over a stub. They are external — the host never lints or renames them (`definition_at` treats `Source` like `Project` for navigation; the LSP's rename/find-references are restricted to `Project`-origin items). Still un-modelled: target-typed forms (lambdas/method refs/switch exprs → `Unknown`), type-parameter bounds, wildcard variance, cross-nominal type-argument propagation, generic-method inference, **Maven-coordinate dependency resolution** (`[dependencies]` takes explicit `{ jar }`, `{ git, branch/tag/rev, dir }`, and `{ path, dir }` forms so far — no `group:artifact:version` / transitive resolution / lockfile yet), and **JDK** classpath *discovery* (locating the JDK's `jimage`/`modules` is host-side and not yet wired — the embedded `java.lang`/`java.util` stubs stand in for it; `[build] classpath` jars/dirs and `[dependencies]` jars **are** now loaded, and `git`/`path` source deps folded into the LSP index). |
| Linter | `jals-lint/src/` | Rule registry (`rules/mod.rs`, `RuleMeta` with a `Checker` per rule — syntactic, resolution-based, index-aware, or **edition-gated** (`Checker::Versioned`)) over the CST; `lint_source`/`lint_node` return byte-range `Diagnostic`s. File-local name resolution is computed at most once per lint and shared across rules. `jalslint.toml`, kebab-case keys, all optional. Pure, wasm-compatible. The `unused-local` and `type-mismatch` rules consume `jals-hir`; `lint_parse_with_index` runs `type-mismatch` against a caller-supplied `ProjectIndex` for cross-file checks. The `compact-source-file` rule is edition-gated: the host injects the project's Java edition as `Config::target_java_version` (from `[package] edition`, **not** a `jalslint.toml` key — it is `#[serde(skip)]`), and the rule flags a compact source file's top-level members (a top-level `main` / field / method, JEP 512) as an error when the target is below Java 25 (where the feature is only a preview); `None`/Java 25+ never fires. |
| Classfile (read/write) | `jals-classfile/src/` | A complete, **byte-exact round-trip** model of the JVM `.class` format (JVMS ch. 4): a hand-written big-endian codec (`ClassFile::read`/`write` over `bytes.rs`, no external byte crate) into a full struct/enum model — constant pool (1-based, `Long`/`Double` two-slot quirk absorbed in `constant_pool.rs`), every standard `attribute.rs` attribute (incl. `stackmap.rs`, annotations) with an `Unknown` raw-bytes fallback, and decoded bytecode (`instruction.rs`, all opcodes + `wide` + switch alignment). Counts / byte-lengths are derived on write, never stored. Every type also derives `serde::{Serialize, Deserialize}` (the struct⇄JSON medium; serde is **not** the binary codec). `descriptor.rs` (§4.3) / `signature.rs` (§4.7.9) parse field/method descriptors and generic signatures. Pure, wasm-compatible, never panics on bad bytes (returns `Err`). Consumed by `jals-hir`'s classpath bridge; the host reads `.class` bytes from disk/JARs via `jals-classpath`. |
| Classpath loading | `jals-classpath/src/lib.rs`, `resolve.rs` | **Host-only** (uses `std::fs` + `walkdir` + `zip` + `reqwest`, so *not* wasm). `load_classpath(entries)` turns a project's resolved classpath entries (`Manifest::classpath_entries`) into parsed `jals_classfile::ClassFile`s for the HIR bridge: a **directory** is walked for `*.class`, a **jar/zip** has its `*.class` members inflated and read, a bare **`.class`** file is read directly. `resolve_dependencies(sources, cache_dir)` (`resolve.rs`) resolves a manifest's `[dependencies]` (classified purely by `Manifest::dependency_sources`) to local jar paths: a `file://`/path source is confirmed to exist, an `https://`/`http://` source is downloaded with `reqwest`'s **blocking** client into `cache_dir` (`<name>-<url-hash>.jar`, skip-if-exists, atomic `.part`→rename), then the host appends those jars to the classpath fed to `load_classpath`. Error-resilient — an unreadable jar / corrupt class / missing entry / failed download becomes a `Warning` and is skipped, never aborting. `reqwest::blocking` panics inside a Tokio runtime, so `jals-lsp` calls `resolve_dependencies` on a dedicated `std::thread` (`jals-cli` is sync and calls it directly). The pure analysis layers (`jals-hir`) only ever see the already-parsed class files. **Bundled jars** (`recursive = true` on a `jar` dependency): `resolve_project_dependencies` makes a second pass over the `recursive`-flagged jars (`Manifest::recursive_jar_dependencies`), and `extract_nested_jars(jar, dest)` recursively unpacks each one's `*.jar` members — the nested jars a fat jar bundles (e.g. `BOOT-INF/lib/*.jar`) that `load_classpath` would otherwise skip — into `target/jals/deps/nested/<jar>-<hash>/…` (skip-if-exists, atomic write, zip-slip-sanitized, depth-capped), at any depth, and appends them to the classpath, so the bundled libraries are folded into both compile (`jals build`/`run`) and analysis (`jals lint`/LSP) through the shared `resolve_project_dependencies`. **Sources jars** (LSP-only, for go-to-definition into library source): `resolve_project_sources` resolves each dependency's optional `sources` jar the same way (`Manifest::dependency_source_jars` → `resolve_dependencies`), then `extract_sources(jars, dest)` inflates its `*.java` members into `target/jals/deps/sources/<jar>-<hash>/…` (skip-if-exists, atomic write, zip-slip-sanitized), returning the extracted `.java` paths the host registers as navigation files. **Decompiled skeletons** (LSP-only, the fallback when a jar ships *no* `sources`): `synthesize_classpath_sources(classes, root, warn)` writes a signature-only `.java` **skeleton** for the loaded classpath `.class` files — `skeleton.rs` is the pure renderer (`skeleton_groups` plans one `SkeletonGroup` per top-level type, each rendered on demand so a skeleton already cached on disk is never re-rendered): one file per top-level type (nested types inlined so dotted FQNs line up), every type/member *declaration* but no method bodies, driven off `jals_classfile`'s descriptors/signatures/flags exactly like a `jals-hir` stdlib stub — into `target/jals/deps/decompiled/<pkg>/<Outer>.java` (skip-if-exists, atomic write); the host appends these to its navigation files **after** the real `-sources.jar` `.java`, so the first-declaration-wins `SourceLocations` overlay keeps real source authoritative and skeletons only fill the gaps, making go-to-definition land somewhere for *any* library type. **Source dependencies** (`git`/`path`, also LSP-only): `resolve_project_source_deps` (`resolve.rs`, classified by `Manifest::dependency_source_dirs`) clones each `git` repo (`git clone` + `git checkout <branch/tag/rev>` via `std::process`, into `target/jals/deps/git/<name>-<hash(url,ref)>`, skip-if-exists, `.part`→rename) or reads each `path` in place, locates the `.java` source root (explicit `dir`, else `src/main/java` → `src` → the root), and returns every `*.java` under it for the host to fold into the index as `Source` types. `git` is a subprocess (not `reqwest`), so it does not itself need a dedicated thread, but `jals-lsp` resolves it on the same off-Tokio thread as the jar/sources downloads. |
| Build/compile | `jals-build/src/` | `jals.toml` (`Manifest`) parsing + validation (`Manifest::validate`) + a pure `javac`/`java` invocation builder (`build_invocation`/`run_invocation`) + run-target resolution (`resolve_run_target`, picking the `main-class` from `[[bin]]`/`default-run`/`[run] main-class`) + the project's Java **edition** (`[package] edition`, the `Edition` enum `java24`/`java25` with `feature_version()` — a fixed serde-variant set, so an unknown value is a TOML parse error, no `validate` check; it is an *analysis-only* language-feature gate consumed by the linter/LSP and is **not** passed to `javac` — the compile knobs stay `[build] release`/`source`/`target`) + clean-path resolution (`clean_paths`, for `jals clean`) + project scaffolding (`scaffold`, for `jals init`) + source-root / classpath path resolution (`source_roots`/`classpath_entries`, resolving `[build] source-dirs`/`classpath` against the manifest dir for the host to read) + **`[dependencies]` classification** (`Dependency` is *itself* the classification — a `#[serde(untagged)]` enum of `Jar(JarDependency)`/`Git(GitDependency)`/`Path(PathDependency)` variants, each `deny_unknown_fields`, so serde chooses the form at parse time and the structural errors — co-occurring forms `{jar,git}`, no form `{}`, a field misplaced for its form `branch`-without-`git` — fail as a TOML parse error, *not* a `DependencyError`; the value-level checks `validate` still makes are empty value / unknown URL scheme / conflicting git refs. The resolution accessors `Dependency::{jar_source, sources_source, source_dependency}` map each variant to the host-facing forms: `dependency_sources` collects the `jar` forms → `DependencySource::{Url, Path}` for the classpath, `dependency_source_jars`/`sources_source` the optional companion `sources` jar, and `dependency_source_dirs` the `git`/`path` source forms → `SourceDependency::{Git, Path}`; the `jar` form also carries an optional `recursive` bool — `recursive_jar_dependencies` names the jars whose **bundled jars** the host unpacks onto the classpath (see Classpath loading) — all pure splitting, no I/O — the host resolves/downloads/clones). Pure lib (serde/toml, no `std::process`/`std::fs`/network), so wasm-compatible; `jals-cli` walks sources, spawns the tools, removes the build output, and writes the scaffold files, and `jals-classpath` does the dependency downloads/clones. `jals-build/README.md` has the full manifest reference and the Cargo-for-Java roadmap. |
| CLI | `jals-cli/src/main.rs` | `jals fmt`/`jals lint`/`jals lsp`/`jals build`/`jals run`/`jals clean`/`jals init`; config discovery memoized per directory. `jals lint` builds a `ProjectIndex` over the files being linted (the host owns the I/O), folding in the discovered project's `[build] classpath` `.class` files **and resolved `[dependencies]` jars** (via `jals-classpath`, best-effort — a missing manifest/classpath just means source+stdlib only) so external library types resolve, and runs the index-aware `type-mismatch` for cross-file checks. It also reads the discovered project's `[package] edition` (`load_project_edition`) and sets it on the lint `Config` (`target_java_version`) so the edition-gated `compact-source-file` rule runs. `jals build`/`run` resolve `[dependencies]` (`resolve_project_dependencies` → downloaded/local jars) and pass them as the `extra_classpath` to `build_invocation`/`run_invocation`, so `javac`/`java` see them too. |
| LSP | `jals-lsp/src/` | `async-lsp` server (`jals lsp`): diagnostics (syntax + `jals-lint` + cross-file unresolved-type / type-mismatch via `jals-hir`), document symbols, formatting, hover, go-to-definition, find-references, document highlight. `Workspace` (`state.rs`) holds a per-project `ProjectIndex` over every source file, with the project's `[build] classpath` `.class` files **and resolved `[dependencies]` jars** (loaded once via `jals-classpath`; the `reqwest` download runs on a dedicated thread to stay off the Tokio runtime) folded in. It also registers each dependency's **extracted `sources` `.java`** — and, for a jar with no `sources`, a **synthesized skeleton `.java`** (`jals_classpath::synthesize_classpath_sources` from the classpath `.class`, appended after the real sources so real source wins) — as read-only `library_files` (a `FileId` above `LIBRARY_FILE_BASE = 1<<31`, disjoint from project files) plus a cached `SourceLocations`, so **go-to-definition lands in a classpath type/member's library source (real or synthesized) for any library type**, and each **`git`/`path` source dependency's `.java`** as `source_dep_files` (a third id space above `SOURCE_DEP_FILE_BASE = (1<<31)+(1<<30)`) which — unlike `library_files` — *are* index inputs (folded into `builder().with_source_deps()` as `Source`-origin types, so the project resolves their types) as well as navigation targets. `ws_file` routes a target id to the project / `library_files` / `source_dep_files` vec by range (a no-source classpath member's reserved id is rejected, never panicking — and `item_references`/`is_renamable` go through `ws_file` / restrict to `Project`-origin so a find-references/rename on an external type can't index out of bounds). Library and source-dep files are never linted. The `Workspace` also records the project's `[package] edition` (`target_java_version`, read from the manifest before it moves into the resolver thread); `publish_diagnostics` injects it into the lint `Config` for the owning file so the edition-gated `compact-source-file` rule runs (files under no manifest stay `None`). Pure handlers + UTF-16 `LineIndex`. Host-only (tokio/stdio). |
| Playground | `jals-playground/` | Yew (CSR) browser app served by Trunk (`Trunk.toml`, tailwind); compiles to `wasm32`. Runs the syntax/formatter in-browser. |

## Commands

```sh
cargo build --workspace
cargo test  --workspace --all-features
cargo run -p jals-cli -- fmt <paths>       # or: echo '...' | cargo run -p jals-cli -- fmt
cargo run -p jals-cli -- init [path]       # scaffold a new jals.toml project (Main.java, .gitignore)
cargo run -p jals-cli -- build [--dry-run] # compile a jals.toml project with javac (--dry-run prints the command)
cargo run -p jals-cli -- run               # compile then run the project's [run] main-class with java
cargo run -p jals-cli -- clean [--dry-run] # remove the project's build output (the classes-dir)
cargo run -p jals-cli -- lsp               # run the language server over stdio (for editors)
cargo run -p xtask -- codegen              # regenerate jals-syntax/src/ast/generated.rs from java.ungram
(cd jals-playground && trunk serve)        # run the browser playground (needs trunk + the wasm32 target)
```

Before considering a change done, run the **exact CI checks** (see
`.github/workflows/ci.yml`) — clippy is `-D warnings`, so warnings fail:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
taplo fmt --check --diff
cargo machete                                                # no unused deps
cargo build --release --target wasm32-unknown-unknown -p jals-syntax
cargo run -p xtask -- codegen --check                        # generated AST is up to date
```

## Invariants — do not break these

These are enforced by unit and `proptest` property tests (`jals-fmt/tests/invariants.rs`,
plus lexer/parser property tests). A change that violates one is wrong, not the test:

1. **Lossless lexer.** Concatenating every token's text reproduces the input exactly.
2. **Never panics.** The lexer and parser must not panic on *any* input (including arbitrary
   Unicode); unmatched bytes become `SyntaxKind::ERROR`. The formatter never panics either.
3. **Always a tree.** The parser recovers from errors and records `SyntaxError`s rather than
   aborting.
4. **Formatter fidelity.** Comments are never dropped, and formatting is idempotent
   (`format(format(x)) == format(x)`). By default the significant-token *sequence* (non-trivia
   tokens) is preserved exactly. Seven options, each off by default, relax this:
   - **`reorder-imports`** may reorder import declarations. The significant-token *multiset* is
     still preserved (none added, dropped, or altered), and each comment stays glued to its
     anchoring token (so a comment moves, with its token, when that token is reordered).
   - **`group-imports`** may reorder import declarations into prefix-defined groups separated by
     blank lines (it overrides `reorder-imports`). The *multiset* is preserved and each comment
     stays glued to its import, exactly as for `reorder-imports`.
   - **`reorder-modifiers`** may reorder a declaration's keyword modifiers into canonical order
     and hoist its annotations to the front. The *multiset* is preserved and each comment stays
     glued to its modifier, exactly as for `reorder-imports`.
   - **`trailing-comma`** (any value other than `preserve`, the default) may add or drop the
     single trailing comma of an **array initializer** — the only Java list (besides enum
     constant lists) where that token is legal. No other token is touched, and a dropped comma
     that carries a comment is kept, so comments are never lost.
   - **`hex-literal-case`** (any value other than `preserve`, the default) may rewrite the case
     of the hex digits of an integer / float literal (`0xCafe` → `0xCAFE` / `0xcafe`). The token
     *kind* sequence is preserved exactly — only a hex literal's *text* changes, and only the
     mantissa digits (the `0x` prefix, `p` exponent, and `l`/`f`/`d` suffix are untouched).
   - **`float-literal-trailing-zero`** (any value other than `preserve`, the default) may add or
     strip the trailing zero of a **decimal** float literal (`1.0` ↔ `1.`). The token *kind*
     sequence is preserved exactly — only an in-scope decimal float's *text* changes; a non-zero
     fraction (`1.50`), a leading-dot float (`.5`), a dotless float (`1e10`), a hex float
     (`0x1.0p3`), and integers are untouched, as are the value, suffix, and exponent.
   - **`literal-suffix-case`** (any value other than `preserve`, the default) may rewrite the case
     of a numeric literal's trailing type suffix — the integer `l`/`L` (`123l` ↔ `123L`) or the
     float `f`/`F`/`d`/`D` (`1.5f` ↔ `1.5F`). The token *kind* sequence is preserved exactly —
     only that single trailing suffix letter's *text* changes; the value, radix prefix, mantissa,
     and exponent are untouched, and an integer's trailing `f`/`d` hex *digit* (`0xabcdef`) is
     never a suffix.
   Idempotency holds in every case. With all seven at their defaults (`reorder-imports`,
   `group-imports`, and `reorder-modifiers` off, `trailing-comma = preserve`,
   `hex-literal-case = preserve`, `float-literal-trailing-zero = preserve`,
   `literal-suffix-case = preserve`), the exact-sequence guarantee is in full force.
5. **`wasm32` compatibility.** Everything except `jals-cli`, `jals-lsp`, and `jals-classpath` must
   build for `wasm32-unknown-unknown` (all three are host-only: `jals-cli` does filesystem/process
   work, `jals-lsp` uses tokio/stdio, `jals-classpath` does `std::fs` + `zip` jar I/O + `reqwest`
   dependency downloads). Do not add non-wasm-compatible deps or `std::fs`/process/network/IO usage
   to `jals-syntax`, `jals-fmt`, `jals-build`, `jals-hir`, or `jals-classfile`; keep that work in
   `jals-cli`/`jals-lsp`/`jals-classpath` (`jals-build` only *plans* `javac`/`java` commands and
   *classifies* `[dependencies]` specs as pure data — `jals-cli`/`jals-lsp` spawn the tools and the
   `jals-classpath` downloader; `jals-classfile` is a pure byte-level codec — `jals-classpath` reads
   the bytes off disk/out of jars/over the network and hands the parsed class files to `jals-hir`).
   CI builds `jals-syntax`, `jals-classfile`, and `jals-hir` for `wasm32` in the `build` matrix.

When touching the lexer, parser, or formatter, prefer adding a snapshot test
(`expect-test`) and confirm the property tests still pass.

## Conventions

- **Code comments and docs are written in English.** (Some older `jals-syntax` files still
  carry Japanese comments from earlier work; new and edited code should be English.)
- Match the surrounding style: `rowan`/rust-analyzer-flavored naming for syntax code,
  `clippy`-clean, `rustfmt`-clean.
- Config keys are kebab-case and every key is optional with a default in `Config::default`.
- Keep `SyntaxKind` variants in sync between the enum, the `From<TokenKind>` mapping, and the
  parser/AST that construct them.
- In `jals-playground`, implement every Yew component as a `struct` definition plus an
  `impl yew::Component` block (struct components) — do not use `#[function_component]` or
  other function-component styles.

## Repository notes

- The untracked `check` file in the repo root is a **local ELF build artifact — never commit
  it** (and do not add binaries to git generally). Only `/target` is gitignored.
- Run git operations (commit, push, branch) **only when explicitly asked.**
- There is no `LICENSE` file yet; do not assume one.
