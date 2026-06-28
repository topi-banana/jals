# jals-build

Cargo-style build orchestration for Java projects — the engine behind `jals build` / `jals run`
/ `jals clean` / `jals init`.

A [`jals.toml`](#the-manifest-jalstoml) manifest is the Java analogue of `Cargo.toml`: it says
where the sources live, where compiled classes go, which Java release to target, and what is on
the classpath. This crate parses that manifest and turns it (plus already-resolved inputs) into
a `javac`/`java` command line, the set of paths a clean removes, or the files a fresh project
needs — all as **pure data**, never spawning a process or touching the filesystem:

```
jals.toml ───────▶ Manifest ──┐
                              ├──▶ build_invocation ─▶ Invocation ──▶ ┐
discovered .java files ───────┘    run_invocation   ─▶ Invocation ──▶ ┤  jals-cli
                                                                      ├▶ spawns javac/java,
InitOptions ──────────────────────▶ scaffold        ─▶ [ScaffoldFile] ┤  writes files,
                                     clean_paths     ─▶ [PathBuf] ─────┘  removes dirs
```

`jals-cli` owns every side effect: it discovers the manifest, walks the source tree, spawns the
JDK tools, writes the scaffold, and deletes the clean paths. Keeping `jals-build` pure makes it
deterministic, unit-testable with no JDK installed, and **`wasm32`-compatible** (it has no
`jals-syntax` dependency and uses only `serde`/`toml`).

## What it does today

Four subcommands are wired through `jals-cli`, each backed by one pure entry point here:

| Command | Backed by | What it does | Flags |
| --- | --- | --- | --- |
| `jals build` | `build_invocation` | Discover the manifest and `.java` sources, build the `javac` command, and run it. | `--manifest-path <PATH>`, `--dry-run`, `-v`/`--verbose`, `--out-dir <DIR>`, `--bin <NAME>` |
| `jals run` | `resolve_run_target` + `build_invocation` + `run_invocation` | Compile, then run the resolved entry point with `java`. Compilation must succeed first. | `--manifest-path <PATH>`, `--dry-run`, `-v`/`--verbose`, `--main-class <FQCN>`, `--bin <NAME>`, `-- <args>` |
| `jals clean` | `clean_paths` | Remove the build output (the `classes-dir`). A never-built project succeeds quietly. | `--manifest-path <PATH>`, `--dry-run` |
| `jals init [PATH]` | `scaffold` | Scaffold a new project: `jals.toml`, a starter `Main.java`, and a `.gitignore`. Refuses to overwrite an existing `jals.toml`. | `--name <NAME>` |

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
- **JDK tool resolution** — `javac`/`java` are located by honoring `$JAVAC`/`$JAVA` first, then
  `$JAVA_HOME/bin/<tool>`, and finally the bare name on `PATH`. The platform classpath separator
  (`:` on Unix, `;` on Windows) is injected, so the invocation builder stays pure.
- **Exit codes** — the JDK tool's exit code propagates; a signal-terminated tool fails with `1`.

## The manifest (`jals.toml`)

Every key is optional and falls back to its default; keys are kebab-case and grouped into
`[package]`, `[build]`, `[run]`, and the repeatable `[[bin]]`. The defaults encode the
Maven-style `src/main/java` → `target/classes` layout, so an empty (or absent) section just
uses them.

```toml
[package]
name = "hello"
version = "0.1.0"
# default-run = "server"           # which [[bin]] `jals run` runs when several exist

[build]
source-dirs = ["src/main/java"]   # -sourcepath roots, also scanned for .java files
classes-dir = "target/classes"    # javac -d
release = 21                       # javac --release N
# source = 17                      # javac --source N  (only when release is unset)
# target = 17                      # javac --target N  (only when release is unset)
classpath = ["libs/guava.jar"]    # -classpath entries (jars or dirs)
javac-flags = ["-Xlint:all"]      # appended verbatim, before the source files

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
```

### `[package]`

| Key | Type | Default | Status |
| --- | --- | --- | --- |
| `name` | string | — | ℹ️ informational (reserved for future jar packaging) |
| `version` | string | — | ℹ️ informational |
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

The run target for `jals run` is resolved in this order (`resolve_run_target`):

1. `--main-class <FQCN>` — runs that class directly, bypassing the manifest.
2. `--bin <name>` — the `[[bin]]` with that name (error if none matches).
3. `[package] default-run` — when several `[[bin]]` exist.
4. the single `[[bin]]`, when exactly one is declared.
5. `[run] main-class` — only when **no** `[[bin]]` is declared (full backward compatibility).

Once any `[[bin]]` exists, `[run] main-class` is ignored for selection. Duplicate bin names and a
`default-run` that names no bin are rejected at manifest load (`Manifest::validate`).

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

```rust
pub fn build_invocation(manifest: &Manifest, project_root: &Path,
                        sources: &[PathBuf], path_sep: char) -> Invocation;
pub fn run_invocation(manifest: &Manifest, project_root: &Path, main_class: &str,
                      program_args: &[String], path_sep: char) -> Invocation;
pub fn resolve_run_target<'m>(manifest: &'m Manifest, bin: Option<&str>)
                          -> Result<&'m str, ResolveTargetError>;
pub fn clean_paths(manifest: &Manifest, project_root: &Path) -> Vec<PathBuf>;
pub fn scaffold(options: &InitOptions) -> Vec<ScaffoldFile>;
```

`Invocation { program, args }` is a resolved command line as pure data; `display_command()`
renders it for `--dry-run`/`-v`. `resolve_run_target` picks the `main-class` `jals run` should
execute from `[[bin]]`/`default-run`/`[run] main-class`. `Manifest::from_file` loads, parses, and
validates (`Manifest::validate`) `jals.toml`; `Manifest::discover_path` locates it.
`Manifest::source_roots` and `Manifest::classpath_entries` resolve the `[build] source-dirs` and
`[build] classpath` entries against the manifest directory, as absolute paths, for the host to read:
sources to compile, and — new — the classpath jars/dirs the host (`jals-classpath`) reads `.class`
files from to feed `jals-hir`'s analysis (so `jals lint` / the LSP see external library types).

## Development

```sh
cargo test  -p jals-build                                          # manifest + invocation + clean + scaffold tests
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
resolved dependency classpath, for instance, is fed into `build_invocation` exactly as the
discovered source list is fed in today.

## 1. Commands to add

| Command | Cargo analogue | What it does | Needs |
| --- | --- | --- | --- |
| `jals new <path>` | `cargo new` | Scaffold into a **new** directory (vs. `init`, which is in-place). Mostly a thin alias over today's `scaffold`. | reuse `scaffold` |
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

`description`, `authors`, `license`, `repository`, `homepage`, `keywords`, and
`edition` / `java-version` (a default `release`). These become a jar's `MANIFEST.MF` / POM
metadata on packaging. (`default-run` is already implemented — see [`[[bin]]`](#bin).)

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
| `[dependencies]` | `[dependencies]` | Maven coordinates (`group:artifact:version`); resolved into the classpath (§3) |
| `[dev-dependencies]` | `[dev-dependencies]` | test/bench-only deps (JUnit, etc.) |
| `[repositories]` | (registries) | Maven repository URLs; default Maven Central |
| `[profile.dev]` / `[profile.release]` | `[profile.*]` | debug vs. optimized/stripped builds (`-g` vs. `-g:none`, lint levels) |
| `[workspace]` / `[[module]]` | `[workspace]` | multi-module builds with a shared lockfile |
| `[lints]` | `[lints]` | wire `jals-lint` / `-Xlint` configuration |

## 3. Dependency management (the largest gap)

Java's defining build feature, and the biggest piece missing. A `[dependencies]` table of Maven
coordinates would be **resolved** (transitively) into the `classpath` that `build_invocation`
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

**Already wired (analysis side):** the *consumption* of a classpath for semantic analysis is done.
`Manifest::classpath_entries` resolves the `[build] classpath` to paths; the host-only
`jals-classpath` crate reads the `.class` files out of those jars/dirs and parses them with
`jals-classfile`; and `jals-hir`'s `ProjectIndex::build_with_classpath` folds them in so external
library types resolve in `jals lint` and the language server. What is still missing is the
*resolver* above — turning Maven coordinates into those classpath entries (download + cache +
lockfile). Until then, a project lists already-present jars/dirs under `[build] classpath` by hand.
JDK standard-library classes are not loaded this way either; the embedded `java.lang`/`java.util`
stubs stand in for them (reading the JDK's `jimage`/`modules` is a separate, still-unwired step).

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
