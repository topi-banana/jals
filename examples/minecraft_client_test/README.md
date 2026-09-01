# A Minecraft client, as a test-only dependency

This project is one Java class — `com.example.mctest.GameClient` — that boots a real Minecraft
client inside the test JVM and hands a `#[test]` method a typed handle on it, plus the runtime
libraries that boot needs, for **every release the SDK carries: 1.14.4 through 26.2, 43 of them**.
It has no `main`, produces no jar anybody ships, and is never compiled into a consumer's output. It
exists to be named in one line of somebody else's manifest:

```toml
[dev-dependencies]
mc-client-test = { path = "../minecraft_client_test" }
```

`examples/minecraft_mod` is the consumer in this repository; a second mod is the same one line, plus
one `mc-client-test/<release>` route on each of its own release features — see below.

## Why `[dev-dependencies]` and not `[dependencies]`

A `path` dependency in jals is a **source** dependency: its `[build] source-dirs` tree is lowered
through its own frontend and handed to the consumer's `javac` alongside the consumer's own files.
That is what makes `GameClient` compile against the consumer's classpath rather than needing a
published jar — and it is also why declaring a test harness in `[dependencies]` would package it
into the mod jar.

`[dev-dependencies]` is resolved by `jals test` and by the analysis hosts (`jals lint`, the language
server) and by nothing that produces output. It is **not transitive**: a consumer of a consumer
never sees this project.

## What crosses the dependency edge, and what does not

|                                            | Crosses                            | Where it comes from                                                       |
| ------------------------------------------ | ---------------------------------- | ------------------------------------------------------------------------- |
| `GameClient.java`                          | yes, as a compile input            | `[build] source-dirs`, lowered under this project's own frontend           |
| the ~60 runtime jars                       | yes, onto the consumer's classpath | `tasks.add_classpath` in `build.rhai`                                      |
| the game jar and its navigation sources    | yes                                | the `minecraft` SDK, reached from both sides of a diamond                  |
| **`-Xmx2G`**                               | **no**                             | `build.add_jvm_arg` reaches a test JVM only from the *root* project's script |
| **`--release`**                            | **no**                             | `build.add_javac_arg` is the same rule, so `GameClient.java` is compiled at whatever `--release` the consumer set |

So a consumer writes the JVM argument itself; the compiler one it already has, because a mod
compiled against a release is capped at that release's class-file level either way:

```rhai
if build.feature("client-test") {
    build.add_jvm_arg("-Xmx2G");
}
```

Leaving it out does not fail cleanly — the boot dies inside the resource reload with an
`OutOfMemoryError`, which reads as a harness bug and is not one.

## 43 releases, and where the lines are

Two names select this harness, and they answer different questions:

- **`enabled`** — is the harness wanted at all? It routes `minecraft/client` and nothing else. A
  consumer routes it from its own `client-test`.
- **a release feature** (`1.20.1`) — which release? It routes `minecraft/1.20.1` and names one
  *threshold*.

Splitting them is what keeps a consumer honest. A release feature routed on its own costs nothing:
`build.rhai` registers no task, `GameClient.java` blanks to its `package` line, and the SDK is
reached with the selection the consumer had already made. So `jals test --features 1.20.1` resolves
exactly the classpath `jals build --features 1.20.1` resolves — the harness is additive or it is
absent, never a third thing in between.

### The threshold chain

`#[cfg]` in `GameClient.java` never names a release. It names a threshold; a release names exactly
one threshold and inherits the rest, so a 44th release is one row in `jals.toml` and no change to any
source file. A threshold exists only where the game's API actually moved, which is why there are
fourteen and not forty-three:

| threshold       | what moves at it                                                                                             |
| --------------- | ------------------------------------------------------------------------------------------------------------ |
| `since-1.14.4`  | the bottom of the chain: `build.rhai` errors when `enabled` is on and no release was named                     |
| `since-1.15`    | the game window is behind `getWindow()`; on 1.14.4 it is the public `window` field                             |
| `since-1.16`    | `getMessage()` returns a `Component`; the overworld is `overworld()` rather than `getLevel(DimensionType.OVERWORLD)`; `LevelSettings` takes a name and a difficulty; `createLevel` replaces `selectLevel` |
| `since-1.17`    | `--release 16`                                                                                                 |
| `since-1.18`    | `--release 17`                                                                                                 |
| `since-1.18.2`  | the builtin registries come from `builtinCopy()` as a `Writable`, not `builtin()` as a `RegistryHolder`         |
| `since-1.19`    | world creation moves onto `Minecraft.createWorldOpenFlows()`                                                   |
| `since-1.19.3`  | `createFreshLevel` takes a `WorldOptions` and a dimensions function                                            |
| `since-1.20.3`  | `createFreshLevel` gains its trailing `Screen`                                                                 |
| `since-1.20.5`  | `--release 21`                                                                                                 |
| `since-1.21.2`  | `WorldPresets.createFlatWorldDimensions` exists; `GameRules` takes the enabled feature flags                   |
| `since-1.21.11` | `GameRules` moves to `net.minecraft.world.level.gamerules`                                                     |
| `since-26.1`    | `LevelSettings` becomes a record carrying a `DifficultySettings` and no game rules                             |
| `since-26.2`    | the showing screen and the overlay move onto `Minecraft.gui`; the flat preset helper is renamed                |

The set is *this project's*. `examples/minecraft_mod` reads the same 43 releases through five
thresholds of its own, because it branches on different things — that two consumers of one catalog
disagree about where the interesting lines are is why the SDK publishes no chain for them to share.

### What that costs the source

One file. The whole of the drift is eight short private methods and 24 bodies between them:
`showing`, `show`, `overlay`, `label`, `windowWidth` and `overworld` have two each, `settings` has
four and `createWorld` has eight. Everything a consumer calls is one method on all 43, and
everything that could be avoided was — `runCommand` dispatches through the server's own Brigadier
dispatcher, because the three calls that takes are identical on every release, while the
client-side spelling moved four times.

Two of the fourteen boundaries are ones no mapping file could have shown, because a ProGuard mapping
carries names and descriptors and no access flags: 1.15's window accessor and 1.16–1.18.2's private
flat preset were both found by compiling.

One thing the harness does *not* hide, because hiding it would be a lie: `openWorld` opens a
**superflat** world on 1.14.4–1.15.2 and on 1.19 and later, and the **default** generator on
1.16–1.18.2. Those nine releases keep the flat preset in a private field of the client's own
`WorldPreset`, and the only public route to the same generator is to assemble it out of a
`FlatLevelGeneratorSettings`, a `FlatLevelSource` and `DimensionType.defaultDimensions` that
themselves differ across 1.16–1.17.1, 1.18–1.18.1 and 1.18.2. Nothing a test asserts depends on the
terrain, so what that costs is a few seconds of generation.

`GameClient.java` is also **Java 8 source** throughout: no `ProcessHandle`, no `Files.writeString`,
no `Stream.toList`, no pattern `instanceof`. It is loaded by the JVM the release runs on, and the
oldest releases run on Java 8.

### The library pins

The ~60 jars a client loads at boot are per release and per platform, so all 43 sets are pinned in
`build.rhai` — 2287 rows, 379 distinct jars, linux/x86_64 — as data the script loops over. A
generator writes them:

```sh
python3 examples/scripts/gen-client-runtime.py
```

It takes no arguments and rewrites every release, reading the release list and each release's
metadata digest out of `examples/minecraft/build.rhai`'s own `CATALOG` rather than restating either.
It refuses to write anything at all if it cannot read one library of one release: a table that is
partly regenerated is a boot that dies in `SharedLibraryLoader` with its cause two files away.

## Which JDK

Two JDKs, chosen independently — `$JAVAC` and `$JAVA` are how a caller says so. The compiler only has
to be able to *read* the game's class files; the runtime has to be one the release can boot on:

| releases         | compiles with | boots on |
| ---------------- | ------------- | -------- |
| 1.14.4 – 1.16.5  | 9+            | 8        |
| 1.17 – 1.17.1    | 16+           | 16       |
| 1.18 – 1.20.4    | 17+           | 17       |
| 1.20.5 – 1.21.11 | 21+           | 21       |
| 26.x             | 25+           | 25       |

One JDK 25 compiles all 43 — `javac` accepts any `--release` from 8 up and reads a newer classpath
class without complaint — which is why CI installs exactly that one, and why the harness's
`--release` cascade tops out at 21 rather than following 26.x to 25. Booting an older release wants
its own JVM:

```sh
JAVAC=$JDK25/bin/javac JAVA=$JDK8/bin/java jals test --features 1.14.4,client-test -j 1
```

## Why every declaration carries `#[cfg(feature = "enabled")]`

A dependency is a graph node whether or not the selection routes a feature to it, so this project's
build script runs and its sources are lowered under *every* selection a consumer makes — including
`jals lint --features 1.20.1`, where no client jar exists and `net.minecraft.client.*` names
nothing. Under those selections the `#[cfg]` blanks the file down to its `package` line: a
compilation unit that declares no type and emits no class. `build.rhai` registers no task for the
same reason, so those selections fetch nothing.

The selection that *does* name the release is the other half of that, and worth knowing:
`jals lint` is unconditionally offline, so under `--features <release>,client-test` it wants that
release's ~60 jars already in the consumer's verified cache. `jals build` no longer puts them there — that is the
point of the table — so a `jals test` has to have run. Without it the graph does not resolve and
lint degrades to a root-only analysis with a warning, the same way it does for any dependency whose
artifacts are not yet built.

That is why the project declares `[package] features = ["attributes"]`, and why `[features]
default` is empty — a default release would be forwarded into the SDK as a *second* version beside
the one the consumer chose, and the SDK rejects that before any download.

## Booting it

```java
try (GameClient game = GameClient.launch()) {
    assert game.screen() instanceof TitleScreen;
    game.openWorld("jals-test");
    game.runCommand("setblock 0 2 0 minecraft:gold_block");
    assert game.evalOnServer(server -> server.getPlayerList().getPlayerCount()) == 1;
}
```

The hinge is that `Minecraft implements java.util.concurrent.Executor`. `execute(Runnable)`
overrides a JDK interface method, so no mapping set may rename it, and a thread that is not the
render thread can put work on the render thread through it. `evalOnClient` is that call with a
result and an exception path attached; `evalOnServer` is the same against `MinecraftServer`, which
is an `Executor` for the same reason. A client rather than a dedicated server, because vanilla
publishes no static accessor for a running `MinecraftServer` — but
`Minecraft.getInstance().getSingleplayerServer()` is public, so one boot hands a test both.

Four constraints come from `jals test` rather than from Minecraft:

- One JVM is one client, so each `#[test]` boots its own. Run them with `-j 1`; two at once want
  two GL contexts and twice the memory.
- The game runs on a **daemon** thread this object never asks to stop. `jals test` reads a test as
  passed when the harness prints its sentinel line, which happens after the method returns, and a
  client that took the JVM down on its way out would take the sentinel with it.
- `close()` therefore abandons the game and arms a watchdog that halts the JVM once the sentinel has
  had its moment. The client leaves non-daemon workers behind, so without it the process never
  exits. It halts with status `0`, which is only safe while the sentinel is what is being read — so
  **do not run these tests with `--no-capture`**, where there is nothing captured to read and the
  runner falls back to the exit status this forces.
- A screen appearing is not readiness. The boot is settled when the overlay is gone *and* the title
  screen is up.

## What it does not supply

Two things a launcher provides and this does not, both measured rather than assumed:

- **No native library directory.** The `-natives-linux` classifier jars go on the classpath like
  every other one; LWJGL's `SharedLibraryLoader` extracts what it needs out of them, so there is no
  `-Djava.library.path` and no unpacked directory.
- **No asset store.** The harness writes an asset index with no objects in it. Almost every object a
  launcher downloads is a sound or a translation; the textures, models and shaders a boot needs are
  inside the client jar.

**Linux only.** GLFW wants the main thread on macOS (`-XstartOnFirstThread`), and the main thread
belongs to the test.

## A note on classpath order

A consumer with the default SDK features gets both the client jar *and* the server bundler's
flattened libraries, so 23 102 classes are declared twice — once by a bundler member and once by one
of the jars pinned here. Whichever group comes first wins, and the graph puts the SDK's first. That
is inert: comparing the bytes of every duplicated class across the two groups leaves exactly one
difference, a `META-INF/versions/11/module-info.class`, which a classpath never loads. Mojang
publishes one library set per release and the server bundler carries the same versions.

## Running the tests that use it

From the consumer, not from here:

```sh
cd ../minecraft_mod
jals test --features 1.21.11,client-test -j 1
```

Any of the 43 releases goes in place of `1.21.11`.

Here, `jals build --features <release>,enabled` is the whole of what this project does on its own: it
proves the harness compiles against the release it claims. CI runs that for all 43, which is also
what verifies all 2287 pinned digests — a build script's fetches execute.

## Legal note

Nothing Mojang publishes is redistributed. The build script fetches the client's runtime libraries
from `libraries.minecraft.net` by URL, each pinned to a SHA-1 and a byte cap, into the local
verified cache. The game itself comes from the `minecraft` SDK dependency on the same terms; see
`../minecraft/README.md`.
