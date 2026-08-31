# A Minecraft client, as a test-only dependency

This project is one Java class — `com.example.mctest.GameClient` — that boots a real Minecraft
client inside the test JVM and hands a `#[test]` method a typed handle on it, plus the ~60 runtime
libraries that boot needs. It has no `main`, produces no jar anybody ships, and is never compiled
into a consumer's output. It exists to be named in one line of somebody else's manifest:

```toml
[dev-dependencies]
mc-client-test = { path = "../minecraft_client_test" }
```

`examples/minecraft_mod` is the consumer in this repository; a second mod is the same one line.

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
| **`--release 21`**                         | **no**                             | `build.add_javac_arg` is the same rule, so `GameClient.java` is compiled at whatever `--release` the consumer set |

So a consumer writes the JVM argument itself; the compiler one it already has, because a mod
compiled against a release is capped at that release's class-file level either way:

```rhai
if build.feature("client-test") {
    build.add_jvm_arg("-Xmx2G");
}
```

Leaving it out does not fail cleanly — the boot dies inside the resource reload with an
`OutOfMemoryError`, which reads as a harness bug and is not one.

## One release, and where the pin lives

The canonical pin is the `[features]` key in `jals.toml`: **`1.21.11`**. Six other places name it
and none of them is the source of truth — `build.rhai`'s guard, the `#[cfg]` on every declaration in
`GameClient.java`, the consumer's `client-test` entry, the consumer's own `build.rhai` guard, this
repository's two CI cells (`minecraft_client_test` and `minecraft_mod (client)`), and the default in
`examples/scripts/gen-client-runtime.py`.

One release rather than the SDK's 43, because the harness is written against a client API that
moves: `openFlatWorld` builds a `LevelSettings` and a `WorldOptions` whose shapes are release
specific, and the pinned library set is per release and per platform anyway. Supporting a second
release means a second `[features]` key, a second generated block, and a `#[cfg]` branch at each
call site that moved.

Regenerating the library set for another release:

```sh
python3 examples/scripts/gen-client-runtime.py 1.21.11
```

## Why every declaration carries `#[cfg(feature = "1.21.11")]`

A dependency is a graph node whether or not the selection routes a feature to it, so this project's
build script runs and its sources are lowered under *every* selection a consumer makes — including
`jals lint --features 1.20.1`, where no client jar exists and `net.minecraft.client.*` names
nothing. Under those selections the `#[cfg]` blanks the file down to its `package` line: a
compilation unit that declares no type and emits no class. `build.rhai` registers no task for the
same reason, so those selections fetch nothing.

The selection that *does* name the release is the other half of that, and worth knowing:
`jals lint` is unconditionally offline, so under `--features 1.21.11,client-test` it wants the ~60
jars already in the consumer's verified cache. `jals build` no longer puts them there — that is the
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
    game.openFlatWorld("jals-test");
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
  exits.
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

Here, `jals build --features 1.21.11` is the whole of what this project does on its own: it proves
the harness compiles against the release it claims.

## Legal note

Nothing Mojang publishes is redistributed. The build script fetches the client's runtime libraries
from `libraries.minecraft.net` by URL, each pinned to a SHA-1 and a byte cap, into the local
verified cache. The game itself comes from the `minecraft` SDK dependency on the same terms; see
`../minecraft/README.md`.
