# jals-build

Cargo-style build orchestration for Java projects — the engine behind `jals build` / `jals run`
/ `jals clean` / `jals init`.

A [`jals.toml`](#the-manifest-jalstoml) manifest is the Java analogue of `Cargo.toml`: it says
where the sources live, where compiled classes go, which Java release to target, and what is on
the classpath. This crate parses that manifest and turns it (plus already-resolved inputs) into
a `javac`/`java` command line, the set of paths a clean removes, or the files a fresh project
needs — the **core is pure data**, never spawning a process or touching the filesystem:

```
jals.toml ───────▶ Manifest ──┐
                              ├──▶ CompileRequest ─▶ SubprocessToolchain ─▶ ┐
discovered .java files ───────┘    RunRequest     ─▶  (native feature)      ┤  jals-cli
                                                       spawns javac/java    ├▶ discovers,
InitOptions ──────────────────────▶ .scaffold()      ─▶ [ScaffoldFile] ────┤  writes files,
                                     CleanTargets::keys  ─▶ [DirKey] ───────┘  removes dirs
```

`jals-cli` owns the *other* side effects: it discovers the manifest, walks the source tree, writes
the scaffold, and deletes the clean paths. Spawning `javac`/`java` (and discovering installed JDKs
for `[toolchain]`) is the one side effect that lives **here**, in the default-on **`native` feature**
(`SubprocessToolchain`, plus the `<dyn Compiler>::select` / `<dyn Runtime>::select` factories that
match the manifest's two `[toolchain]` enums — `jals_config::Compiler` / `jals_config::Runtime` —
to a boxed backend, one per step) — a deliberate exception so a single toolchain abstraction serves
both the CLI and, later, an in-browser (wasm) backend. The abstraction is a **pair of object-safe
traits** mirroring the two selectors — `Compiler` (compile + describe) and `Runtime` (run +
describe) — and the subprocess backend is not their only implementation: the core also ships
`BuiltinToolchain`, the **in-process backend** selected by `[toolchain] compiler/runtime =
"builtin"` — today a *dummy* (compile copies each source into the `classes-dir` unchanged; run is a
successful no-op) whose I/O goes through a revisioned `jals_storage::ProjectStorage`; memory and
native adapters obey the same transaction contract, and a real embedded compiler later replaces
the copy step without touching the seam. Because each step is selected from its own enum and driven as its
own `&dyn Compiler` / `&dyn Runtime`, mixed selections need no routing composite. The core — the
`Manifest` model, the `Invocation` planner, and the `Compiler`/`Runtime` traits plus the
filesystem-free `ToolResolver` and `BuiltinToolchain` — stays pure: deterministic, unit-testable
with no JDK installed, and **`wasm32`-compatible** with `--no-default-features` (its only
dependencies are `jals-config`, for the pure `Manifest` model, `jals-storage`, for typed project
keys/revisions/cache, and `toml`, for `ManifestError`'s underlying parse-error type).

## What it does today

Four subcommands are wired through `jals-cli`, each backed by one pure entry point here:

| Command | Backed by | What it does | Flags |
| --- | --- | --- | --- |
| `jals build` | `Invocation::build` | Discover the manifest and `.java` sources, build the `javac` command, and run it. | `--manifest-path <PATH>`, `--dry-run`, `-v`/`--verbose`, `--out-dir <DIR>`, `--bin <NAME>` |
| `jals run` | `RunTarget::resolve` + `Invocation::build` + `Invocation::run` | Compile, then run the resolved entry point with `java`. Compilation must succeed first. | `--manifest-path <PATH>`, `--dry-run`, `-v`/`--verbose`, `--main-class <FQCN>`, `--bin <NAME>`, `-- <args>` |
| `jals clean` | `CleanTargets::paths` | Remove the build output (the `classes-dir`). A never-built project succeeds quietly. | `--manifest-path <PATH>`, `--dry-run` |
| `jals init [PATH]` | `InitOptions::scaffold` | Scaffold a new project: `jals.toml`, a starter `Main.java`, and a `.gitignore`. Refuses to overwrite an existing `jals.toml`. | `--name <NAME>` |

Common behavior, all implemented in `jals-cli` on top of this crate:

- **Manifest discovery** — `Manifest::discover_path` searches upward from the cwd for `jals.toml`
  (like Cargo). The project root is the manifest's parent directory; every manifest path is
  resolved relative to it. A missing manifest is an **error** (there is nothing to build),
  unlike the formatter/linter configs where a missing file means "use defaults".
- **Source discovery** — every `source-dirs` entry must exist, and at least one `.java` file
  must be found, else the build errors. Sources are passed to `javac` last, in sorted order.
- **`--dry-run`** prints the exact command(s) (via `Invocation::display_command`, which quotes
  whitespace) and exits without compiling/running/deleting. `-v`/`--verbose` prints the same
  command(s) and then runs them.
- **JDK tool resolution** — the `SubprocessToolchain` (the crate's default-on `native` feature)
  selects `javac`/`java` per the manifest's [`[toolchain]`](#toolchain): the `$JAVAC`/`$JAVA` override
  first, then the `[toolchain] compiler`/`runtime` selection (an explicit path, or a
  distribution/version discovered among the installed JDKs), then `$JAVA_HOME/bin/<tool>`, and finally
  the bare name on `PATH`. Discovery/spawning live in that feature; the pure planner
  (`Invocation`/`ToolResolver`) still injects the platform classpath separator (`:` on Unix, `;` on
  Windows) and touches nothing.
- **Exit codes** — the JDK tool's exit code propagates; a signal-terminated tool fails with `1`.

## The manifest (`jals.toml`)

Every key is optional and falls back to its default; keys are kebab-case and grouped into
`[package]`, `[build]`, `[run]`, `[toolchain]`, the repeatable `[[bin]]`, and `[dependencies]`. The
defaults encode the Maven-style `src/main/java` → `target/classes` layout, so an empty (or absent)
section just uses them.

```toml
[package]
name = "hello"
version = "0.1.0"
# features = ["java25"]            # language features (release presets + individual); gates analysis, not javac
# default-run = "server"           # which [[bin]] `jals run` runs when several exist

[build]
source-dirs = ["src/main/java"]   # -sourcepath roots, also scanned for .java files
classes-dir = "target/classes"    # javac -d
release = 21                       # javac --release N
# source = 17                      # javac --source N  (only when release is unset)
# target = 17                      # javac --target N  (only when release is unset)
classpath = ["libs/guava.jar"]    # -classpath entries (jars or dirs)
javac-flags = ["-Xlint:all"]      # appended verbatim, before the source files

# [toolchain]                       # which javac/java to use (defaults to the system tools)
# compiler = { distribution = { name = "temurin", version = 21 } }  # discover an installed JDK
# runtime  = "system"               # "system" | "builtin" | { path = "…" } | { distribution = { … } }

[run]
main-class = "com.example.Main"   # entry point for `jals run` (used only when no [[bin]] exists)

# Or declare several named entry points instead of [run] main-class:
# [[bin]]
# name = "server"
# main-class = "com.example.Server"
#
# [[bin]]
# name = "cli"
# main-class = "com.example.Cli"

[dependencies]
# A local jar (file:// URL or a bare path relative to the manifest dir):
guava = { jar = "libs/guava.jar" }
# A remote jar, downloaded into target/jals/deps and cached:
junit = { jar = "https://repo1.maven.org/maven2/junit/junit/4.13.2/junit-4.13.2.jar" }
# An optional companion sources jar enables go-to-definition into the library's real .java source:
gson = { jar = "https://example.com/gson-2.11.jar", sources = "https://example.com/gson-2.11-sources.jar" }
```

### `[package]`

| Key | Type | Default | Status |
| --- | --- | --- | --- |
| `name` | string | — | ℹ️ informational (reserved for future jar packaging) |
| `version` | string | — | ℹ️ informational |
| `features` | array of feature names | `[]` | the language features the project enables (Cargo's `[features]`, additive-only). A **Java release preset** (`"java8"` … `"java25"`) selects everything that release stabilized — each preset implies the one before it, so `java25 ⊇ java24 ⊇ …` holds from one entry — while an **individual feature** name (`"module-imports"`, `"compact-source-files"`) turns on a single otherwise-preview construct (the analogue of one `--enable-preview` flag). A *language-feature gate* for analysis only (the linter / LSP), **not** passed to `javac` — the compile knobs stay `[build] release`/`source`/`target`. E.g. `["java24"]` flags a top-level `main` (compact source files) via the `compact-source-file` lint and an `import module …;` (module import declarations) via the `module-import` lint — both preview features there, permanent in `java25`. Empty/unset means no feature gate. The name set is a closed enum (an unknown name is a parse error), so jals-specific dialect features can join later. |
| `default-run` | string | — | which `[[bin]]` `jals run` runs when several exist and `--bin` is not given. Must name a declared `[[bin]]`. |

### `[build]`

| Key | Type | Default | Maps to |
| --- | --- | --- | --- |
| `source-dirs` | array of strings | `["src/main/java"]` | `-sourcepath` (joined) **and** the roots scanned for `.java` files |
| `classes-dir` | string | `"target/classes"` | `javac -d` (also the dir `jals clean` removes) |
| `release` | integer | — | `--release N` — sets source level, target level, and bootclasspath together; when present, `source`/`target` are ignored |
| `source` | integer | — | `--source N` — only when `release` is unset |
| `target` | integer | — | `--target N` — only when `release` is unset |
| `classpath` | array of strings | `[]` | `-classpath` (joined with the platform separator); omitted entirely when empty |
| `javac-flags` | array of strings | `[]` | appended **verbatim** after the generated flags, before the source files — an escape hatch for anything the manifest does not model yet |

### `[run]`

| Key | Type | Default | Maps to |
| --- | --- | --- | --- |
| `main-class` | string | — | the fully-qualified entry point passed to `java`, used **only when no `[[bin]]` is declared**. `jals run` errors if it is unset, no `[[bin]]` exists, and `--main-class` is not given. The run classpath is `classes-dir` followed by `classpath`. |

### `[toolchain]`

Selects **which `javac` compiles** the project and **which `java` runs** it — the two are chosen
independently, so a project can compile with one JDK and run on another. The rough analogue of
`rust-toolchain.toml`. Omitting the table (or a field) uses the system tools, so an existing manifest
is unaffected.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `compiler` | string or table | `"system"` | which `javac` to use (see the forms below) |
| `runtime` | string or table | `"system"` | which `java` to use (same forms) |

Each value is one of four forms — a keyword string, or a tagged table naming the form (the enum's
plain serde representation; nothing is classified from a free-form string):

| Form | Example | Meaning |
| --- | --- | --- |
| `"system"` | `"system"` | the system tools — identical to omitting the field |
| `"builtin"` | `"builtin"` | the **in-process backend** instead of a JDK tool — today a *dummy* (compile copies each source into the `classes-dir` unchanged, nothing is compiled; run is a successful no-op, nothing is executed), the placeholder a future embedded compiler replaces behind the same selector |
| `{ path = "…" }` | `{ path = "/opt/jdk-21" }`, `{ path = "./jdk/bin/javac" }` | an explicit JDK home directory (the tool is `<path>/bin/<tool>`) or the tool binary itself; a relative path resolves against the manifest dir. Used verbatim — a non-existent path errors rather than silently reverting to `PATH`. |
| `{ distribution = { … } }` | `{ distribution = { name = "temurin", version = 21 } }` | a JDK to **discover** among the installed ones by distribution and/or version; both keys are optional (a bare `version` matches any distribution, a bare `name` any version) |

A JDK tool is resolved in this order: the `$JAVAC`/`$JAVA` environment override (wins
unconditionally, for CI/back-compat) → the `[toolchain]` selection above → `$JAVA_HOME/bin/<tool>` →
the bare name on `PATH`. A distribution selector is matched against the JDKs found under the common
install locations (SDKMAN, IntelliJ `~/.jdks`, `~/.jdk`, `/usr/lib/jvm`, the macOS JVM bundle
directory) — **discovery only**; automatically downloading a missing JDK (rust-toolchain style) is
future work, and an un-discovered distribution falls back to the system tools. A `"builtin"`
selector skips program resolution entirely — no process is spawned for that step; the two selectors
are independent (each is its own enum, matched by its own `select` factory), so e.g.
`compiler = "builtin"` with the runtime unset dummy-"compiles" but still runs with the real `java`.

### `[[bin]]`

A repeatable array-of-tables declaring **named entry points** (Cargo's `[[bin]]`). Both keys are
**required**.

| Key | Type | Maps to |
| --- | --- | --- |
| `name` | string | the bin's selector for `--bin <name>` and `[package] default-run` |
| `main-class` | string | the fully-qualified class `java` runs for this bin |

Because `javac` compiles **all** discovered sources in one pass, a `[[bin]]` is *not* a separate
compilation unit (unlike Rust). It only selects which `main-class` `java` runs — it never changes
what is compiled. `jals build --bin <name>` therefore only validates that the name exists; the
compile command is unchanged.

The run target for `jals run` is resolved in this order (`RunTarget::resolve`):

1. `--main-class <FQCN>` — runs that class directly, bypassing the manifest.
2. `--bin <name>` — the `[[bin]]` with that name (error if none matches).
3. `[package] default-run` — when several `[[bin]]` exist.
4. the single `[[bin]]`, when exactly one is declared.
5. `[run] main-class` — only when **no** `[[bin]]` is declared (full backward compatibility).

Once any `[[bin]]` exists, `[run] main-class` is ignored for selection. Duplicate bin names and a
`default-run` that names no bin are rejected at manifest load (`Manifest::validate`).

### `[dependencies]`

A table mapping a **dependency name** to its spec (Cargo's `[dependencies]`). Each entry picks exactly
one **primary form** — `jar` (compiled classes), `git` (a checked-out source repo), or `path` (a local
source tree) — plus form-specific options:

| Key | Type | Form | Maps to |
| --- | --- | --- | --- |
| `jar` | string | jar | a `.jar` location: an `https://`/`http://` URL (downloaded and cached), a `file://` URL, or a bare path (relative to the manifest dir) |
| `sources` | string | jar | *optional* companion **sources** `.jar` (the library's `.java`), located like `jar`. Editor go-to-definition only — never a compile or analysis input |
| `recursive` | bool | jar | *optional* (default `false`) — recursively unpack the jar's **bundled jars** (`*.jar` members nested inside it, as in a fat jar's `BOOT-INF/lib/*.jar`) onto the classpath, at any depth |
| `git` | string | git | a repository URL to clone for its `.java` source |
| `branch` / `tag` / `rev` | string | git | *optional*, **at most one** — which commit to check out (default: the repo's default branch) |
| `path` | string | path | a local directory tree of `.java` source (relative to the manifest dir) |
| `dir` | string | git, path | *optional* source root **within** the repo/dir (e.g. `core/src/main/java`); omit to auto-detect (`src/main/java` → `src` → the root) |

```toml
[dependencies]
guava = { jar = "libs/guava.jar" }                          # local path
other = { jar = "file:///opt/libs/other.jar" }              # file:// URL
junit = { jar = "https://example.com/junit-4.13.2.jar" }    # remote, downloaded
# A sources jar lets the editor jump into the library's real .java on go-to-definition:
gson  = { jar = "https://example.com/gson-2.11.jar", sources = "https://example.com/gson-2.11-sources.jar" }
# A fat jar bundles its dependencies as nested jars; `recursive` unpacks them onto the classpath:
app   = { jar = "libs/app-all.jar", recursive = true }
# Source directly from a git repo (pin with branch/tag/rev), or a local checkout:
mylib = { git = "https://github.com/owner/mylib", tag = "v1.2" }
local = { path = "../sibling-lib" }
# A non-standard layout names its source root explicitly:
core  = { git = "https://github.com/owner/mono", rev = "abc123", dir = "core/src/main/java" }
```

A **`jar`** dependency is resolved to a local `.jar` by the **host** (`jals-cli`/`jals-lsp` via
`jals-classpath`) and folded into the classpath for both **analysis** (`jals lint`, the LSP) and
**compilation** (`jals build`/`run` add it to `javac`/`java`'s `-classpath`). Remote jars are
downloaded once into `target/jals/deps` and cached. With **`recursive = true`** the host also unpacks
the jar's **bundled jars** — the `*.jar` members a fat jar nests inside itself (e.g. a Spring-Boot
layout's `BOOT-INF/lib/*.jar`), which the classpath loader otherwise skips — into
`target/jals/deps/nested`, recursively (a jar-in-jar-in-jar resolves too), and adds them to the same
classpath, so the bundled libraries are available for both analysis and compilation. Its optional
`sources` jar is purely an **editor** aid: `jals-lsp` extracts the `.java` into
`target/jals/deps/sources` and points go-to-definition at the real declaration; never a compile or
analysis classpath input. When a jar ships **no** `sources` jar, `jals-lsp` still makes
go-to-definition work: it decompiles a `.java` **skeleton** from each classpath `.class` (every type
and member declaration, with increasingly real method bodies reconstructed from the bytecode) into
`target/jals/deps/decompiled` and navigates there — so jump-to-definition lands on a declaration for
*any* library type, with a real `sources` jar taking precedence when present. The decompiled output
is always valid Java (an un-reconstructable method falls back to a safe placeholder body rather than
emit broken source). (Editor-only; never a compile or `lint` input.)

A **`git`** / **`path`** dependency supplies `.java` **source** directly. The host clones each git repo
(into `target/jals/deps/git`, the requested ref checked out) or reads each path in place, locates its
`.java` source root, and uses those `.java` two ways:

- **compilation** (`jals build`/`run`): the located `.java` are passed to `javac` as additional
  sources, compiled alongside the project's own into the `[build] classes-dir` — so a project that
  depends on a source dependency builds and runs. (The dependency's `.class` land in `classes-dir`,
  already first on the run classpath, so `jals run` needs nothing extra.)
- **editor analysis + navigation** (`jals-lsp`): the same `.java` are folded into the LSP index as
  library-source types, so references resolve for inference, hover, completion, and go-to-definition
  lands in the real source.

They are **not** a `jals lint` input, so a `jals lint` run may report unresolved types for code that
uses a source dependency even though `jals build` compiles it.

A malformed entry is rejected when the manifest loads, in two stages. `Dependency` is a
`#[serde(untagged)]` enum whose `Jar`/`Git`/`Path` variants each `deny_unknown_fields`, so the
**structural** errors — more than one primary form (`{ jar, git }`), no form at all (`{}`), or a field
misplaced for its form (`branch` without `git`, `sources` with `git`) — match no variant and fail at
**parse** time (a TOML error). The remaining **value-level** errors — an empty value, an unknown URL
scheme, conflicting git refs (`branch` + `tag`) — are caught by `Manifest::validate`. A *runtime*
failure (a download/clone error, a local jar/dir that does not exist) is a best-effort warning that
skips just that dependency, never aborting. `jals-build` itself only *classifies* each spec
(`Manifest::dependency_sources` → `DependencySource`, `dependency_source_jars` → the `sources` jar,
`dependency_source_dirs` → `SourceDependency::{Git, Path}`), staying pure — it performs no I/O.
Maven-coordinate resolution (`group:artifact:version` + transitive download + lockfile) is still future
work (see the roadmap §3).

## Usage

```sh
jals init my-app            # scaffold ./my-app (jals.toml, src/main/java/Main.java, .gitignore)
cd my-app
jals build                  # compile with javac
jals build --dry-run        # print the javac command without compiling
jals run                    # compile, then run the resolved entry point
jals run --bin server       # run the [[bin]] named "server"
jals run -- arg1 arg2       # ...passing args to the program
jals run --main-class com.example.Other
jals clean                  # remove target/classes
```

## Library API

Every entry point is an associated function on the type it produces or acts on — no free
functions (the crate follows the workspace's `no-free-functions` rule):

```rust
impl Invocation {
    pub fn build(req: &CompileRequest<'_>, path_sep: char) -> Self;
    pub fn run(req: &RunRequest<'_>, path_sep: char) -> Self;
}

impl RunTarget {
    pub fn resolve<'m>(manifest: &'m Manifest, bin: Option<&str>)
                    -> Result<&'m str, ResolveTargetError>;
}

impl CleanTargets {
    pub fn paths(manifest: &Manifest, project_root: &Path) -> Vec<PathBuf>;
}

impl InitOptions {
    pub fn scaffold(&self) -> Vec<ScaffoldFile>;
}
```

`Invocation { program, args }` is a resolved command line as pure data; `display_command()`
renders it for `--dry-run`/`-v`. A `CompileRequest` carries an `extra_sources` list — the
`git`/`path` source deps' `.java`, appended after `sources` — and both requests carry an
`extra_classpath` of already-resolved jar paths (the host's resolved `[dependencies]` jars),
appended after the `[build] classpath` entries on `javac`/`java`'s `-classpath`; the classpath
separator is supplied by the backend planning the command (the requests stay tool-agnostic).
`RunTarget::resolve`
picks the `main-class` `jals run` should execute from `[[bin]]`/`default-run`/`[run] main-class`.
`InitOptions { name }.scaffold()` (for `jals init`) and `CleanTargets::paths` (for `jals clean`)
round out the pure planning surface.

The `ManifestExt` trait (`jals-build`'s host-side extension of `jals_config::Manifest`) adds the
path-resolving half: `Manifest::from_file` loads, parses, and validates (`Manifest::validate`, an
inherent method in `jals-config`) `jals.toml`; `Manifest::discover_path` locates it.
`Manifest::source_roots` and `Manifest::classpath_entries` resolve the `[build] source-dirs` and
`[build] classpath` entries against the manifest directory, as absolute paths, for the host to
read: sources to compile, and the classpath jars/dirs the host (`jals-classpath`) reads `.class`
files from to feed `jals-hir`'s analysis. `Manifest::dependency_sources` classifies each `jar`
`[dependencies]` entry into a `DependencySource::{Url, Path}` (pure, no I/O), `Manifest::dependency_source_jars`
does the same for the optional `sources` jars, and `Manifest::dependency_source_dirs` classifies
each `git`/`path` entry into a `SourceDependency::{Git, Path}` (`jals_config::Dependency` is itself
the classification — a `#[serde(untagged)]` enum of `Jar`/`Git`/`Path` variants, each
`deny_unknown_fields`, so serde picks the form at parse time and rejects co-occurring/missing/misplaced
forms as a parse error; `manifest_ext.rs`'s private `DependencySource::{from_jar, from_sources}` /
`SourceDependency::from_dependency` classifiers back the three `ManifestExt` methods above); the host
(`jals_classpath::ProjectInputs::assemble`) resolves project bytes and external locators into a
verified `ArtifactCache`, so `jals lint` / the LSP / `jals build` see external library types from
named `jar` dependencies, and `jals-lsp` additionally extracts the `sources` jars and folds each
`git`/`path` source tree into its index for go-to-definition into (and analysis of references to) library
source.

## Development

```sh
cargo test  -p jals-build                                          # manifest + invocation + target + clean + scaffold tests
cargo clippy -p jals-build --all-targets --all-features -- -D warnings
cargo build --release --target wasm32-unknown-unknown -p jals-build  # stays wasm-compatible
```

---

# Roadmap

`jals-build` today is a thin, faithful `javac`/`java` wrapper. The goal is to grow it into a
**Cargo-for-Java** front end: dependency management, packaging, testing, and richer build
configuration. Each item below names its Cargo analogue (or marks a Java-specific extension).

**The architectural rule for every item:** `jals-build` stays **pure** — no filesystem, process,
or network I/O, so it keeps building for `wasm32`. New side effects (downloading a jar, running
a test runner, writing a jar archive) live in `jals-cli`; this crate only *plans* them. A
resolved dependency classpath, for instance, is fed into `Invocation::build` exactly as the
discovered source list is fed in today.

## 1. Commands to add

| Command | Cargo analogue | What it does | Needs |
| --- | --- | --- | --- |
| `jals new <path>` | `cargo new` | Scaffold into a **new** directory (vs. `init`, which is in-place). Mostly a thin alias over today's `InitOptions::scaffold`. | reuse `InitOptions::scaffold` |
| `jals check` | `cargo check` | Compile for diagnostics only, no runnable output (`javac -proc:only` / throwaway `-d`), or fold in `jals fmt --check` + `jals lint`. | a "check" invocation variant |
| `jals test [filter]` | `cargo test` | Compile test sources and run them via the JUnit Platform launcher; filter by class/method. | `[test]` section, `test-source-dirs`, a JUnit dep on the classpath, a runner invocation builder |
| `jals doc` | `cargo doc` | Run `javadoc` into `target/doc`; optionally open it. | a `javadoc` invocation builder, `[doc]` options |
| `jals jar` / `jals package` | `cargo package` | Produce a runnable jar (`Main-Class` in the manifest), optionally a fat/uber jar bundling classpath deps. | a `jar`/archive plan, `[package]` metadata |
| `jals add <coord>` / `jals remove <coord>` | `cargo add` / `cargo remove` | Edit `[dependencies]` in `jals.toml`. | manifest **writing** + Maven coordinate parsing |
| `jals tree` | `cargo tree` | Print the resolved (transitive) dependency tree. | a dependency resolver (§3) |
| `jals fetch` | `cargo fetch` | Download and cache dependencies without building. | a dependency resolver (§3) |
| `jals update` | `cargo update` | Re-resolve and update locked dependency versions. | a lockfile + resolver (§3) |
| `jals metadata` | `cargo metadata` | Emit the resolved manifest + dependency graph as JSON for external tooling. | resolver (§3) |
| `jals install` | `cargo install` | Build and install a runnable jar / launcher script. | packaging (§4) |
| `jals publish` | `cargo publish` | Publish artifacts to a Maven repository. | packaging (§4) + repo auth |
| `jals bench` | `cargo bench` | Run a JMH benchmark harness. | a JMH integration |

## 2. Manifest sections & keys to add

### `[package]` expansion (Cargo `[package]`)

`description`, `authors`, `license`, `repository`, `homepage`, and `keywords`. These become a jar's
`MANIFEST.MF` / POM metadata on packaging. (`default-run` is already implemented — see
[`[[bin]]`](#bin); `features` too, as an analysis-only language-feature gate — see [`[package]`](#package).
Making a `features` release preset also imply a default `javac --release` is still open.)

### `[build]` additions

| Key | Maps to | Notes |
| --- | --- | --- |
| `encoding` | `javac -encoding` | source encoding; default `UTF-8` |
| `enable-preview` | `--enable-preview` (with `--release`) | preview-language features (also needed at `java` run time) |
| `debug` / `debug-info` | `-g` / `-g:none` | debug-symbol level |
| `parameters` | `-parameters` | keep formal parameter names at runtime |
| `lint` / `warnings` | `-Xlint:all`, `-Werror` | typed `-Xlint` config instead of raw `javac-flags` |
| `annotation-processors` | `-processor`, `-processorpath`, `-proc:` | annotation processing |
| `resource-dirs` | (copy step) | `src/main/resources` → `classes-dir`, like Maven |
| `module` / `module-path` | `--module-path`, `--module-source-path` | JPMS (modular) builds vs. the classpath |
| `target-dir` | `-d` parent | override the `target/` location (also a CLI flag, §6) |
| `incremental` | (skip unchanged) | recompile only stale sources — needs timestamp/hash tracking in `jals-cli` |

### `[run]` additions (Cargo `[profile]`/run)

`jvm-args` (`java -X…`/`-D…`), `env` (environment variables), `args` (default program args),
`working-dir`, and `enable-preview` (the `java`-side flag).

### New sections

| Section | Cargo analogue | Purpose |
| --- | --- | --- |
| `[dependencies]` | `[dependencies]` | **partly done**: the `{ jar = "url-or-path" }` form is wired (downloaded/local jars folded into the analysis + compile classpath, plus an optional `sources` jar for editor go-to-definition), as are the source forms `{ git = "url", branch/tag/rev, dir }` and `{ path = "...", dir }` (cloned/read `.java` folded into the LSP index for analysis + navigation **and** compiled by `jals build`/`run` as extra `javac` sources, not a `lint` input); Maven coordinates (`group:artifact:version`) + transitive resolution are §3 |
| `[dev-dependencies]` | `[dev-dependencies]` | test/bench-only deps (JUnit, etc.) |
| `[toolchain]` | `rust-toolchain.toml` | **partly done**: `compiler`/`runtime` select `javac`/`java` independently — `"system"`, `"builtin"`, an explicit `{ path = "…" }`, or a `{ distribution = { name, version } }` discovered among the installed JDKs (SDKMAN / `~/.jdks` / `~/.jdk` / `/usr/lib/jvm` / macOS). Still to come: **automatic download** of a missing JDK (rust-toolchain style, e.g. via the foojay disco API) into a per-user cache, and letting a `[package] features` release preset default `[build] release`. |
| `[repositories]` | (registries) | Maven repository URLs; default Maven Central |
| `[profile.dev]` / `[profile.release]` | `[profile.*]` | debug vs. optimized/stripped builds (`-g` vs. `-g:none`, lint levels) |
| `[workspace]` / `[[module]]` | `[workspace]` | multi-module builds with a shared lockfile |
| `[lints]` | `[lints]` | wire `jals-lint` / `-Xlint` configuration |

## 3. Dependency management (the largest gap)

Java's defining build feature, and the biggest piece missing. A `[dependencies]` table of Maven
coordinates would be **resolved** (transitively) into the `classpath` that `Invocation::build`
already consumes. The pure/`wasm32` split is preserved by keeping resolution's I/O in
`jals-cli` (or a new host-only crate, e.g. `jals-resolve`):

- **Resolver** — parse POMs, walk the transitive graph, pick versions on conflict (nearest-wins
  or a `[patch]`/override mechanism).
- **Local cache** — reuse `~/.m2/repository` or a dedicated `~/.jals` cache.
- **Lockfile** — a `jals.lock` pinning resolved versions + checksums for reproducible builds
  (drives `jals fetch` / `jals update` / `--locked` / `--frozen`).
- **Network fetch** — download missing jars from `[repositories]`; gated by `--offline`.

`jals-build` itself only needs the *result*: the resolved classpath, fed in like sources are
today. No part of this changes the crate's purity.

**Already wired (analysis + compile side):** the *consumption* of a classpath is done, and the
explicit-jar form of `[dependencies]` now resolves end-to-end. `Manifest::classpath_entries`
resolves the `[build] classpath` to paths, and `Manifest::dependency_sources` classifies each
`[dependencies]` `{ jar = "..." }` into a URL or local path; the host-only `jals-classpath` crate
reads the `.class` bytes out of typed project/cache entries (and **downloads** remote jars into the
SHA-256 verified artifact cache via `ProjectInputs::assemble`) and parses them with `jals-classfile`; and
`jals-hir`'s `ProjectIndex::builder().with_classpath()` folds them in so external library types resolve in
`jals lint` and the language server, while `jals build`/`run` put the same jars on `javac`/`java`'s
classpath. What is still missing is the *resolver* above — turning **Maven coordinates** into those
classpath entries (POM walking + transitive graph + version conflict resolution + lockfile). Until
then, a project lists explicit jar URLs/paths under `[dependencies]` (or jars/dirs under
`[build] classpath`) by hand. JDK standard-library classes are not loaded this way either; the
embedded `java.lang`/`java.util` stubs stand in for them (reading the JDK's `jimage`/`modules` is a
separate, still-unwired step).

## 4. Packaging

| Capability | Cargo analogue | Notes |
| --- | --- | --- |
| Plain jar (`Main-Class` manifest) | `cargo build --release` artifact | a `jar` invocation/archive plan from `[package]` + `[run] main-class` |
| Fat / uber jar | — | bundle dependency jars into one runnable archive |
| `jpackage` / native image | — | OS installers / GraalVM native binaries |
| Source & javadoc jars | — | `-sources.jar` / `-javadoc.jar` for publishing |

## 5. Testing

`jals test` compiles `src/test/java` against the main classes + `[dev-dependencies]`, then runs
the JUnit Platform launcher (or TestNG). Needs: a `[test]`/`test-source-dirs` config, a test
classpath plan, a runner-`Invocation` builder, and result reporting in `jals-cli`. Pairs with
`jals bench` (JMH) once dependencies (§3) exist.

## 6. Operational / CLI flags (language-agnostic)

| Flag | Cargo analogue | Notes |
| --- | --- | --- |
| `--release` / `--profile <name>` | `--release` / `--profile` | select a `[profile.*]` |
| `--offline` / `--frozen` / `--locked` | same | dependency-resolution modes |
| `--target-dir <DIR>` | `--target-dir` | override `target/` (generalizes today's `--out-dir`) |
| `--color auto\|always\|never` | `--color` | colored output |
| `-q`/`--quiet`, `-v`/`-vv` | same | verbosity levels (build already has `-v`) |
| `--workspace` / `-p <pkg>` | same | multi-module selection |
| `--manifest-path` everywhere | `--manifest-path` | already on build/run/clean; extend to `init`/future commands |

## Out of scope (for `jals-build` the crate)

Anything with side effects stays in `jals-cli` (or a future host-only helper crate): process
spawning, filesystem walking/writing, network fetches, and the dependency cache. This crate's
remit is the **pure planning layer** — manifests in, command plans / path lists / scaffold files
out — which is exactly what keeps it deterministic, unit-testable without a JDK, and
`wasm32`-buildable.

## Suggested priority

By Java-user impact:

1. **High-value `[build]` keys** — `resource-dirs`, `encoding`, `enable-preview`, `-Xlint`
   (cheap, immediately useful, no new infrastructure).
2. **`jals test`** — JUnit integration; the first thing most projects need after `build`/`run`.
3. **Dependency management (§3)** — `[dependencies]` + resolver + `jals.lock`. The highest
   impact and the largest effort; unblocks `add`/`remove`/`tree`/`fetch`/`update`.
4. **Packaging (§4)** — `jals jar`, then fat jars.
5. **The rest** — `doc`, profiles, workspaces, `publish`, `bench`.
