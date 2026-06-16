# jals

[![CI](https://github.com/topi-banana/jals/actions/workflows/ci.yml/badge.svg)](https://github.com/topi-banana/jals/actions/workflows/ci.yml)

A Java toolchain written in Rust, built on a **lossless** syntax tree.

`jals` parses Java source into a full-fidelity concrete syntax tree (CST) — every byte,
including whitespace and comments, is preserved — and uses that tree to power source
tooling. Today it ships a code formatter and a language server (LSP); the same foundation is
designed to host a linter too. Alongside them, a Cargo-style build front end
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
- **Cargo-style Java builds.** A `jals.toml` manifest — the Java analogue of `Cargo.toml` —
  drives `jals build` / `run` / `clean` / `init`, a thin, pure `javac`/`java` wrapper that
  plans commands as data and never touches the JDK itself until the CLI runs them.
- **`wasm32`-ready core.** Everything except the CLI builds for `wasm32-unknown-unknown`,
  so the syntax and formatting layers can run in the browser.

## Workspace layout

`jals` is a Cargo workspace of five core crates plus a browser playground:

| Crate | Description |
| --- | --- |
| [`jals-syntax`](jals-syntax) | A lossless Java 26 lexer (`logos`) and an error-resilient CST parser (`rowan`), plus a typed AST layer over the CST. The shared foundation for every other tool. |
| [`jals-fmt`](jals-fmt) | A Wadler/Prettier-style pretty-printer driven by the `jals-syntax` CST. |
| [`jals-lsp`](jals-lsp) | A Language Server Protocol server (the `jals lsp` subcommand) providing diagnostics, document symbols, and formatting from the same CST. Host-only. |
| [`jals-build`](jals-build) | A Cargo-style build orchestrator: it parses a `jals.toml` manifest and turns it into `javac`/`java` command plans, clean paths, and project scaffolding — all as pure data, with no `jals-syntax` dependency and no I/O. Backs `jals build`/`run`/`clean`/`init`. |
| [`jals-cli`](jals-cli) | The `jals` command-line binary. |
| [`jals-playground`](jals-playground) | A browser playground built with [Yew](https://yew.rs) and served by [Trunk](https://trunkrs.dev). It compiles to `wasm32` and runs the `jals-syntax`/`jals-fmt` layers entirely in the browser. |

```
jals/
├── jals-syntax/      # lexer + CST parser + typed AST  (wasm-compatible)
├── jals-fmt/         # formatter (CST -> Doc IR -> text)
├── jals-lsp/         # LSP server (async-lsp, `jals lsp`)
├── jals-build/       # Cargo-style javac/java build planner  (wasm-compatible)
├── jals-cli/         # `jals` binary
└── jals-playground/  # browser playground (Yew + Trunk -> wasm)
```

## Installation

Requires a Rust toolchain with the **2024 edition** (Rust 1.85 or newer; CI builds on
stable).

```sh
# Build the workspace
cargo build --release

# Install the `jals` binary into ~/.cargo/bin
cargo install --path jals-cli
```

The release binary is produced at `target/release/jals`.

## Usage

`jals` is invoked through subcommands: `fmt` (format source), `lsp` (language server), and a
Cargo-style build front end — `init`, `build`, `run`, and `clean`.

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

### Run the language server

`jals lsp` starts an LSP server over stdio for editor integration — diagnostics, document
symbols, and whole-document formatting, all driven by the same CST. It is launched by an
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
[Trunk](https://trunkrs.dev)) that runs the `wasm32`-compiled syntax and formatting layers
client-side, with no server round-trip.

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
source ──▶ lexer (logos) ──▶ CST parser (rowan) ──▶ typed AST
              lossless           error-resilient        (jals-syntax)
                                       │
                                       ▼
                            lower CST ──▶ Doc IR ──▶ render ──▶ formatted text
                                          Wadler/Prettier        (jals-fmt)
```

- **Lexer** (`jals-syntax`): a `logos`-based scanner that emits trivia (whitespace,
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
cargo clippy --workspace --all-targets --all-features -- -D warnings   # lints
cargo test --workspace --all-features                         # tests
taplo fmt --check --diff                                      # TOML formatting
cargo machete                                                 # unused dependencies
cargo build --release --target wasm32-unknown-unknown -p jals-syntax   # wasm core
```

The build matrix also compiles the workspace for `x86_64`/`aarch64` Linux. Dependency
updates are automated with Dependabot.

### Invariants worth protecting

These properties are enforced by tests (including `proptest` property tests) and must hold
for any change to the syntax or formatting layers:

- The lexer is lossless and never panics.
- The parser always returns a tree and never panics.
- The formatter preserves the significant-token sequence, never drops or reorders comments,
  and is idempotent.
- `jals-syntax` (and `jals-fmt`) build for `wasm32-unknown-unknown`.

## Status

Early stage (`0.1.0`). The formatter and language server are functional and the syntax layer
covers a broad slice of Java 26, but APIs may change. The `jals build`/`run`/`clean`/`init`
front end is a faithful but thin `javac`/`java` wrapper today, with dependency management,
testing, and packaging on its [roadmap](jals-build/README.md#roadmap). A linter (`jals-lint`)
is the intended next consumer of the syntax layer.

## License

No license has been declared yet. Until one is added, all rights are reserved by the
authors.
