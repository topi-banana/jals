# jals

[![CI](https://github.com/topi-banana/jals/actions/workflows/ci.yml/badge.svg)](https://github.com/topi-banana/jals/actions/workflows/ci.yml)

A Java toolchain written in Rust, built on a **lossless** syntax tree.

`jals` parses Java source into a full-fidelity concrete syntax tree (CST) — every byte,
including whitespace and comments, is preserved — and uses that tree to power source
tooling. Today it ships a code formatter, a linter, and a language server (LSP), all
backed by a shared semantic layer (`jals-hir`) that does name resolution, cross-file type
indexing, and type inference/checking — including resolving types from a project's compiled
classpath and `[dependencies]` (explicit local/remote jars plus transitive `git`/`path` JALS source
projects, with readable decompiled Java when a jar has no sources). Alongside them, a Cargo-style
build front end (`jals build` / `run` / `clean` / `init`) wraps the JDK's `javac` / `java` from a
`jals.toml` manifest and can run sandboxed Rhai build scripts before compilation.

> 日本語版の README は [README_jp.md](README_jp.md) にあります。

## Highlights

- **Lossless & error-resilient.** The lexer maps every byte of input to exactly one token,
  and the parser always produces a tree — neither ever panics, even on malformed input.
- **Java 26 grammar.** Classes, interfaces, enums, records, sealed types, annotations,
  lambdas, switch expressions, patterns (including record patterns and guards), and more.
- **A formatter with guarantees.** Comments are never dropped or reordered, formatting is idempotent
  (`format(format(x)) == format(x)`), and the significant-token multiset changes only where a
  declared operation says it may — every one of them off by default bar the dialect's own. An output
  that fails that check is discarded and the input handed back untouched — and the run *says* so,
  because a file that was refused is byte-identical to one that needed nothing.
- **A linter with real semantics.** Beyond syntactic checks, `jals lint` catches unused
  locals, type mismatches, unreported checked exceptions, and dead conditionals, using name
  resolution and type inference over the CST — not just pattern matching.
- **Cargo-style Java builds.** A `jals.toml` manifest — the Java analogue of `Cargo.toml` —
  drives `jals build` / `run` / `clean` / `init`. Optional Rhai scripts run before `javac`, using
  bounded storage-only APIs to generate sources and augment flags, classpaths, and environments.
- **Transitive source-project graphs.** `git`/`path` dependencies can themselves be JALS projects.
  Stable node identities deduplicate diamonds; every unique node is preprocessed dependency-first,
  then projected into verified source/classpath artifacts without mutating dependency trees.
- **`wasm32`-ready core.** The syntax, formatting, linting, and semantic-analysis layers
  (`jals-editor`, `jals-syntax`, `jals-fmt`, `jals-lint`, `jals-hir`, `jals-classfile`,
  `jals-decompile`, `jals-javac`, `jals-storage`, `jals-config`) are `no_std` and build for
  `wasm32-unknown-unknown`; `jals-classpath`'s resolution core, `jals-project`'s in-memory graph, and
  `jals-build`'s Rhai runner do too (host I/O sits behind `native` features). The browser playground
  therefore runs the same analysis, project-graph, and build-script stack client-side.

## Workspace layout

`jals` is a Cargo workspace of sixteen product crates, including a browser playground:

| Crate                                | Description                                                                                                                                                                                                                                                                                                                                                                                                         |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [`jals-editor`](jals-editor)         | Protocol-neutral editor semantics (definition, references, hover, completion, signature help, and highlights) plus UTF-8 byte/UTF-16 coordinate conversion, shared by the LSP and browser playground.                                                                                                                                                                                                               |
| [`jals-syntax`](jals-syntax)         | A lossless Java lexer and an error-resilient CST parser (`rowan`), plus a typed AST layer over the CST. The shared foundation for every other tool.                                                                                                                                                                                                                                                                 |
| [`jals-fmt`](jals-fmt)               | **WIP (rewrite in progress).** A Wadler/Prettier-style pretty-printer driven by the `jals-syntax` CST — currently a no-op that returns its input unchanged.                                                                                                                                                                                                                                                         |
| [`jals-lint`](jals-lint)             | The linter (`jals lint` via `jals-cli`): a rule registry over the CST plus `jals-hir` — unused locals, type mismatches, unreported exceptions, dead (constant) conditionals, and feature-gated preview-feature checks.                                                                                                                                                                                              |
| [`jals-hir`](jals-hir)               | Name resolution, a cross-file project type index, and type inference/checking over the CST — the semantic foundation the linter and LSP build on. Also bridges in external types from a compiled classpath.                                                                                                                                                                                                         |
| [`jals-classfile`](jals-classfile)   | A complete, byte-exact read/write model of the JVM `.class` file format (JVMS ch. 4).                                                                                                                                                                                                                                                                                                                               |
| [`jals-decompile`](jals-decompile)   | Reconstructs readable Java from a parsed `.class` file: type/signature rendering, initializers, declared `throws`, and (incrementally) full method-body decompilation from bytecode.                                                                                                                                                                                                                                |
| [`jals-javac`](jals-javac)           | The compiler: Java source to executable code. Emits one JVM class file per declared type, or one WebAssembly module for a whole project with the host's garbage collector managing every object. It never checks — diagnostics are `jals-lint`'s job — but it does resolve, because emitting one `invokevirtual` needs the selected overload and its descriptor.                                                       |
| [`jals-classpath`](jals-classpath)   | Resolves and loads project bytes and verified classpath artifacts (local/remote and bundled/nested jars) for `jals-hir`, the linter, and the LSP; falls back to decompiled `.java` skeletons when a dependency ships no sources.                                                                                                                                                                                    |
| [`jals-config`](jals-config)         | The pure data model, parsing, discovery, and validation for all three config files (`jals.toml`, `jalsfmt.toml`, `jalslint.toml`).                                                                                                                                                                                                                                                                                  |
| [`jals-exec`](jals-exec)             | The unified current-thread execution context for native, browser, and inline hosts, including deterministic worker fan-out and runtime-free cooperative yielding.                                                                                                                                                                                                                                                   |
| [`jals-storage`](jals-storage)       | Deterministic, revisioned project storage. Portable code uses validated `FileKey`/`DirKey` values, immutable `CodeTree` snapshots, transactions, overlays, a SHA-256 verified artifact cache (whole-buffer `lookup` or streaming `open_verified` readers), and the portable `io` byte-stream traits the class-file codec parses through; memory and `std`-gated native adapters implement the same sealed contract. |
| [`jals-project`](jals-project)       | Discovers the transitive path/Git/JAR project graph with stable node identity, probes only each selected root's exact `jals.toml`, enforces the resolved-to-preprocessed phase transition, and publishes dependency inputs only as node-scoped verified artifacts for `jals-classpath`. Includes portable in-memory and native acquisition hosts.                                                                   |
| [`jals-build`](jals-build)           | A Cargo-style build orchestrator: it turns `jals.toml` into `javac`/`java` plans, clean keys, and scaffolding, and optionally runs sandboxed Rhai pre-build scripts over revisioned project storage. Backs `jals build`/`run`/`clean`/`init` and the LSP/playground build phase.                                                                                                                                    |
| [`jals-lsp`](jals-lsp)               | A Language Server Protocol server (the `jals lsp` subcommand) providing diagnostics, document symbols, formatting, hover, go-to-definition, find-references, and more from the same CST and semantic layer. Host-only.                                                                                                                                                                                              |
| [`jals-cli`](jals-cli)               | The `jals` command-line binary.                                                                                                                                                                                                                                                                                                                                                                                     |
| [`jals-playground`](jals-playground) | A browser playground built with [Yew](https://yew.rs) and served by [Trunk](https://trunkrs.dev). It compiles to `wasm32` and runs the syntax/formatting/analysis layers entirely in the browser.                                                                                                                                                                                                                   |

Two more workspace members are development-only tooling, not part of the shipped product:
[`jals-tests`](jals-tests) (corpus harnesses that check parser soundness and formatter
fidelity against real-world Java) and `xtask` (the `cargo xtask codegen` AST generator).

```
jals/
├── jals-editor/      # editor queries + byte/UTF-16 coordinates (no_std, wasm-compatible)
├── jals-syntax/      # lexer + CST parser + typed AST           (no_std, wasm-compatible)
├── jals-fmt/         # formatter — WIP rewrite, currently a no-op (no_std, wasm-compatible)
├── jals-lint/        # linter (rules over CST + jals-hir)       (no_std, wasm-compatible)
├── jals-hir/         # name resolution + type inference         (no_std, wasm-compatible)
├── jals-classfile/   # JVM .class read/write model              (no_std, wasm-compatible)
├── jals-decompile/   # .class -> readable Java                  (no_std, wasm-compatible)
├── jals-javac/       # Java -> .class / WasmGC compiler         (no_std, wasm-compatible)
├── jals-classpath/   # classpath + dependency resolution        (no_std + wasm-compatible core)
├── jals-config/      # jals.toml/jalsfmt.toml/jalslint.toml models (no_std, wasm-compatible)
├── jals-exec/        # current-thread execution + worker fan-out (no_std, wasm-compatible)
├── jals-storage/     # revisioned project storage               (no_std, wasm-compatible)
├── jals-project/     # transitive source-project graph          (no_std + wasm-compatible core)
├── jals-build/       # Cargo-style javac/java build planner     (no_std + wasm-compatible core)
├── jals-lsp/         # LSP server (async-lsp, `jals lsp`)       (std, host-only)
├── jals-cli/         # `jals` binary                            (std)
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

### npm / bun

Install the `jals` command straight from the Git repository — no Rust toolchain needed. The launcher
downloads the prebuilt binary that matches your platform from the GitHub release assets (verifying its
SHA-256) on first run:

```sh
bun install -g git+https://github.com/topi-banana/jals.git
# or, with npm:
npm install -g git+https://github.com/topi-banana/jals.git
```

This resolves the release tagged `v<version>`, where `<version>` is the `package.json` version, so a
matching release must be published. Prebuilt binaries are available for Linux, macOS, and Windows on
`x64`/`arm64`; on any other platform the launcher points you at `cargo install`. Set
`JALS_INSTALL_BASE_URL` to fetch the assets from a mirror. The launcher runs on Node.js — bun
ships a `node` shim, so a bun-only setup works out of the box.

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

It also fails when the formatter **refused its own output** — the fail-safe rejected the layout and
handed the input back, so the file is byte-identical without having been formatted. That is a bug in
`jals-fmt` rather than in the source, and it is reported as such; `--check` fails because its
question is "is every file formatted", and this file was not.

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

# Cargo-style build features a `script` reads with `build.feature("…")`. Select them with
# `--features` / `--all-features` / `--no-default-features`; selection is additive. Per package,
# as in Cargo: a feature reaches a dependency only where a manifest says so — `<dep>/<feature>`
# below, or `features` under [dependencies].
# [features]
# default = ["server"]
# server  = []
# client  = []
# gpu     = ["render/vulkan"]   # enables `vulkan` in the `render` dependency (Cargo's `serde/std`)

[build]
release = 21                        # javac --release N
# source-dirs = ["src/main/java"]   # -sourcepath roots, also scanned for .java files
# classes-dir = "target/classes"    # javac -d
# classpath   = ["libs/guava.jar"]  # -classpath entries
# script = { type = "rhai", file = "build.rhai" }

[run]
main-class = "com.example.Main"     # entry point for `jals run` (used when no [[bin]] exists)

[dependencies]
# Source projects are discovered transitively; `dir` selects a project inside a monorepo.
shared = { path = "../shared" }
core = { git = "https://github.com/example/mono", rev = "abc123", dir = "core" }
# `features` enables build features in that dependency's own build.rhai (Cargo's per-dep features);
# `default-features = false` skips that dependency's own `default` list.
render = { path = "../render", features = ["vulkan"], default-features = false }

# Or declare several named entry points and pick one with `jals run --bin <name>`:
# [[bin]]
# name = "server"
# main-class = "com.example.Server"
```

With `script` configured, `build.rhai` runs before source discovery and `javac`. It can read the
project snapshot and the selected `[features]`, publish ordinary files below
`target/jals/build/rhai/out`, and add generated
sources, classpath entries, `javac`/JVM flags, and compile/run environment entries. A typed `tasks`
DAG can also declare bounded, digest-verified downloads, JSON projections, safe source-JAR
extraction, Mojang-mappings jar remapping, jar merge, compile-oriented decompilation, and explicit
exclusive source-tree publication; Rhai never reads task results or invokes a process.
`replace-root` replaces every file below its declared destination and is atomic with
ordinary script output. The native CLI and LSP execute tasks; the LSP defers a root containing an
open document, while the browser rejects physical publication before fetching. See the runnable
[`examples/rhai_build_script`](examples/rhai_build_script) project and the
[`jals-build` Rhai reference](jals-build/README.md#rhai-build-scripts) for the complete API,
fingerprinting/cache behavior, sandbox limits, and Rust `BuildScript` model.
The source-archive task shape is shown in
[`examples/task_source_archive`](examples/task_source_archive); a full remapped-Minecraft example is
[`examples/minecraft`](examples/minecraft).

The root Rhai phase itself is capability-limited, but its compiler/JVM arguments, classpath entries,
and subprocess environment directives intentionally affect the later explicit `jals build`/`run`
JDK process. Treat root build scripts as project code and review them before building an untrusted
checkout.

Outside that portable phase, `jals-build` plans commands as data and `jals-cli` owns host source
discovery and JDK execution (resolving `javac`/`java` via `$JAVAC`/`$JAVA`, then
`$JAVA_HOME/bin`, then `PATH`).

### Transitive project dependencies

For a `path` or `git` dependency, the dependency root is the declared directory/checkout followed by
`dir` when present. `jals-project` probes exactly `<selected-root>/jals.toml` and never searches
upward. If that file exists, the node is a JALS project: its child dependencies, `[build] classpath`,
and `[build] source-dirs` all resolve relative to its selected root. If it is missing, the node keeps
the legacy source convention (`src/main/java`, then `src`, then the selected root). A present but
malformed manifest or a dependency cycle is a hard `jals build`/`run` failure.

Graph nodes have stable identities, so a diamond is visited once even when dependency names differ.
Every unique node takes the preprocessing transition unconditionally and exactly once in
dependency-first order; binary and legacy-source nodes are no-ops, while a manifest-backed node runs
its optional Rhai script. A dependency script exports only sources registered with
`build.add_source` and classpath entries registered with `build.add_classpath`. Its `javac`/JVM
arguments, compile/run environment, and metadata remain node-local and do not propagate. Outputs,
classpath entries, and source snapshots are published under the node identity as digest-verified
artifacts, and dependency source trees are never mutated. The root script retains the full semantics
described above, including process arguments/environment and revision-checked root output updates.

The native CLI uses this complete graph for `build`/`run`, compiling transitive sources and adding
transitive JARs and declared classpaths. `lint` resolves its binary/classpath side while continuing to
lint only requested files. The LSP indexes source artifacts for analysis/navigation, watches local
path roots, and reports a hard graph error on the root manifest before falling back to root-only
analysis. The playground runs the portable `MemoryProjectGraph` over one captured in-memory
`CodeTree`, so in-tree path projects and their scripts work in the browser. Git acquisition is not
available in a browser: Git entries produce warnings and are omitted; this is not browser Git
support.

This transitive JALS source-project graph is implemented now. Maven/POM coordinate resolution,
coordinate version selection, transitive Maven downloads, and a `jals.lock` lockfile remain future
work.

### Options

| Option            | Description                                                                                                                 |
| ----------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `[PATHS]...`      | Files or directories to format. Directories are searched recursively for `.java` files. No paths → stdin/stdout.            |
| `--check`         | Do not write anything; exit non-zero if any file would change, or if the formatter refused its own output.                   |
| `-D <LINT>`       | Deny lints (repeatable). Only `warnings` is recognized: fail when any file has syntax warnings.                             |
| `--config <PATH>` | Use this config file instead of discovering `jalsfmt.toml`.                                                                 |
| `--no-migrate`    | Do not generate a `jalsfmt.toml` from a detected native formatter config. The detected settings are still used for the run. |

## Configuration

The formatter reads a `jalsfmt.toml` file. The CLI discovers it by searching upward from
each formatted file's directory (or pass `--config <PATH>` to use a specific file). The config
is a set of **sections** — `[layout]`, `[blank-lines]`, `[braces]`, `[wrapping]`, `[spacing]`,
`[comments]`, `[imports]`, `[literals]` — and every key, and every whole section, is optional
and falls back to the default. Keys use kebab-case.

```toml
# jalsfmt.toml — every key is optional; values below are the defaults.
[layout]
indent-style = "space" # "space" | "tab" | "mixed"
indent-width = 4       # columns per indentation level
tab-width = 4          # display width of a literal tab
max-width = 100        # column limit
line-ending = "lf"     # "lf" | "crlf" | "auto" | "native"
insert-final-newline = true

[blank-lines]
max-in-code = 1 # longest run of source blank lines kept in a body

[imports]
order = "preserve" # "preserve" | "sort" | "group"

[comments]
format-javadoc = false
width = 80 # only consulted when a reflow key is on
```

`jals-fmt/jalsfmt.toml` lists the frequently-touched keys of every section; each section module
under `jals-config/src/fmt/` documents the full surface, and `jals-fmt/MAPPING.md` records which
Eclipse / IntelliJ / google-java-format / Spotless setting each key corresponds to.

### Migrating an existing formatter config

With no `jalsfmt.toml`, `jals fmt` and `jals init` look for a formatter config the project
already has, translate it into jals's own options, and write the result out. The search is by
**content**, not by file name — an exported Eclipse profile or IntelliJ scheme can be called
anything — and the first match wins:

1. `.editorconfig`
2. `.idea/codeStyles/Project.xml`, or any top-level `*.xml` holding a `<code_scheme>`
3. any top-level `*.xml` holding an Eclipse formatter profile
4. `.settings/org.eclipse.jdt.core.prefs`

The generated `jalsfmt.toml` lists only the keys that differ from the defaults, under a header
recording where it came from:

```toml
# Generated by jals from .settings/org.eclipse.jdt.core.prefs (eclipse).

[layout]
indent-width = 2
max-width = 120
```

Some ground rules:

- **An existing `jalsfmt.toml` is never touched.** Finding one — in the directory or any ancestor
  — ends the search.
- **The search stops at the project root**, the nearest ancestor holding a `jals.toml` or a
  `.git`. A directory tree with neither is not treated as a project, and nothing is migrated or
  written there.
- **`--check`, `--diff`, and stdin write nothing** but still use the migrated settings, so a CI
  check agrees with what a normal run would produce. `--config <PATH>` skips migration entirely,
  and `--no-migrate` keeps the settings while skipping the write.
- **Spotless is not detected.** Its configuration is a Gradle/Maven build DSL — code, not data —
  and it selects a delegate engine, so its values cannot be read reliably.

Translation is onto jals's common vocabulary, not a bijection. A native option jals has no
equivalent for is not carried across, and one that jals splits across sections lands in each of
them — an IntelliJ `RIGHT_MARGIN`, say, sets both `layout.max-width` and `comments.width` — so the
generated file can name a key your native config never mentioned. `jals-fmt/MAPPING.md` §7 records what is deliberately left
behind, and `jals-fmt/DESIGN.md` §15 describes the whole scheme.

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
[Trunk](https://trunkrs.dev)) that runs the `wasm32`-compiled syntax, formatting, analysis, and
sandboxed Rhai build-script layers client-side, with no server round-trip — including generated
Java sources, remote jars, and the portable in-memory path-project graph from a `jals.toml` so
hover/completion/type-check see those inputs. The browser cannot clone Git dependencies; it reports
each one as a warning rather than claiming Git support.

*Build* compiles the workspace with the in-process `jals-javac` — no JDK, no subprocess — and offers
the result as a download: `[build] backend = { type = "jals" }` packages one class file per declared
type into a runnable `.jar`, and `{ type = "jals-wasm" }` emits a single WebAssembly module for the
whole project. (`{ type = "javac" }`, the manifest default, has nothing to spawn in a browser tab and
says so.) The compiler resolves against `jals-hir`'s embedded JDK stubs rather than a classpath, so
resolved `[dependencies]` jars stay on the editor's classpath but not the compiler's, and any
construct without a lowering yet is *reported* in the Build output tab rather than mis-emitted.

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

> **WIP — the formatter is being rewritten from scratch and is currently a no-op.**
> `FormatOutput::format_source` returns its input byte-for-byte unchanged (only parser syntax
> errors are surfaced as warnings); a `Config` is accepted and ignored. The example below shows
> the intended shape once the rewrite lands.

```rust
use jals_config::fmt::Config;
use jals_fmt::FormatOutput;

let out = FormatOutput::format_source("class C{int x=1;}", &Config::default()).await;
// Intended (once implemented): "class C {\n    int x = 1;\n}\n".
// Today (no-op): out.formatted == "class C{int x=1;}".
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
- **Formatter** (`jals-fmt`): **WIP — under a from-scratch rewrite, currently a no-op.** The
  intended design lowers the CST into a Wadler/Prettier-style document IR, then renders it,
  choosing for each group whether it fits on one line or must break.
- **Project graph** (`jals-project`): discovers transitive path/Git/JAR nodes with stable identities,
  probes only the selected root's exact manifest, and makes preprocessing a required type-level
  transition before assembly. Assembly exposes graph metadata but sends consumers only authored or
  script-registered sources/classpath through node-scoped verified artifacts; native acquisition and
  the portable one-`CodeTree` memory host share that deep interface.

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
cargo check -p jals-project --no-default-features             # portable project-graph core
cargo check -p jals-project --all-features                    # native path/Git acquisition

# wasm: the pure `no_std` crate set (built as one package set so their `std` features stay off) …
cargo build --release --target wasm32-unknown-unknown \
  -p jals-editor -p jals-syntax -p jals-classfile -p jals-hir -p jals-decompile \
  -p jals-javac -p jals-fmt -p jals-lint -p jals-storage -p jals-config
# … plus jals-classpath's wasm-compatible core (host I/O is behind its default `native` feature)
cargo build --release --target wasm32-unknown-unknown -p jals-classpath --no-default-features
# The portable in-memory project graph includes dependency-script preparation and artifact projection
cargo check -p jals-project --no-default-features --target wasm32-unknown-unknown
# The Rhai feature remains host-I/O-free and wasm-compatible; the browser builds the same engine
cargo check -p jals-build --no-default-features --features rhai --target wasm32-unknown-unknown
cargo build -p jals-playground --target wasm32-unknown-unknown
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
- The formatter preserves the significant-token multiset except where an operation declared in
  `jals_fmt::passes::token_license::OPERATIONS` (`jals-fmt/DESIGN.md` §20) applies, never drops or
  reorders comments, and is idempotent. The fail-safe reads that table, and a run it cannot vouch
  for returns the input unchanged.
- `jals-editor`, `jals-syntax`, `jals-fmt`, `jals-lint`, `jals-hir`, `jals-classfile`,
  `jals-decompile`, `jals-javac`, `jals-storage`, and `jals-config` build for
  `wasm32-unknown-unknown` as `no_std` crates;
  `jals-classpath`'s resolution core builds for `wasm32` too (`--no-default-features`), as does
  `jals-build` with its portable `rhai` feature and `jals-project`'s in-memory graph.

## Status

Early stage (`0.1.0`). The formatter, linter, and language server are functional and the
syntax layer covers a broad slice of Java, but APIs may change. Semantic analysis
(`jals-hir`) covers name resolution, cross-file type indexing, and type inference/checking,
including types resolved from a project's classpath and `[dependencies]`; generic-method
inference, richer bytecode decompilation (`switch`/`try`-`catch`/`break`/`continue`), and
Maven-coordinate (`group:artifact:version`) POM/version resolution and a lockfile are still open.
The transitive JALS `path`/`git` source-project graph is implemented; broader Maven dependency
management, testing, and packaging remain on the build
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
