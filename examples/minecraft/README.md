# Minecraft Mojang-mappings remapped sources

This example uses the build-task DAG to:

1. Fetch the selected release's Minecraft version metadata (pinned SHA-1).
2. Resolve the selected jar — and, for an obfuscated release, the official Mojang mappings —
   through JSON projections.
3. For a server bundler jar (1.18+), extract `META-INF/versions/<version>/server-<version>.jar` and
   flatten nested library jars onto the classpath with `add_nested_classpath`.
4. **Remap** the jar with those mappings (member paths renamed to official names) — skipped from
   26.1, which ships deobfuscated and declares no mappings.
5. **Decompile** every class under `net/minecraft` into compile-oriented Java skeletons.
6. Publish the tree at `src/main/java/net/minecraft` (`replace-root`).
7. With the default `mixin` feature, fetch **both** SpongePowered Mixin 0.8.7 jars: the classes onto
   the classpath, and the real `org/spongepowered` sources published at
   `src/main/java/org/spongepowered` under a second owner (`extract_java`, not `decompile_java` —
   Mixin ships actual `.java`).
8. With the default `mixinextras` feature, do the same once more for MixinExtras 0.5.4, published at
   `src/main/java/com/llamalad7` under a third owner.

The release, the distribution, and the two libraries all come from the `[features]` declared in
`jals.toml`. Every axis contributes the same pair — bytecode on the classpath, source to read — which
is what lets another project depend on this one and _compile_ against all three.

## Version selection

Every release from **1.14.4** to **26.2** is a feature — 43 of them, named exactly like the
release:

```
26.2     26.1.2   26.1.1  26.1
1.21.11  1.21.10  1.21.9  1.21.8  1.21.7  1.21.6  1.21.5  1.21.4  1.21.3  1.21.2  1.21.1  1.21
1.20.6   1.20.5   1.20.4  1.20.3  1.20.2  1.20.1  1.20
1.19.4   1.19.3   1.19.2  1.19.1  1.19
1.18.2   1.18.1   1.18
1.17.1   1.17
1.16.5   1.16.4   1.16.3  1.16.2  1.16.1  1.16
1.15.2   1.15.1   1.15
1.14.4
```

They are mutually **exclusive**: at most one may be active. `jals.toml` therefore keeps
`default = ["server", "mixin", "mixinextras"]` — the default list carries a side and the two
libraries, but deliberately no version — and `build.rhai` falls back to `DEFAULT_VERSION` (26.2, the
newest release) when no version feature is selected. Selecting a version needs nothing else:

```sh
jals build                       # 26.2 (the fallback) + server + mixin + mixinextras
jals build --features 1.20.1     # 1.20.1 + server + mixin + mixinextras
jals build --features 1.16.5,client   # 1.16.5, client overlaid on server, + both libraries
```

Two or more version features fail before any download, in `build.rhai` rather than in the manifest,
because `[features]` resolution is additive and cannot express exclusivity:

```
$ jals build --features 1.20.1,1.19.4
error: build script reported: error: select at most one Minecraft version feature, got `1.20.1` and `1.19.4`
```

`--all-features` therefore always fails here — it selects all 43 releases at once.

Three boundaries are baked into the catalog at the top of `build.rhai`, carried by the two flags on
each entry (`bundler?` and `obfuscated?`), which are independent of each other:

- **1.14.4 is the floor.** Mojang published no official mappings before it, so earlier releases
  cannot be remapped and are not declared at all — `--features 1.14` is rejected by the CLI as an
  undeclared feature.
- **26.1 drops the mappings.** From 26.1 Minecraft ships with its real names already in the jar, and
  the version metadata declares no `client_mappings`/`server_mappings` download at all — projecting
  one would not resolve. Those entries are marked `obfuscated? = false`, and the script skips both
  the mappings `fetch_text` and `remap_jar`; everything else (bundler extraction, decompile,
  publication) is unchanged. This is orthogonal to the layout boundary below: 26.x is still a
  bundler.
- **1.18 changes the server layout.** From 1.18 the server download is a _bundler_: the game jar
  sits at `META-INF/versions/<version>/server-<version>.jar` with its libraries under
  `META-INF/libraries/`, so the script pulls the game out with `nested_jar` and flattens the
  libraries with `add_nested_classpath`. 1.14.4–1.17.1 ship one flat jar with the libraries
  (netty, guava, gson, log4j, …) alongside the game classes; there is nothing to unwrap, and
  `add_classpath` on the remapped jar already puts those libraries on the compile classpath.

The resolved feature set is always part of the build-script fingerprint, so switching versions
re-runs the script and `replace-root` swaps the whole published tree for the new release.
`--offline` succeeds only for a version already in the verified cache.

## Side selection

Selection is **additive**, exactly like Cargo: a feature never subtracts, so `--features client`
keeps the default `server` and therefore builds the _merged_ jar. Drop `server` with
`--no-default-features`.

| selection                                 | resolved features                          | behaviour                                                   |
| ----------------------------------------- | ------------------------------------------ | ----------------------------------------------------------- |
| (none)                                    | `server`, `mixin`, `mixinextras`           | server jar only (26.2 — no mappings, no remap)              |
| `--features client`                       | `server`, `client`, `mixin`, `mixinextras` | remap both if obfuscated, then `merge_jars(server, client)` |
| `--features server,client`                | `server`, `client`, `mixin`, `mixinextras` | same as above                                               |
| `--no-default-features --features client` | `client`                                   | client jar only — and neither library at all                |
| `--features 1.16.5`                       | `server`, `mixin`, `mixinextras`, `1.16.5` | 1.16.5 server jar + server mappings                         |
| `--no-default-features --features 1.16.5` | `1.16.5`                                   | same — no side selected falls back to `server`              |

The `behaviour` column describes the game side only; `mixin` and `mixinextras` are orthogonal to it
and are covered below.

`merge_jars` overlays the client onto the server, so the client wins path conflicts. A client-only
build never enters the server branch, so on 1.18+ `add_nested_classpath` is skipped and the bundled
libraries (brigadier, guava, netty, …) are absent from its compile classpath. Client-_specific_
libraries (lwjgl, icu4j, jorbis, …) are never fetched at all — the launcher resolves them from the
metadata's `libraries` list, which this example does not walk — so `net/minecraft/client` classes
referencing them stay unresolved in a merged build too.

```sh
# First run downloads ~52 MiB (the game plus the two library jars) and then remaps +
# decompiles (slow).
cargo run -p jals-cli -- build

# Another release, in place of the default.
cargo run -p jals-cli -- build --features 1.20.1

# Merged: the client overlaid on the server.
cargo run -p jals-cli -- build --features client

# Client only — and, because `--no-default-features` also drops `mixin` and `mixinextras`, neither
# library on the classpath nor either published root.
cargo run -p jals-cli -- build --no-default-features --features client

# Subsequent runs reuse the verified SHA-256 project cache.
cargo run -p jals-cli -- build --offline

cargo run -p jals-cli -- clean   # removes every owned publication root too
```

## Mixin

The default `mixin` feature is a **third axis**, independent of both the side and the release: it
fetches no Minecraft artifact at all, and composes freely with any combination of the two. When it
is on, `build.rhai` fetches [SpongePowered Mixin](https://github.com/SpongePowered/Mixin) 0.8.7 from
the SpongePowered Maven repository — **both jars**, each pinned by SHA-1 exactly like every other
fetch here:

| item        | value                                                                           |
| ----------- | ------------------------------------------------------------------------------- |
| coordinate  | `org.spongepowered:mixin:0.8.7`                                                 |
| classes     | `8ab114ac385e6dbdad5efafe28aba4df8120915f` (1.1 MiB, capped at 2 MiB) — classpath |
| sources     | `b5fd91c657404b1712a612ece6c8ddf66069be0f` (989 KiB, capped at 2 MiB) — published |
| owner       | `mixin-0.8.7`                                                                   |
| destination | `src/main/java/org/spongepowered` (`replace-root`)                              |
| contents    | 443 real `.java` — the `asm/**` runtime and the `tools/**` annotation processor |

The two halves do different jobs, and a consumer needs both. The **classes** jar goes on the compile
classpath with `add_classpath`, which is the only reason `org.spongepowered.asm.*` resolves anywhere
— a dependency's published trees are navigation sources for a reader and never compile inputs, so a
publication on its own would leave a consumer's `javac` with nothing. The **sources** jar is what
that reader opens. Because Mixin ships actual sources, this half of the script uses **`extract_java`**
rather than `decompile_java`: the same fetch → publish contract as the game, minus the skeleton
rendering, so the published tree is the library's own code rather than a reconstruction of it.

The published tree still does not compile, and that is a property of the tree rather than of the
classpath entry. Mixin 0.8.7 is unshaded, so its sources need asm, guava, gson, commons-io, log4j2
and modlauncher, and all of `org/spongepowered/tools` is an annotation processor; none of that is
fetched here. A consumer compiling against `mixin-0.8.7.jar` never sees any of it. Nor is anything
defined twice: javac resolves a type it is handed both as a source file and as a classpath class to
the source, and `duplicate class` is what two *sources* declaring one name produce.

Turning the feature off removes both halves: with `mixin` unselected the script fetches neither jar,
so nothing reaches the classpath and the `mixin-0.8.7` owner is never registered — and dropping an
owner removes its root. So
`--no-default-features --features 1.20.1` deletes `src/main/java/org/spongepowered` on its next
successful build — the same exclusive-ownership rule that swaps the game tree on a version switch.

## MixinExtras

The default `mixinextras` feature is the **fourth axis**, and the second one that fetches no
Minecraft artifact: [MixinExtras](https://github.com/LlamaLad7/MixinExtras), the companion library
whose `@WrapOperation` / `@ModifyExpressionValue` / `@Local` injectors and sugar every recent mod
applies on top of Mixin. It goes through exactly the same two-jar shape — classes on the classpath,
sources through `extract_java` → `publish_tree`:

| item        | value                                                                             |
| ----------- | --------------------------------------------------------------------------------- |
| coordinate  | `io.github.llamalad7:mixinextras-common:0.5.4`                                    |
| classes     | `0626e00b72e3879a07e6653d8015cd3466ff5b75` (709 KiB, capped at 1 MiB) — classpath |
| sources     | `fd5d27cff1c8118f5a4a037e7f549b606d117caf` (215 KiB, capped at 512 KiB) — published |
| owner       | `mixinextras-0.5.4`                                                               |
| destination | `src/main/java/com/llamalad7` (`replace-root`)                                    |
| contents    | 216 real `.java` — the injectors, the sugar, and the `ap/**` annotation processor |

`mixinextras-common` is the platform-neutral core; the `-fabric` / `-forge` / `-neoforge` artifacts
are only thin per-loader bootstraps around it, so the `common` sources are the whole library worth
browsing — including the expression engine under `expression/**` that backs `@Expression`. It comes
from Maven Central, where a released version is immutable, so the one SHA-1 pins both the URL and
the fetch like every other fetch here. Apart from its manifest the jar holds nothing outside
`com/llamalad7/mixinextras`, so the single `com/llamalad7` prefix takes all of it.

`mixinextras` is independent of `mixin`, not layered on it: neither feature enables the other, they
own disjoint destinations, and each root appears, is replaced, or is removed on its own. So
`--no-default-features --features mixinextras` publishes this root and the game tree (no side
selected still falls back to `server`), but not Mixin's. The default selection simply lists both
libraries, which is the combination worth browsing: MixinExtras' sources refer to
`org.spongepowered.asm.*` throughout, so having Mixin's own tree open next to them is what makes
them readable.

Like Mixin's, the published tree does not compile — it is written against Mixin, asm and the loader
APIs this example never fetches, and `mixinextras/ap` is an annotation processor — and, like Mixin's,
that says nothing about the classes jar a consumer actually compiles against. The two are verified
together: `@Mixin`, `@Shadow`, `@Inject`, `@At`, `CallbackInfo`, `CallbackInfoReturnable`,
`@ModifyExpressionValue`, `@WrapOperation` and `Operation` all resolve with
`mixin-0.8.7.jar` and `mixinextras-common-0.5.4.jar` alone — no asm, guava or log4j2 needed.

## What it demonstrates

- `tasks.fetch_json` / `fetch_jar` / `fetch_text` with mandatory HTTPS + digest + byte cap.
- `tasks.json_url` / `json_sha1` / `json_u64` projections over Mojang version metadata.
- `tasks.nested_jar(jar, member)` — pull the game jar out of a 1.18+ server bundler.
- `tasks.add_nested_classpath(jar)` — flatten every nested library jar onto the compile classpath.
- `tasks.remap_jar(jar, mappings)` — hierarchy-aware Mojang mojmap deobfuscation. The default 26.2
  build does not reach it; `--features 1.21.11` (or any release up to it) does.
- `tasks.merge_jars(base, overlay)` — deterministic union, overlay wins on conflict.
- `tasks.decompile_java(jar, prefix)` — compile-oriented skeleton source tree.
- `tasks.extract_java(jar, prefix)` — its counterpart for a library that ships real `.java`: the
  Mixin and MixinExtras sources jars go through the same fetch → publish contract with no
  reconstruction step.
- `tasks.publish_tree(..., "replace-root", "navigation")` + `tasks.add_classpath` — **paired three
  times**, once per root: the resolved game jar behind the decompiled tree, `mixin-0.8.7.jar` behind
  `org/spongepowered`, and `mixinextras-common-0.5.4.jar` behind `com/llamalad7`. Pairing them is
  what makes this project usable as a dependency at all, and `"navigation"` is the half of the pair
  the script states: every tree here is a *view* of types the classpath already carries, so a
  consumer reads them and compiles against the jars. (The other intent, `"compile"`, is for a tree
  that is the only carrier of its package — which none of these is, and none of which would compile
  anyway.)
- **Three independent publication roots from one script**: `src/main/java/net/minecraft` (owner
  `minecraft-<version>`), `src/main/java/org/spongepowered` (owner `mixin-0.8.7`), and
  `src/main/java/com/llamalad7` (owner `mixinextras-0.5.4`). They are produced by disjoint task
  subgraphs, own disjoint destinations, and are enabled by unrelated features — so each appears, is
  replaced, or is removed on its own.
- `build.feature("server")` / `build.feature("client")` / `build.feature("mixin")` /
  `build.feature("mixinextras")` for `[features]` switching — the resolved feature set is always
  part of the build-script fingerprint, so no `rerun_if_env_changed` is needed for it.
- **Mutually exclusive features on top of an additive `[features]` model**: the script scans its
  catalog with `build.feature`, rejects a second match with `build.error` (which publishes nothing
  and runs no task), and falls back to `DEFAULT_VERSION` when none matched.
- **One version-shaped pipeline**: the same task graph serves 43 releases, with the catalog's
  `bundler?` and `obfuscated?` flags as its only two structural branches — independent of each
  other, so 26.x takes the bundler path without the remap one — and the version threaded through
  the metadata URL, the nested member path, and the `publish_tree` owner (`minecraft-<version>`).
  Switching versions swaps the owner of one destination and replaces the published tree wholesale.

## Compile-safety

Compile-oriented rendering applies several defenses so skeletons stay closer to valid Java:

- field `final` is dropped (avoids blank-final errors under empty constructors),
- bridge/synthetic methods are omitted (they reference anonymous types the tree does not render),
- enum `values()` / `valueOf()` are omitted (javac synthesizes them),
- interface methods with bodies are marked `default`,
- nested classes keep outer capture (not forced `static`) so enclosing type parameters still bind,
- `extends` is omitted so empty constructors never need an unavailable `super(...)`,
- method bodies are safe placeholders (`{}` / `throw new RuntimeException()`).

Even so, a vanilla release still leaves residual `javac` errors (generic type bounds that depended
on dropped supers, and other structural edge cases). Treat the published tree as **reference
sources + remapped bytecode classpath** first: browse it in the LSP, and expect `jals build` of the
full tree to report remaining errors. Full semantic recompilation of vanilla is not guaranteed.

The demonstrated piece is the pipeline itself (fetch → nested extract → remap → decompile →
exclusive publish); a cleanly compiling tree is best-effort.

None of this applies to the `mixin` and `mixinextras` trees, which are the libraries' own source
rather than skeletons — but they do not compile either, for the unrelated reason given above:
Mixin's dependencies (asm, guava, gson, commons-io, log4j2, modlauncher) are never fetched, and
MixinExtras needs those plus the parts of Mixin its own `ap/**` uses. `jals build` reports
unresolved references in both trees. Compiling *against* the two libraries is a different question
and works: that goes through their classes jars, which are on the classpath.

The target release also sets the JDK the optional `javac` step needs: 26.x declares
`javaVersion.majorVersion` 25 (1.21.11 declares 21), so its class files are major version 69.
Decompilation and publication do not care — `jals-classfile` reads the version without gating on it
— but compiling the published tree wants a matching toolchain.

## Ownership and clean

`replace-root` exclusively owns `src/main/java/net/minecraft`, and — with the `mixin` and
`mixinextras` features — `src/main/java/org/spongepowered` and `src/main/java/com/llamalad7`. A
successful changed result removes every existing descendant before rewriting the tree — including
after a version switch, which retires the previous `minecraft-<version>` owner of that same
destination, and after dropping `mixin` or `mixinextras`, which retires the `mixin-0.8.7` or
`mixinextras-0.5.4` owner and with it that whole root. `jals clean` drops all three directories
along with `target/classes` and `target/jals/build`. The shared verified cache under
`target/jals/cache` is kept so `--offline` rebuilds stay fast.

## Using it as a dependency

Another project can depend on this one and get the game without running any of it itself:

```toml
[dependencies]
minecraft = { path = "../jals/examples/minecraft", features = ["client", "26.2"] }
```

The consumer's `jals build` runs this build script under its _own_ feature selection and receives
**three classpath jars and three navigation trees**, one pair per feature axis:

| feature       | on the compile classpath                                       | as navigation sources     |
| ------------- | -------------------------------------------------------------- | ------------------------- |
| side/release  | the resolved game jar (+ a 1.18+ bundler's flattened libraries) | `net/minecraft/**`        |
| `mixin`       | `mixin-0.8.7.jar`                                               | `org/spongepowered/**`    |
| `mixinextras` | `mixinextras-common-0.5.4.jar`                                  | `com/llamalad7/**`        |

The classpath column is where a consumer's types actually come from: a dependency's published trees
reach an editor and stop there — they are never compile inputs, because handing `javac` a decompiled
skeleton next to the jar it was decompiled from is a duplicate, not an improvement. So a Minecraft
mod depending on this example compiles `net.minecraft.*`, `org.spongepowered.asm.*` and
`com.llamalad7.mixinextras.*` out of the box, and can open the real source behind any of them.

That pairing is the point, and jals says so when it is missing: a dependency that publishes a root no
classpath entry backs is reported against the publication, not left to fail as
`package … does not exist` in the consumer several layers away.

Everything lands in the _consumer's_ `target/jals/cache`. This directory is not written to:
`src/main/java/net/minecraft` is only ever physically published when this project is built as the
root, and whether you have ever done that makes no difference to a consumer. A `replace-root`
destination belongs to its publication, so the 6000-odd files a root build leaves there are read as
what they are — output of the same plan, which the consumer already has as navigation sources — and
never compiled a second time as if somebody had written them.

There is deliberately no `Main.java` and no `[run]` section here, for the same reason: a source
dependency's authored files are compiled into whoever consumes it, so a type in the default package
would collide with the consumer's own. Only `src/main/java/README.md` is tracked, so the declared
source root exists before the first build has published anything into it — which also means
`jals build --dry-run` on a fresh clone reports `no .java files found`, because `--dry-run` skips the
publication that would otherwise fill the root. A real `jals build` publishes first and is fine.

In an editor, the extra classpath jars mean skeletons are now synthesized for Mixin's and
MixinExtras' classes as well. They do not displace the published sources: a type is addressed by one
package-relative path whichever producer offers it, and the assembled navigation set keeps the first
producer to claim that path — a library's own `.java`, then a published tree, then a synthesized
skeleton. So `org.spongepowered.asm.mixin.Mixin` still resolves to the real source, and the skeleton
for it is the fallback that never gets used.

The first consumer build pays the same download-and-decompile cost documented above, plus ~1.8 MiB
for the two library jars; after that the whole task execution is memoized under the resolved feature
set, so rebuilds and editor reloads reuse it.

## Writing a mod against this

[`examples/minecraft_mod`](../minecraft_mod) is that consumer: a Mixin mod built from this project's
three classpath jars, reobfuscated by `[build] remap` into the names vanilla actually loads, for all
43 releases. Read it for the whole shape; what belongs *here* is the one fact about the game that
decides how such a mod is written.

**Not every class is obfuscated.** Mojang keeps a few names, and the entry points are exactly where
they do it — which is the opposite of where a "hello world at startup" mod would first reach:

| type                                            | 1.14.4 – 1.15.2 | 1.16 – 1.21.11               | 26.x |
| ----------------------------------------------- | --------------- | ---------------------------- | ---- |
| `net.minecraft.server.MinecraftServer`          | itself          | itself                       | —    |
| `net.minecraft.server.Main`                     | **absent**      | itself                       | —    |
| `net.minecraft.server.dedicated.DedicatedServer`| `uk` / `wd`     | `zg` (1.16.5) … `ary` (1.21.11) | —    |

(Verified against all 39 published `server_mappings`. The 26.x column is empty because those
releases ship deobfuscated and declare no mappings at all — see the catalog's `obfuscated?` flag.)

So `MinecraftServer` and `Main` map to themselves: a mixin aimed at either round-trips through
`remap_jar` unchanged, which is fine for the mod and proves nothing about the remap. `Main` is also
not in the mappings before 1.16, so it is not even a target that spans the range. `DedicatedServer`
is obfuscated in every mapped release, which makes it the one entry point where a mod compiled
against these sources genuinely needs reobfuscating before vanilla will load it.

The same table is why the mod example touches no *member*: jals rewrites annotation `Class` values
and never annotation strings, and generates no refmap, so `@Mixin(DedicatedServer.class)` is
rewritten while `@Shadow` or `@At(target = "…")` would silently bind to the wrong name.

## Legal note

Generated Minecraft sources and the original jars/mappings are Mojang's copyrighted material.
This example only records the download URL and digests; artifacts stay local to your machine
and must not be redistributed.

See [`jals-build/README.md`](../../jals-build/README.md#rhai-build-scripts) for the complete
task API.
