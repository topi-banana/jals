# jals-build

Cargo-style build orchestration for Java projects — the engine behind `jals build` / `jals run`
/ `jals clean` / `jals init`.

A [`jals.toml`](#the-manifest-jalstoml) manifest is the Java analogue of `Cargo.toml`: it says
where the sources live, where compiled classes go, which Java release to target, what is on the
classpath, and optionally which Rhai script runs before compilation. This crate turns that manifest
and already-resolved inputs into `javac`/`java` plans, clean keys, or scaffold files. With the `rhai`
feature it can also evaluate the script against revisioned `jals-storage` project data, without
giving the script direct host access:

```
jals.toml + project snapshot ─▶ optional Rhai pre-build ─▶ generated files/directives
              │                                                    │
              └──────────────────────┬─────────────────────────────┘
                                     ▼
discovered .java files ───────▶ CompileRequest / RunRequest ─▶ javac / java
InitOptions ──────────────────▶ [ScaffoldFile]       CleanTargets ─▶ [DirKey]
```

`jals-cli` owns manifest discovery, host source walking, scaffold writes, and clean removal. Spawning
`javac`/`java` (and discovering installed JDKs for `[toolchain]`) lives in the default-on `native`
feature (`SubprocessToolchain`, plus the `<dyn Compiler>::select` / `<dyn Runtime>::select`
factories). The toolchain abstraction is a pair of object-safe traits, `Compiler` and `Runtime`, and
the subprocess backend is not their only implementation: the core also ships
`BuiltinToolchain`, the **in-process backend** selected by `[toolchain] compiler/runtime =
"builtin"` — today a *dummy* (compile copies each source into the `classes-dir` unchanged; run is a
successful no-op) whose I/O goes through a revisioned `jals_storage::ProjectStorage`; memory and
native adapters obey the same transaction contract, and a real embedded compiler later replaces
the copy step without touching the seam. Because each step is selected from its own enum and driven as its
own `&dyn Compiler` / `&dyn Runtime`, mixed selections need no routing composite. The core — the
`Manifest` model, the `Invocation` planner, and the `Compiler`/`Runtime` traits plus the
filesystem-free `ToolResolver` and `BuiltinToolchain` — remains deterministic and testable with no
JDK installed.

The optional `rhai` feature adds [`execute_build_script`](#rhai-build-scripts). It is also portable:
`jals-build --no-default-features --features rhai` is `no_std + alloc`, uses only typed project
storage and its verified artifact cache, and builds for `wasm32-unknown-unknown`. The browser
playground uses that exact configuration.

Transitive project discovery is deliberately a deeper layer in `jals-project`, not this planner.
It discovers stable path/Git/JAR graph nodes, preprocesses every unique node dependency-first, and
projects only verified source/classpath artifacts back into the already-resolved inputs consumed
here. This keeps command planning independent of graph acquisition and preserves the portable
`jals-build` boundary.

## What it does today

Four subcommands are wired through `jals-cli`:

| Command | Backed by | What it does | Flags |
| --- | --- | --- | --- |
| `jals build` | `execute_build_script` + `jals-project` + `Invocation::build` | Run the root pre-build script, preprocess the transitive dependency graph, discover `.java` sources, build the `javac` command, and run it. | `--manifest-path <PATH>`, `--dry-run`, `-v`/`--verbose`, `--out-dir <DIR>`, `--bin <NAME>` |
| `jals run` | `execute_build_script` + `jals-project` + `RunTarget::resolve` + invocations | Run the root and dependency pre-build phases, compile the complete source graph, then run the resolved entry point with `java`. Compilation must succeed first. | `--manifest-path <PATH>`, `--dry-run`, `-v`/`--verbose`, `--main-class <FQCN>`, `--bin <NAME>`, `-- <args>` |
| `jals clean` | `CleanTargets::keys` | Remove `classes-dir` and `target/jals/build`, including stale outputs after a script is removed. A never-built project succeeds quietly. | `--manifest-path <PATH>`, `--dry-run` |
| `jals init [PATH]` | `InitOptions::scaffold` | Scaffold a new project: `jals.toml`, a starter `Main.java`, and a `.gitignore`. Refuses to overwrite an existing `jals.toml`. | `--name <NAME>` |

Common behavior, all implemented in `jals-cli` on top of this crate:

- **Manifest discovery** — `Manifest::discover_path` searches upward from the cwd for `jals.toml`
  (like Cargo). The project root is the manifest's parent directory; every manifest path is
  resolved relative to it. A missing manifest is an **error** (there is nothing to build),
  unlike the formatter/linter configs where a missing file means "use defaults".
- **Source discovery** — after the optional build script runs, at least one project or generated
  `.java` source must be present. Without a generated source, every `source-dirs` entry must exist;
  generated sources permit an otherwise absent or empty ordinary source root. Sources are passed to
  `javac` last, in sorted order.
- **`--dry-run`** prints the exact command(s) (via `Invocation::display_command`, which quotes
  whitespace) and exits without compiling/running/deleting. A configured root script still runs and
  reconciles its storage output, and dependency scripts still prepare verified artifacts, because
  those registrations are needed to plan that exact command. `-v`/`--verbose` prints the same
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
# script = { type = "rhai", file = "build.rhai" } # optional pre-javac phase
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
# A remote jar, downloaded into the verified artifact cache:
junit = { jar = "https://repo1.maven.org/maven2/junit/junit/4.13.2/junit-4.13.2.jar" }
# An optional companion sources jar enables go-to-definition into the library's real .java source:
gson = { jar = "https://example.com/gson-2.11.jar", sources = "https://example.com/gson-2.11-sources.jar" }
# Source projects can be local or Git-backed and are discovered transitively:
shared = { path = "../shared" }
core = { git = "https://github.com/example/mono", rev = "abc123", dir = "core" }
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
| `script` | tagged table | — | optional pre-`javac` build phase; currently `{ type = "rhai", file = "build.rhai" }` |
| `source-dirs` | array of strings | `["src/main/java"]` | `-sourcepath` (joined) **and** the roots scanned for `.java` files |
| `classes-dir` | string | `"target/classes"` | `javac -d` (also the dir `jals clean` removes) |
| `release` | integer | — | `--release N` — sets source level, target level, and bootclasspath together; when present, `source`/`target` are ignored |
| `source` | integer | — | `--source N` — only when `release` is unset |
| `target` | integer | — | `--target N` — only when `release` is unset |
| `classpath` | array of strings | `[]` | `-classpath` (joined with the platform separator); omitted entirely when empty |
| `javac-flags` | array of strings | `[]` | appended **verbatim** after the generated flags, before the source files — an escape hatch for anything the manifest does not model yet |

### Rhai build scripts

Enable the optional pre-build phase with an inline tagged table:

```toml
[build]
script = { type = "rhai", file = "build.rhai" }
```

The corresponding Rust model is `BuildScript::Rhai { file }`; `tag_name()` returns the exact serde
tag used in the manifest:

```rust
use jals_config::BuildScript;

let script = BuildScript::Rhai {
    file: "build.rhai".into(),
};
assert_eq!(script.tag_name(), "rhai");
```

`file` must be a non-root portable project-relative file path outside both `[build] classes-dir`
and the managed `target/jals/build` tree, since `jals clean` removes both. The CLI executes the root
project's script before source discovery and `javac` for both `build` and `run`. Its complete output
retains the existing root semantics: generated files are reconciled into root storage, and
registered sources/classpath plus `javac`/JVM arguments and compile/run environment entries affect
the later command. The LSP and playground run that same root adapter for analysis but do not spawn
`javac`/`java`; root script failures are diagnosed and ordinary root analysis continues.

The root adapter receives a `ProjectStorage` aggregate rather than a host path. It evaluates against
one immutable `ProjectView`, buffers generated files and directives, then commits files in one
revision-checked transaction only after successful evaluation. Immutable dependency preparation
uses the same evaluator without that source-storage commit. Scripts get four scope objects; `tasks`
records a typed DAG while the first three retain the direct APIs below:

| Object | Method | Effect |
| --- | --- | --- |
| `project` | `read(path)` | Read a project file as an array of bytes (`0..=255`). |
| `project` | `read_text(path)` | Read a UTF-8 project file as a string. |
| `project` | `exists(path)` | Test whether a project-relative file or directory exists. |
| `project` | `read_dir(path)` | List direct child paths in deterministic order. |
| `project` | `walk_files(path)` | List all files below a directory in deterministic order. |
| `output` | `write(path, bytes)` | Buffer bytes below `target/jals/build/rhai/out` and return an `OutputPath`. |
| `output` | `write_text(path, text)` | Buffer UTF-8 text below the same output root and return an `OutputPath`. |
| `build` | `env(name)` | Read a value from the environment map explicitly supplied by the host; returns `()` when absent. |
| `build` | `rerun_if_changed(path)` | Track one project file for cache invalidation. |
| `build` | `rerun_if_env_changed(name)` | Track one supplied environment value for cache invalidation. |
| `build` | `add_source(path)` | Add a project file or returned `OutputPath` to the later source set. |
| `build` | `add_classpath(path)` | Add a project file or returned `OutputPath` to the classpath. |
| `build` | `add_javac_arg(arg)` / `add_jvm_arg(arg)` | Append compiler or JVM arguments in call order. |
| `build` | `set_compile_env(name, value)` / `set_run_env(name, value)` | Add environment entries to the compiler or runtime request. |
| `build` | `warning(message)` / `error(message)` | Report a non-fatal warning or a fatal diagnostic. Any error prevents publication. |
| `build` | `metadata(key, value)` | Return deterministic host-readable metadata without changing a tool invocation. |

Root scripts can use these `tasks` methods. They only record work; network/archive effects run
asynchronously after Rhai evaluation and capability preflight succeed:

| Method | Result/effect |
| --- | --- |
| `https_url(url)` | Typed HTTPS URL. A fetch still requires an expected digest and byte limit. |
| `project_jar(path)` | Typed JAR from the immutable project snapshot. |
| `sha1(hex)` / `sha256(hex)` / `bytes(n)` | Typed verification and size values. |
| `fetch_json(url, digest, max)` / `fetch_jar(...)` / `fetch_text(...)` | Verified cache-first artifact fetch (JSON, JAR, or UTF-8 text). |
| `json_at(json, path)` / `json_find_string(json, path, field, value)` | Typed JSON projection without exposing fetched values to Rhai. |
| `json_url(json, path)` / `json_sha1(...)` / `json_sha256(...)` / `json_u64(...)` | Values for a dependent fetch, resolved by the host DAG executor. |
| `extract_java(jar, prefix)` | Safe `.java` source tree below `prefix`, with the prefix stripped. |
| `nested_jar(jar, member)` | Extract one nested `.jar` member and treat it as a JAR. |
| `remap_jar(jar, mappings)` | Deobfuscate a JAR with Mojang/ProGuard mappings text (hierarchy-aware). |
| `merge_jars(base, overlay)` | Deterministic JAR union; overlay wins path conflicts. |
| `decompile_java(jar, prefix)` | Compile-oriented skeleton source tree below `prefix`. |
| `add_classpath(jar)` | Add a task-produced JAR to the root classpath. |
| `add_nested_classpath(jar)` | Expand every nested `.jar` member onto the root classpath (library bundlers). |
| `publish_tree(owner, tree, destination, "replace-root")` | Atomically replace an exclusive physical source subtree. |

For example:

```rhai
let jar = tasks.project_jar("vendor/example-sources.jar");
let sources = tasks.extract_java(jar, "net/example");
tasks.publish_tree(
    "example-sources",
    sources,
    "src/main/java/net/example",
    "replace-root"
);
```

`replace-root` is deliberately explicit and destructive: after every non-empty task result succeeds,
the complete destination is replaced, including files manually added or edited below it. The
destination must be a strict descendant of a configured source root and may not overlap another
owner or managed build inputs. Failures leave the previous tree untouched. Ownership is recorded at
`target/jals/build/tasks/ownership-v1.json`; dropping an owner or running `jals clean` removes its
root before build state. Outside declared roots, files are never changed.

`jals build --offline` and `jals run --offline` permit verified cache hits but no task fetch. The
native LSP executes the same task plan, always offline: opening a folder in an editor runs whatever
`build.rhai` it contains, and nobody reviews a repository before opening it, so the server consumes
only what a real `jals build` already fetched and verified into the cache. It also defers
publication while an open document is below the destination. The browser playground rejects physical publication before any fetch. Immutable
dependency projects reject task terminals in this release. Tasks expose no shell/process API.

A concise `build.rhai` that generates and registers a Java source is:

```rhai
let source = output.write_text(
    "generated/BuildInfo.java",
    "public final class BuildInfo { public static final String VALUE = \"rhai\"; }\n"
);
build.add_source(source);
build.add_javac_arg("-Xlint:all");
build.add_jvm_arg("-Djals.build.script=rhai");
build.set_run_env("JALS_BUILD_SCRIPT", "rhai");
build.rerun_if_changed("src/main/java/Main.java");
build.rerun_if_env_changed("CI");
build.warning("generated BuildInfo.java");
build.metadata("generator", "rhai");
```

For CLI builds, `build.env` sees only `JALS_`-prefixed host variables plus `OUT_DIR` (always
`target/jals/build/rhai/out`), `JALS_MANIFEST_DIR` (`.`), and the optional
`JALS_PACKAGE_NAME`/`JALS_PACKAGE_VERSION`. The fixed values replace same-named host entries. The LSP
and playground deliberately supply only those fixed project values, not their host/browser
environment.

The rest of the host environment is withheld on purpose. A script can forward anything `build.env`
returns into a task fetch URL, so inheriting wholesale would expose every credential on the machine
(`GITHUB_TOKEN`, `AWS_SECRET_ACCESS_KEY`, registry tokens, …) to an unreviewed `build.rhai` —
including a **dependency's**, which the user never looked at. Pass host state deliberately by naming
it with the `JALS_` prefix (`JALS_MC_SIDE=client jals build`). For the root script, `set_compile_env`/`set_run_env` contribute entries to the eventual
CLI subprocesses; dependency process directives remain node-local. The LSP/playground do not apply
process-only flag/environment directives because they spawn no JDK tools.

Root-script `javac` arguments follow manifest `javac-flags` and remain before source paths. Root JVM
arguments precede `-cp`. Manifest classpath entries come first; root-script and resolved dependency
entries are stably deduplicated while preserving their first-occurrence order. Added sources compile
with ordinary project and source-dependency sources. Warnings are surfaced by each host.
`build.error`, a Rhai compile/evaluation error, a bad path, or a limit violation publishes no partial
generated output. Metadata is available in `BuildScriptOutput` for host integrations and is not
otherwise interpreted by `javac` or `java`.

Manifest-backed source dependencies use the lower-level immutable preparation API instead. The
project graph invokes every unique binary, legacy-source, and JALS-source node unconditionally in
dependency-first order (the first two are no-ops). A dependency script exports only paths explicitly
registered with `build.add_source` and `build.add_classpath`. Its `javac`/JVM arguments, compile/run
environment, and metadata remain node-local and never propagate to a parent or root invocation.
Generated bytes are read from the immutable preparation result and published as node-scoped,
digest-verified artifacts; the dependency source snapshot and its host tree are never mutated.

#### Fingerprints, cache, and clean

Successful state and generated bytes are published write-once to the storage artifact cache and
read back through digest-verified lookups. The native adapter persists them under
`target/jals/cache`; memory hosts retain them in their aggregate. A fingerprint covers the API/state
versions, script path and bytes, `jals.toml`, limits, tracked project bytes, and declared environment
values. A matching cache hit restores outputs and all directives without evaluating Rhai.

The root uses the distinguished `BuildScriptCacheScope::ROOT`; each dependency uses a scope derived
from its stable graph node identity. Identical `build.rhai` and output paths in two dependencies
therefore cannot collide in cache state or generated artifacts.

If the script calls no `rerun_if_changed`, the conservative default fingerprints every project file
except `target/jals/build/**`. Calling it at least once narrows project-file tracking to the declared
set; the script and manifest remain tracked independently. Only names passed to
`rerun_if_env_changed` contribute environment values to the fingerprint. Managed build-output paths
cannot be registered as rerun inputs, preventing generated files from invalidating or certifying
their own build.

Stale generated files are removed only when their current bytes still match output bytes verified
from this session or the persistent cache. Unknown or externally modified stale files are never
deleted. Missing cached output, a digest mismatch, changed inputs, or invalid cached state causes
normal script evaluation. Failure to persist cache state becomes a warning after generated output
has committed, not a failed build.

`jals clean` first removes exclusive task-owned source roots, then both `classes-dir` and
`target/jals/build`, including stale
`target/jals/build/rhai/out` files after a script is removed. It intentionally leaves the shared
verified cache at `target/jals/cache`; the next build can safely restore matching output from it.

#### Sandbox and WebAssembly

The synchronous script has no API for host filesystem access, spawning processes, clock/time, or
randomness. It can declare bounded, digest-verified task fetches, but cannot inspect fetched bytes or
perform network I/O during evaluation. Project reads and output writes go only through validated storage keys; output paths
cannot escape the dedicated root. Module loading, Rhai time support, custom syntax, `print`, and
`debug` do not provide host capabilities (`print`/`debug` output is discarded).

For the root project, compiler/JVM arguments, classpath entries, and compile/run environment
directives are inert during the Rhai phase but intentionally affect the later JDK subprocess started
by an explicit CLI `build`/`run`. They can enable compiler plugins, annotation processors, agents,
or other JDK features, so root build scripts remain trusted project code rather than a security
boundary for the subsequent compiler process. Dependency process directives do not propagate. The
LSP and playground never spawn that process.

Default limits include 1 MiB script source, 1,000,000 operations, 1,024 variables, 256 functions,
32 nested calls, expression depths of 64/32, 1 MiB strings, 65,536-item arrays, 4,096-entry maps,
4 KiB/128-segment paths, 1 MiB aggregate directives, 256 output files, 4 MiB per output, 16 MiB
total output, 4 MiB cached state, 4,096 task nodes, 16,384 task edges, 1 MiB task literals, 256 task
terminals, and 32 publication roots. Hosts may supply stricter non-zero `BuildScriptLimits`. The
same bounded engine builds for `wasm32-unknown-unknown` with:

```sh
cargo check -p jals-build --no-default-features --features rhai --target wasm32-unknown-unknown
```

See [`examples/rhai_build_script`](../examples/rhai_build_script) for a runnable project.
[`examples/task_source_archive`](../examples/task_source_archive) demonstrates exclusive source-JAR
publication. [`examples/minecraft-1.21.1-mojang-remap`](../examples/minecraft-1.21.1-mojang-remap)
fetches, remaps, and decompiles Minecraft 1.21.1 through the task DAG.

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
one **primary form** — `jar` (compiled classes), `git` (a checked-out project repository), or `path`
(a local project root) — plus form-specific options:

| Key | Type | Form | Maps to |
| --- | --- | --- | --- |
| `jar` | string | jar | a `.jar` location: an `https://`/`http://` URL (downloaded and cached), a `file://` URL, or a bare path (relative to the manifest dir) |
| `sources` | string | jar | *optional* companion **sources** `.jar` (the library's `.java`), located like `jar`. Editor go-to-definition only — never a compile or analysis input |
| `recursive` | bool | jar | *optional* (default `false`) — recursively unpack the jar's **bundled jars** (`*.jar` members nested inside it, as in a fat jar's `BOOT-INF/lib/*.jar`) onto the classpath, at any depth |
| `git` | string | git | a project repository URL to clone |
| `branch` / `tag` / `rev` | string | git | *optional*, **at most one** — which commit to check out (default: the repo's default branch) |
| `path` | string | path | a local project root (relative to the manifest dir) |
| `dir` | string | git, path | *optional* selected **project root** within the repository/path (e.g. `core`). Manifest probing and all child-relative paths start there. |

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
# A monorepo dependency selects the project root whose exact jals.toml should be probed:
core  = { git = "https://github.com/owner/mono", rev = "abc123", dir = "core" }
```

A **`jar`** dependency is resolved to a local `.jar` by the **host** (`jals-cli`/`jals-lsp` via
`jals-classpath`) and folded into the classpath for both **analysis** (`jals lint`, the LSP) and
**compilation** (`jals build`/`run` add it to `javac`/`java`'s `-classpath`). Remote jars are
downloaded into the SHA-256 verified artifact cache. With **`recursive = true`** the host also
unpacks the jar's **bundled jars** — the `*.jar` members a fat jar nests inside itself (e.g. a
Spring-Boot layout's `BOOT-INF/lib/*.jar`) — recursively into verified artifacts and adds them to the
same classpath. Its optional `sources` jar is purely an **editor** aid: `jals-lsp` stages its `.java`
from the cache and points go-to-definition at the real declaration; it is never a compile or
analysis classpath input. When a jar ships **no** `sources` jar, `jals-lsp` still makes
go-to-definition work by staging a decompiled `.java` skeleton from each classpath `.class`. The
decompiled output is editor-only, never a compile or `lint` input, and a real `sources` jar takes
precedence.

A **`git`** / **`path`** dependency selects a source-project root: the declared path or Git checkout,
followed by `dir` when present. `jals-project` probes only `<selected-root>/jals.toml`; it never
searches upward. An exact manifest makes the node a JALS project, so its own dependencies are
discovered transitively and its `[build] source-dirs`, `[build] classpath`, JAR locators, and child
path/Git locators resolve relative to that selected root. A missing manifest keeps the legacy source
convention (`src/main/java`, then `src`, then the selected root). A present but malformed manifest and
a dependency cycle are structural errors; `jals build`/`run` fail before `javac` rather than silently
falling back to legacy discovery.

Stable node identities deduplicate diamonds independently of dependency aliases. Native path nodes
use their stable selected location; Git nodes use repository identity, checked-out commit, and
selected directory, not a temporary checkout path. Discovery snapshots each source node, runs every
unique node's preprocessing transition exactly once in dependency-first order, and then projects its
authored sources, registered generated sources, declared classpath, and JAR dependencies into
node-scoped verified artifacts. Temporary Git checkouts are discarded after capture; no dependency
source tree is used as an output directory or otherwise mutated.

- **Compilation** (`jals build`/`run`) materializes the verified artifacts, passes all transitive
  source files to `javac`, and places transitive JAR/declared classpath entries on both compile and run
  classpaths.
- **Lint** resolves the transitive binary and declared-classpath graph for root-file analysis, but
  continues to lint only the source files requested by the command.
- **LSP** indexes transitive source artifacts for inference, hover, completion, navigation, and
  references. A hard graph error is diagnosed on the root `jals.toml`, then analysis falls back to
  the root project without dependencies; local path roots are watched for reassembly.
- **Playground** uses the portable `MemoryProjectGraph` over one captured in-memory `CodeTree`, so
  path projects inside that tree and their scripts work without host paths. Browser hosts cannot
  clone Git dependencies: each such entry produces a warning and is omitted. Remote JAR fetching is
  separate and remains available through the browser fetch adapter.

A malformed entry is rejected when the manifest loads, in two stages. `Dependency` is a
`#[serde(untagged)]` enum whose `Jar`/`Git`/`Path` variants each `deny_unknown_fields`, so the
**structural** errors — more than one primary form (`{ jar, git }`), no form at all (`{}`), or a field
misplaced for its form (`branch` without `git`, `sources` with `git`) — match no variant and fail at
**parse** time (a TOML error). The remaining **value-level** errors — an empty value, an unknown URL
scheme, conflicting git refs (`branch` + `tag`) — are caught by `Manifest::validate`. A runtime
acquisition failure (a download/clone error, a local jar/dir that does not exist) is reported as a
warning where recovery is possible and skips that input. `jals-build` itself only *classifies* each spec
(`Manifest::dependency_sources` → `DependencySource`, `dependency_source_jars` → the `sources` jar,
`dependency_source_dirs` → `SourceDependency::{Git, Path}`), staying pure — it performs no I/O.
This implemented transitive JALS source-project graph is distinct from Maven-coordinate resolution:
POM traversal, coordinate version selection, transitive Maven downloads, and `jals.lock` are still
future work (see roadmap §3).

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
jals clean                  # remove target/classes and target/jals/build
```

## Library API

Planning entry points are associated functions on the type they produce or act on:

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
    pub fn keys(manifest: &Manifest) -> Result<Vec<DirKey>, PathError>;
}

impl InitOptions {
    pub fn scaffold(&self) -> Vec<ScaffoldFile>;
}
```

`Invocation { program, args }` is a resolved command line as pure data; `display_command()`
renders it for `--dry-run`/`-v`. A `CompileRequest` carries an `extra_sources` list — the project
graph's verified transitive source artifacts, appended after `sources` — and both requests carry
already-resolved classpath entries in `extra_classpath`,
appended after the `[build] classpath` entries on `javac`/`java`'s `-classpath`; the classpath
separator is supplied by the backend planning the command (the requests stay tool-agnostic).
`RunTarget::resolve`
picks the `main-class` `jals run` should execute from `[[bin]]`/`default-run`/`[run] main-class`.
`InitOptions { name }.scaffold()` (for `jals init`) and `CleanTargets::keys` (for `jals clean`)
round out the pure planning surface.

With the `rhai` feature, `prepare_build_script(view, cache, cache_scope, manifest, environment,
limits)` is the immutable execution/cache seam. It returns `Ok(None)` when no script is configured,
or a `PreparedBuildScript` whose `output(revision)` exposes registrations/directives,
`file_bytes(view, key)` resolves generated or existing registered files, and `persist(cache)`
publishes generated bytes and state write-once. Preparation itself only reads the immutable view and
verified cache: it never reconciles files, mutates a source backend, or publishes artifacts. Callers
must use `BuildScriptCacheScope::ROOT` for the root and a stable, distinct scope for each dependency.

`execute_build_script(storage, manifest, environment, limits, session)` is the root-project adapter
over preparation. It atomically reconciles generated files into root storage, preserves all process
directives, updates `BuildScriptSession` ownership, and then persists the cache. Dependency graph
hosts instead consume only registered source/classpath bytes from `PreparedBuildScript` and publish
them under the node identity without reconciling the dependency snapshot.

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
`SourceDependency::from_dependency` classifiers back the three `ManifestExt` methods above). The
deeper recursive host is `jals-project`: `NativeProjectGraph` or `MemoryProjectGraph` discovers a
`ResolvedProjectGraph`, `preprocess` consumes it into the only state that permits `assemble`, and
assembly publishes artifact-only inputs for `jals_classpath::ProjectInputs`. The native adapter also
returns typed compile-classpath artifacts for CLI materialization and path roots for LSP watching.

## Development

```sh
cargo test -p jals-build --all-features
cargo clippy -p jals-build --all-targets --all-features -- -D warnings
cargo check -p jals-build --no-default-features
cargo check -p jals-build --no-default-features --features rhai --target wasm32-unknown-unknown
cargo check -p jals-project --no-default-features --target wasm32-unknown-unknown
cargo check -p jals-project --all-features
cargo build -p jals-playground --target wasm32-unknown-unknown
```

---

# Roadmap

`jals-build` today is a thin, faithful `javac`/`java` wrapper with a portable Rhai pre-build phase,
backed by the implemented transitive JALS source-project graph in `jals-project`.
The goal is to grow it into a **Cargo-for-Java** front end: dependency management, packaging,
testing, and richer build configuration. Each item below names its Cargo analogue (or marks a
Java-specific extension).

**The architectural rule for every item:** portable code gets no direct host filesystem, process,
or network capability, so it keeps building for `wasm32`. Storage-backed operations use typed
`ProjectStorage`; direct host effects (downloading a jar, running a test runner, writing a jar
archive) live in native adapters or `jals-cli`. A resolved dependency classpath, for instance, is
fed into `Invocation::build` exactly as the discovered source list is fed in today.

## 1. Commands to add

| Command | Cargo analogue | What it does | Needs |
| --- | --- | --- | --- |
| `jals new <path>` | `cargo new` | Scaffold into a **new** directory (vs. `init`, which is in-place). Mostly a thin alias over today's `InitOptions::scaffold`. | reuse `InitOptions::scaffold` |
| `jals check` | `cargo check` | Compile for diagnostics only, no runnable output (`javac -proc:only` / throwaway `-d`), or fold in `jals fmt --check` + `jals lint`. | a "check" invocation variant |
| `jals test [filter]` | `cargo test` | Compile test sources and run them via the JUnit Platform launcher; filter by class/method. | `[test]` section, `test-source-dirs`, a JUnit dep on the classpath, a runner invocation builder |
| `jals doc` | `cargo doc` | Run `javadoc` into `target/doc`; optionally open it. | a `javadoc` invocation builder, `[doc]` options |
| `jals jar` / `jals package` | `cargo package` | Produce a runnable jar (`Main-Class` in the manifest), optionally a fat/uber jar bundling classpath deps. | a `jar`/archive plan, `[package]` metadata |
| `jals add <coord>` / `jals remove <coord>` | `cargo add` / `cargo remove` | Edit `[dependencies]` in `jals.toml`. | manifest **writing** + Maven coordinate parsing |
| `jals tree` | `cargo tree` | Print the implemented source-project graph plus the future resolved Maven dependency tree. | CLI presentation + a Maven dependency resolver (§3) |
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
| `[dependencies]` | `[dependencies]` | **partly done**: explicit JARs are wired for analysis/compile (plus optional navigation sources), and `{ git = "url", branch/tag/rev, dir }` / `{ path = "...", dir }` form a transitive JALS source-project graph with exact manifest probing, dependency scripts, LSP navigation, and `build`/`run` compilation. Maven coordinates, POM/version resolution, transitive Maven download, and a lockfile remain §3. |
| `[dev-dependencies]` | `[dev-dependencies]` | test/bench-only deps (JUnit, etc.) |
| `[toolchain]` | `rust-toolchain.toml` | **partly done**: `compiler`/`runtime` select `javac`/`java` independently — `"system"`, `"builtin"`, an explicit `{ path = "…" }`, or a `{ distribution = { name, version } }` discovered among the installed JDKs (SDKMAN / `~/.jdks` / `~/.jdk` / `/usr/lib/jvm` / macOS). Still to come: **automatic download** of a missing JDK (rust-toolchain style, e.g. via the foojay disco API) into a per-user cache, and letting a `[package] features` release preset default `[build] release`. |
| `[repositories]` | (registries) | Maven repository URLs; default Maven Central |
| `[profile.dev]` / `[profile.release]` | `[profile.*]` | debug vs. optimized/stripped builds (`-g` vs. `-g:none`, lint levels) |
| `[workspace]` / `[[module]]` | `[workspace]` | multi-module builds with a shared lockfile |
| `[lints]` | `[lints]` | wire `jals-lint` / `-Xlint` configuration |

## 3. Maven dependency management (the largest remaining gap)

The transitive graph of explicit JALS path/Git source projects is implemented. The separate missing
piece is Maven's coordinate graph: a `[dependencies]` table of Maven coordinates would be resolved
through POMs and version selection into the `classpath` that `Invocation::build` already consumes.
The pure/`wasm32` split is preserved by keeping resolution's I/O in `jals-cli` (or a new host-only
crate, e.g. `jals-resolve`):

- **Resolver** — parse POMs, walk the transitive graph, pick versions on conflict (nearest-wins
  or a `[patch]`/override mechanism).
- **Local cache** — reuse `~/.m2/repository` or a dedicated `~/.jals` cache.
- **Lockfile** — a `jals.lock` pinning resolved versions + checksums for reproducible builds
  (drives `jals fetch` / `jals update` / `--locked` / `--frozen`).
- **Network fetch** — download missing jars from `[repositories]`; gated by `--offline`.

`jals-build` itself only needs the *result*: the resolved classpath, fed in like the current
`jals-project` source/artifact projection. No part of this changes the crate's purity.

**Already wired (analysis + compile side):** the *consumption* of a classpath is done, the
explicit-jar form resolves end-to-end, and JALS path/Git projects recurse through their own exact
manifests. `Manifest::classpath_entries`
resolves the `[build] classpath` to paths, and `Manifest::dependency_sources` classifies each
`[dependencies]` `{ jar = "..." }` into a URL or local path; the host-only `jals-classpath` crate
reads the `.class` bytes out of typed project/cache entries (and **downloads** remote jars into the
SHA-256 verified artifact cache via `ProjectInputs::assemble`) and parses them with `jals-classfile`; and
`jals-hir`'s `ProjectIndex::builder().with_classpath()` folds them in so external library types resolve in
`jals lint` and the language server, while `jals build`/`run` put the same jars on `javac`/`java`'s
classpath. What is still missing is the *resolver* above — turning **Maven coordinates** into those
classpath entries (POM walking + coordinate version conflict resolution + lockfile). Until
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

Direct host capabilities stay in `jals-cli`, a native adapter, or a future host-only helper crate:
process spawning, host filesystem walking, and network fetches. Portable `jals-build` code either
plans data or operates through `ProjectStorage`; Rhai scripts receive only the latter's typed,
revisioned project/cache contract. This keeps the portable feature set deterministic, unit-testable
without a JDK, and `wasm32`-buildable.

## Suggested priority

By Java-user impact:

1. **High-value `[build]` keys** — `resource-dirs`, `encoding`, `enable-preview`, `-Xlint`
   (cheap, immediately useful, no new infrastructure).
2. **`jals test`** — JUnit integration; the first thing most projects need after `build`/`run`.
3. **Maven dependency management (§3)** — coordinate/POM resolver + `jals.lock`. The highest
   impact and the largest remaining effort; unblocks `add`/`remove`/`fetch`/`update` and Maven
   entries in `tree`.
4. **Packaging (§4)** — `jals jar`, then fat jars.
5. **The rest** — `doc`, profiles, workspaces, `publish`, `bench`.
