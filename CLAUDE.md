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
- Crate graph: `jals-cli` → `{jals-fmt, jals-lint, jals-lsp, jals-build, jals-hir, jals-syntax}`;
  `jals-lsp` → `{jals-fmt, jals-lint, jals-hir, jals-syntax}`; `jals-lint` → `{jals-hir, jals-syntax}`;
  `jals-hir`/`jals-fmt` → `jals-syntax`.
  `jals-build` has no `jals-syntax` dependency (it only orchestrates `javac`/`java`).
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
| Name resolution + types (HIR) | `jals-hir/src/` | Three layers, all pure/wasm-compatible/never-panic. **File-local resolution** (`resolve`/`resolve_node` → `Resolved`: `defs`, `scopes`, `references`): two-pass — `resolve/build.rs` builds scopes and registers defs (recording each reference with its scope), then each reference is looked up its scope chain; value/method/type namespaces, sequential (block/for/resources) vs. hoisting scopes. **Project index** (`ProjectIndex::build` over many `(FileId, SyntaxNode)`, or `build_with_stdlib` to also fold in embedded `java.lang`/`java.util` signature stubs as `ItemOrigin::Stdlib` items — see `stdlib.rs`): cross-file type-name + member resolution. **Type inference/checking** (`infer`/`infer_node` → `TypeInference`; `type_mismatches`): a structural `Ty` per expr/decl and assignment-conversion checks (`Ty::is_assignable_to`, return/initializer/call-argument, overload resolution). Conservative — un-inferable types are `Ty::Unknown` and never flagged. Generics **are** modelled: a class type carries its type arguments and member access substitutes them down the supertype chain (`List<String>.get(0)` → `String`), and assignment enforces same-nominal type-argument invariance (`List<String>` ↛ `List<Object>`). Standard-library types resolve through the stubs (precise for inference/hover) but are treated leniently in type checking — a stub `Ty` is demoted to external for assignability and counted as an incomplete method set — so the deliberately-partial stubs (autoboxing, an omitted supertype) never yield a false mismatch. Still un-modelled: target-typed forms (lambdas/method refs/switch exprs → `Unknown`), type-parameter bounds, wildcard variance, cross-nominal type-argument propagation, generic-method inference, and the real JDK classpath beyond the stubs (other external types by name only). |
| Linter | `jals-lint/src/` | Rule registry (`rules/mod.rs`, `RuleMeta` with a `Checker` per rule — syntactic, resolution-based, or index-aware) over the CST; `lint_source`/`lint_node` return byte-range `Diagnostic`s. File-local name resolution is computed at most once per lint and shared across rules. `jalslint.toml`, kebab-case keys, all optional. Pure, wasm-compatible. The `unused-local` and `type-mismatch` rules consume `jals-hir`; `lint_parse_with_index` runs `type-mismatch` against a caller-supplied `ProjectIndex` for cross-file checks. |
| Build/compile | `jals-build/src/` | `jals.toml` (`Manifest`) parsing + validation (`Manifest::validate`) + a pure `javac`/`java` invocation builder (`build_invocation`/`run_invocation`) + run-target resolution (`resolve_run_target`, picking the `main-class` from `[[bin]]`/`default-run`/`[run] main-class`) + clean-path resolution (`clean_paths`, for `jals clean`) + project scaffolding (`scaffold`, for `jals init`). Pure lib (serde/toml, no `std::process`/`std::fs`), so wasm-compatible; `jals-cli` walks sources, spawns the tools, removes the build output, and writes the scaffold files. `jals-build/README.md` has the full manifest reference and the Cargo-for-Java roadmap. |
| CLI | `jals-cli/src/main.rs` | `jals fmt`/`jals lint`/`jals lsp`/`jals build`/`jals run`/`jals clean`/`jals init`; config discovery memoized per directory. `jals lint` builds a `ProjectIndex` over the files being linted (the host owns the I/O) and runs the index-aware `type-mismatch` for cross-file checks. |
| LSP | `jals-lsp/src/` | `async-lsp` server (`jals lsp`): diagnostics (syntax + `jals-lint` + cross-file unresolved-type / type-mismatch via `jals-hir`), document symbols, formatting, hover, go-to-definition, find-references, document highlight. `Workspace` (`state.rs`) holds a per-project `ProjectIndex` over every source file. Pure handlers + UTF-16 `LineIndex`. Host-only (tokio/stdio). |
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
5. **`wasm32` compatibility.** Everything except `jals-cli` and `jals-lsp` must build for
   `wasm32-unknown-unknown` (both are host-only: `jals-cli` does filesystem/process work,
   `jals-lsp` uses tokio/stdio). Do not add non-wasm-compatible deps or `std::fs`/process/IO
   usage to `jals-syntax`, `jals-fmt`, or `jals-build`; keep that work in `jals-cli`/`jals-lsp`
   (`jals-build` only *plans* `javac`/`java` commands as pure data — `jals-cli` spawns them).

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
