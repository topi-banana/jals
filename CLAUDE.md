# CLAUDE.md

Guidance for Claude Code (and other agents) working in this repository.

## What this is

`jals` is a Rust workspace providing Java tooling built on a **lossless, error-resilient**
syntax tree. A hand-written lexer and `rowan` CST parser (`jals-syntax`) feed a Wadler/Prettier
pretty-printer (`jals-fmt`), exposed through the `jals` CLI (`jals-cli`). An LSP server
(`jals-lsp`, run via `jals lsp`) is another consumer; a linter is an intended future one.

- Edition 2024, resolver 3, workspace version `0.1.0`. Needs Rust 1.85+.
- Crate graph: `jals-cli` → `{jals-fmt, jals-lsp}`; `jals-lsp`/`jals-fmt` → `jals-syntax`.
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
| CLI | `jals-cli/src/main.rs` | `jals fmt`/`jals lsp`; config discovery memoized per directory. |
| LSP | `jals-lsp/src/` | `async-lsp` server (`jals lsp`): diagnostics, document symbols, formatting. Pure handlers + UTF-16 `LineIndex`. Host-only (tokio/stdio). |
| Playground | `jals-playground/` | Yew (CSR) browser app served by Trunk (`Trunk.toml`, tailwind); compiles to `wasm32`. Runs the syntax/formatter in-browser. |

## Commands

```sh
cargo build --workspace
cargo test  --workspace --all-features
cargo run -p jals-cli -- fmt <paths>       # or: echo '...' | cargo run -p jals-cli -- fmt
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
   tokens) is preserved exactly. Six options, each off by default, relax this:
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
   Idempotency holds in every case. With all six at their defaults (`reorder-imports`,
   `group-imports`, and `reorder-modifiers` off, `trailing-comma = preserve`,
   `hex-literal-case = preserve`, `float-literal-trailing-zero = preserve`), the exact-sequence
   guarantee is in full force.
5. **`wasm32` compatibility.** Everything except `jals-cli` and `jals-lsp` must build for
   `wasm32-unknown-unknown` (both are host-only: `jals-cli` does filesystem/process work,
   `jals-lsp` uses tokio/stdio). Do not add non-wasm-compatible deps or `std::fs`/process/IO
   usage to `jals-syntax` or `jals-fmt`; keep that work in `jals-cli`/`jals-lsp`.

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
