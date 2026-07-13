# jals

[![CI](https://github.com/topi-banana/jals/actions/workflows/ci.yml/badge.svg)](https://github.com/topi-banana/jals/actions/workflows/ci.yml)

A Java toolchain written in Rust, built on a **lossless** syntax tree.

`jals` parses Java source into a full-fidelity concrete syntax tree (CST) — every byte,
including whitespace and comments, is preserved — and uses that tree to power source
tooling. Today it ships a code formatter, a linter, and a language server (LSP), all
backed by a shared semantic layer (`jals-hir`) that does name resolution, cross-file type
indexing, and type inference/checking — including resolving types from a project's compiled
classpath and `[dependencies]` (local, remote, and `git`/`path` jars, decompiled to readable
Java when no source jar is available). Alongside them, a Cargo-style build front end
(`jals build` / `run` / `clean` / `init`) wraps the JDK's `javac` / `java` from a `jals.toml`
manifest.

> 日本語版の README は [README_jp.md](README_jp.md) にあります。

## Highlights

- **Lossless & error-resilient.** The lexer maps every byte of input to exactly one token,
  and the parser always produces a tree — neither ever panics, even on malformed input.
- **Java 26 grammar.** Classes, interfaces, enums, records, sealed types, annotations,
  lambdas, switch expressions, patterns (including record patterns and guards), and more.
- **A formatter with guarantees.** Significant tokens are never changed, comments are never
  dropped or reordered, and formatting is idempotent (`format(format(x)) == format(x)`).
- **A linter with real semantics.** Beyond syntactic checks, `jals lint` catches unused
  locals, type mismatches, unreported checked exceptions, and dead conditionals, using name
  resolution and type inference over the CST — not just pattern matching.
- **Cargo-style Java builds.** A `jals.toml` manifest — the Java analogue of `Cargo.toml` —
  drives `jals build` / `run` / `clean` / `init`, a thin, pure `javac`/`java` wrapper that
  plans commands as data and never touches the JDK itself until the CLI runs them.
- **`wasm32`-ready core.** The syntax, formatting, linting, and semantic-analysis layers
  (`jals-syntax`, `jals-fmt`, `jals-lint`, `jals-hir`, `jals-classfile`, `jals-decompile`,
  `jals-fs`, `jals-config`) are `no_std` and build for `wasm32-unknown-unknown`, and
  `jals-classpath`'s resolution core does too (host I/O sits behind a `native` feature) — so
  the browser playground runs the same analysis stack client-side.

## Workspace layout

`jals` is a Cargo workspace of thirteen crates plus a browser playground:

| Crate | Description |
| --- | --- |
| [`jals-syntax`](jals-syntax) | A lossless Java lexer and an error-resilient CST parser (`rowan`), plus a typed AST layer over the CST. The shared foundation for every other tool. |
| [`jals-fmt`](jals-fmt) | A Wadler/Prettier-style pretty-printer driven by the `jals-syntax` CST. |
| [`jals-lint`](jals-lint) | The linter (`jals lint` via `jals-cli`): a rule registry over the CST plus `jals-hir` — unused locals, type mismatches, unreported exceptions, dead (constant) conditionals, and feature-gated preview-feature checks. |
| [`jals-hir`](jals-hir) | Name resolution, a cross-file project type index, and type inference/checking over the CST — the semantic foundation the linter and LSP build on. Also bridges in external types from a compiled classpath. |
| [`jals-classfile`](jals-classfile) | A complete, byte-exact read/write model of the JVM `.class` file format (JVMS ch. 4). |
| [`jals-decompile`](jals-decompile) | Reconstructs readable Java from a parsed `.class` file: type/signature rendering, initializers, declared `throws`, and (incrementally) full method-body decompilation from bytecode. |
| [`jals-classpath`](jals-classpath) | Resolves and loads a project's classpath and `[dependencies]` (local/remote jars, bundled/nested jars, `git`/`path` source deps) for `jals-hir`, the linter, and the LSP; falls back to decompiled `.java` skeletons when a dependency ships no sources. |
| [`jals-config`](jals-config) | The pure data model, parsing, discovery, and validation for all three config files (`jals.toml`, `jalsfmt.toml`, `jalslint.toml`). |
| [`jals-fs`](jals-fs) | A virtual filesystem trait (`FileTree`) the pure crates read config and sources through, with an in-memory implementation and a `std`-gated real-filesystem one. |
| [`jals-build`](jals-build) | A Cargo-style build orchestrator: it parses a `jals.toml` manifest and turns it into `javac`/`java` command plans, clean paths, and project scaffolding — all as pure data, with no `jals-syntax` dependency and no I/O. Backs `jals build`/`run`/`clean`/`init`. |
| [`jals-lsp`](jals-lsp) | A Language Server Protocol server (the `jals lsp` subcommand) providing diagnostics, document symbols, formatting, hover, go-to-definition, find-references, and more from the same CST and semantic layer. Host-only. |
| [`jals-cli`](jals-cli) | The `jals` command-line binary. |
| [`jals-playground`](jals-playground) | A browser playground built with [Yew](https://yew.rs) and served by [Trunk](https://trunkrs.dev). It compiles to `wasm32` and runs the syntax/formatting/analysis layers entirely in the browser. |

Two more workspace members are development-only tooling, not part of the shipped product:
[`jals-tests`](jals-tests) (corpus harnesses that check parser soundness and formatter
fidelity against real-world Java) and `xtask` (the `cargo xtask codegen` AST generator).

```
jals/
├── jals-syntax/      # lexer + CST parser + typed AST      (wasm-compatible)
├── jals-fmt/         # formatter (CST -> Doc IR -> text)   (wasm-compatible)
├── jals-lint/        # linter (rules over CST + jals-hir)  (wasm-compatible)
├── jals-hir/         # name resolution + type inference    (wasm-compatible)
├── jals-classfile/   # JVM .class read/write model         (wasm-compatible)
├── jals-decompile/   # .class -> readable Java             (wasm-compatible)
├── jals-classpath/   # classpath + dependency resolution   (wasm-compatible core)
├── jals-config/      # jals.toml/jalsfmt.toml/jalslint.toml models (wasm-compatible)
├── jals-fs/          # virtual filesystem abstraction       (wasm-compatible)
├── jals-build/       # Cargo-style javac/java build planner (wasm-compatible)
├── jals-lsp/         # LSP server (async-lsp, `jals lsp`)
├── jals-cli/         # `jals` binary
├── jals-playground/  # browser playground (Yew + Trunk -> wasm)
├── jals-tests/       # corpus test harnesses (dev-only)
└── xtask/            # codegen automation (dev-only)
```

## Installation

### Prebuilt binary (cargo-binstall)

[`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) downloads a prebuilt `jals`
binary from the GitHub release assets — no compilation needed:

```sh
cargo binstall --git https://github.com/topi-banana/jals jals-cli
```

### From source (git)

Requires a Rust toolchain with the **2024 edition** (Rust 1.85 or newer; CI builds on
stable). This compiles `jals` from the latest source:

```sh
cargo install --git https://github.com/topi-banana/jals jals-cli
```

The `jals-cli` package name is required: this is a Cargo workspace that ships several
binaries, and `cargo install --git` searches the whole repo, so it cannot pick one without
being told which package to install.

### From a local checkout

```sh
# Build the workspace
cargo build --release

# Install the `jals` binary into ~/.cargo/bin
cargo install --path jals-cli
```

The release binary is produced at `target/release/jals`.

## Usage

`jals` is invoked through subcommands: `fmt` (format source), `lint` (lint source), `lsp`
(language server), and a Cargo-style build front end — `init`, `build`, `run`, and `clean`.

### Format files in place

```sh
# Format specific files
jals fmt src/Main.java src/Util.java

# Format a directory tree (searched recursively for *.java)
jals fmt src/
```

### Format via stdin/stdout

With no paths, source is read from stdin and the formatted result is written to stdout:

```sh
cat Main.java | jals fmt
```

### Check mode (CI-friendly)

`--check` writes nothing and exits non-zero if any file would change. Files that would be
reformatted are listed on stderr:

```sh
jals fmt --check src/
```

### Treat syntax warnings as errors

The formatter is best-effort on invalid input (the CST is lossless, so it still formats).
Pass `-D warnings` to make any syntax error fail the run:

```sh
jals fmt -D warnings src/
```

### Lint files

```sh
# Lint specific files
jals lint src/Main.java src/Util.java

# Lint a directory tree (searched recursively for *.java)
jals lint src/
```

`jals lint` checks unused locals, type mismatches, unreported checked exceptions, dead
(constant-condition) branches, and feature-gated preview features, using name resolution and
type inference (`jals-hir`) — not just pattern matching over the syntax tree. If a `jals.toml`
manifest is discovered, its `[build] classpath` and `[dependencies]` are resolved so types
from external libraries are understood too. Configure via `jalslint.toml` (discovered the same
way as `jalsfmt.toml`).

### Run the language server

`jals lsp` starts an LSP server over stdio for editor integration — diagnostics (including
lint diagnostics), document symbols, hover, go-to-definition, find-references, and whole-
document formatting, all driven by the same CST and semantic layer. It is launched by an
editor rather than run by hand; see [`jals-lsp`](jals-lsp/README.md) for editor setup.

```sh
jals lsp
```

### Build Java projects (Cargo-style)

Beyond source tooling, `jals` is a small Cargo-style front end for the JDK. A
[`jals.toml`](jals-build/README.md) manifest — the Java analogue of `Cargo.toml` — declares
where sources live, where compiled classes go, which Java release to target, and the
classpath; the build subcommands turn it into `javac`/`java` invocations.

```sh
jals init my-app            # scaffold ./my-app (jals.toml, src/main/java/Main.java, .gitignore)
cd my-app
jals build                  # compile with javac
jals build --dry-run        # print the javac command without compiling
jals run                    # compile, then run the resolved entry point
jals run --bin server       # run a named [[bin]] entry point
jals run -- arg1 arg2       # ...passing args to the program
jals clean                  # remove the build output (target/classes)
```

A minimal `jals.toml` — every key is optional and defaults to the Maven-style
`src/main/java` → `target/classes` layout:

```toml
[package]
name = "hello"
version = "0.1.0"

[build]
release = 21                        # javac --release N
# source-dirs = ["src/main/java"]   # -sourcepath roots, also scanned for .java files
# classes-dir = "target/classes"    # javac -d
# classpath   = ["libs/guava.jar"]  # -classpath entries

[run]
main-class = "com.example.Main"     # entry point for `jals run` (used when no [[bin]] exists)

# Or declare several named entry points and pick one with `jals run --bin <name>`:
# [[bin]]
# name = "server"
# main-class = "com.example.Server"
```

The build crate (`jals-build`) only *plans* commands as pure data — `jals-cli` discovers the
manifest, walks the sources, and spawns the JDK tools (resolving `javac`/`java` via
`$JAVAC`/`$JAVA`, then `$JAVA_HOME/bin`, then `PATH`). See
[`jals-build/README.md`](jals-build/README.md) for the full manifest reference and the
roadmap toward a fuller Cargo-for-Java front end.

### Options

| Option | Description |
| --- | --- |
| `[PATHS]...` | Files or directories to format. Directories are searched recursively for `.java` files. No paths → stdin/stdout. |
| `--check` | Do not write anything; exit non-zero if any file would change. |
| `-D <LINT>` | Deny lints (repeatable). Only `warnings` is recognized: fail when any file has syntax warnings. |
| `--config <PATH>` | Use this config file instead of discovering `jalsfmt.toml`. |

## Configuration

The formatter reads a `jalsfmt.toml` file. The CLI discovers it by searching upward from
each formatted file's directory (or pass `--config <PATH>` to use a specific file). Every
key is optional and falls back to the default; keys use kebab-case.

```toml
# jalsfmt.toml — every key is optional; values below are the defaults.
indent-style = "space"      # "space" | "tab"
indent-width = 4
max-blank-lines = 1         # collapse runs of blank lines down to this many
line-ending = "lf"          # "lf" | "crlf"
insert-final-newline = true
max-width = 100             # code wrap target (columns)
comment-width = 80          # comment / Javadoc reflow target (columns)
```

### Example

Input:

```java
package a.b;import java.util.List;public class Foo{private int x=1;void m(int a){if(a>0){foo(a);}return;}}
```

Output of `jals fmt`:

```java
package a.b;
import java.util.List;
public class Foo {
    private int x = 1;
    void m(int a) {
        if (a > 0) {
            foo(a);
        }
        return;
    }
}
```

## Playground

`jals-playground` is a small browser app ([Yew](https://yew.rs), built and served with
[Trunk](https://trunkrs.dev)) that runs the `wasm32`-compiled syntax, formatting, and
analysis layers client-side, with no server round-trip — including resolving a `jals.toml`'s
`[dependencies]` in-browser so hover/completion/type-check see external library types.

```sh
# One-time setup: the wasm target and Trunk
rustup target add wasm32-unknown-unknown
cargo install trunk

# Serve with live reload (defaults to http://0.0.0.0:8000)
cd jals-playground
trunk serve
```

The browser bundle is produced by Trunk (`wasm32`). As a regular workspace member,
`jals-playground` is also compiled by the host `cargo build`, `clippy`, and `test` jobs.

## Using the crates as libraries

The crates are not published to crates.io yet; depend on them via git or a path.

### `jals-syntax`

```rust
use jals_syntax::{tokenize, SyntaxKind};

// Lex: concatenating every token's text reproduces the input (lossless).
let tokens = tokenize("int x = 1;");
assert_eq!(tokens[0].kind, SyntaxKind::INT_KW);

// Parse into a typed AST view over the CST.
use jals_syntax::ast::{AstNode, SourceFile};
let parse = jals_syntax::parse("class Foo { }");
let file = SourceFile::cast(parse.syntax()).unwrap();
let class = file.decls().next().unwrap();
assert_eq!(class.syntax().text().to_string(), "class Foo { }");
```

### `jals-fmt`

```rust
use jals_fmt::{Config, format_source};

let out = format_source("class C{int x=1;}", &Config::default());
assert_eq!(out.formatted, "class C {\n    int x = 1;\n}\n");
assert!(!out.has_warnings());
```

## Architecture

```
source ──▶ lexer (hand-written) ──▶ CST parser (rowan) ──▶ typed AST
              lossless                error-resilient        (jals-syntax)
                                            │
                                            ▼
                            lower CST ──▶ Doc IR ──▶ render ──▶ formatted text
                                          Wadler/Prettier        (jals-fmt)
```

- **Lexer** (`jals-syntax`): a hand-written scanner that emits trivia (whitespace,
  newlines, comments) as real tokens so the stream is lossless. Context-sensitive keywords
  (`var`, `record`, `sealed`, `when`, module directives, …) are lexed as identifiers and
  promoted by the parser.
- **Parser** (`jals-syntax`): a hand-written recursive-descent parser that emits an event
  stream assembled into a `rowan` green tree. It recovers from errors and records them as
  `SyntaxError`s rather than aborting.
- **Typed AST** (`jals-syntax`): zero-cost newtype views over the CST, so consumers read the
  tree through typed accessors instead of matching raw kinds.
- **Formatter** (`jals-fmt`): lowers the CST into a Wadler/Prettier-style document IR, then
  renders it, choosing for each group whether it fits on one line or must break.

## Development

```sh
cargo build --workspace
cargo test  --workspace --all-features
```

CI (GitHub Actions) runs the following checks; mirror them locally before pushing:

```sh
cargo fmt --all --check                                       # formatting
cargo run -p xtask -- codegen --check                         # generated AST is up to date
cargo clippy --workspace --all-targets --all-features -- -D warnings   # lints
cargo test --workspace --all-features                         # tests
taplo fmt --check --diff                                      # TOML formatting
cargo machete                                                 # unused dependencies
typos                                                         # spelling
ast-grep test --skip-snapshot-tests                           # ast-grep rule tests
ast-grep scan --error                                         # structural lints (no-free-functions, …)

# wasm: the pure `no_std` crate set (built as one package set so their `std` features stay off) …
cargo build --release --target wasm32-unknown-unknown \
  -p jals-syntax -p jals-classfile -p jals-hir -p jals-decompile \
  -p jals-fmt -p jals-lint -p jals-fs -p jals-config
# … plus jals-classpath's wasm-compatible core (host I/O is behind its default `native` feature)
cargo build --release --target wasm32-unknown-unknown -p jals-classpath --no-default-features
```

Lints are configured workspace-wide in the root `Cargo.toml` under `[workspace.lints]`
(clippy `all` / `pedantic` / `nursery` at `warn`, denied in CI), and structural rules live in
`.ast-grep/rules/`. The build matrix also compiles the workspace for `x86_64`/`aarch64` Linux.
Dependency updates are automated with Dependabot.

The main structural rule, `no-free-functions`, asks helpers to be associated (or nested)
functions rather than free functions. Abstraction is treated as the top priority here — it
raises the overall quality of the codebase and can contribute meaningfully to performance — so
free functions are avoided wherever possible. An associated function's parent type lets a caller
tell at a glance what the function relates to and does, which matters most for a `pub` function
reached through an external import; a bare free function offers no such anchor. Collecting
functions on a specific struct also makes near-duplicate helpers easy to notice and consolidate.
Move a helper into an `impl`/`trait`, or nest it inside its sole caller when it is purely local.

### Invariants worth protecting

These properties are enforced by tests (including `proptest` property tests) and must hold
for any change to the syntax or formatting layers:

- The lexer is lossless and never panics.
- The parser always returns a tree and never panics.
- The formatter preserves the significant-token sequence, never drops or reorders comments,
  and is idempotent.
- `jals-syntax`, `jals-fmt`, `jals-lint`, `jals-hir`, `jals-classfile`, `jals-decompile`,
  `jals-fs`, and `jals-config` build for `wasm32-unknown-unknown` as `no_std` crates;
  `jals-classpath`'s resolution core builds for `wasm32` too (`--no-default-features`).

## Status

Early stage (`0.1.0`). The formatter, linter, and language server are functional and the
syntax layer covers a broad slice of Java, but APIs may change. Semantic analysis
(`jals-hir`) covers name resolution, cross-file type indexing, and type inference/checking,
including types resolved from a project's classpath and `[dependencies]`; generic-method
inference, richer bytecode decompilation (`switch`/`try`-`catch`/`break`/`continue`), and
Maven-coordinate (`group:artifact:version`) dependency resolution are still open. The `jals
build`/`run`/`clean`/`init` front end is a faithful but thin `javac`/`java` wrapper today,
with fuller dependency management, testing, and packaging on its
[roadmap](jals-build/README.md#roadmap).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.
