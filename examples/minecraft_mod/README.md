# A Minecraft mixin mod, built for every release from 1.14.4 to 26.2

One mixin, one resource, one jar — and **one source tree** for all 43 releases
[`examples/minecraft`](../minecraft) knows. It prints a line naming the running game as the
dedicated server object finishes construction:

```sh
cargo run -p jals-cli -- build --manifest-path examples/minecraft_mod/jals.toml --features 1.20.1
# → examples/minecraft_mod/target/jals/remap/hellomod-0.1.0.jar
```

Six declarations in `jals.toml` carry the whole thing:

- **`[dependencies] minecraft`** — a `path` dependency on the SDK example. Its build script fetches
  the release, remaps it with the official mappings, and puts the game jar, `mixin-0.8.7.jar` and
  `mixinextras-common-0.5.4.jar` on *this* project's compile classpath, with the matching sources
  published as navigation trees an editor can open.
- **`[[mappings.mojmap]]`** — 39 feature-gated alternatives of one name, one per release that ships
  obfuscated.
- **`[build] remap`** — reobfuscate the compiled classes with whichever alternative the selection
  activates, and package them, resources included, into a jar.
- **`[package] features = ["attributes"]`** — the jals dialect's `#[cfg(...)]`. Mojang renamed one
  API this mixin uses, so the source carries both spellings as live branches instead of the project
  carrying two source trees.
- **`[build] script`** — a Rhai script deriving the class-file level `javac` compiles at, which is
  one of the two things that vary with the release and are not names.
- **`[build.resources] template`** — the other one: the `compatibilityLevel` the mixin configuration
  declares, rendered into the resource on its way into the jar.

**Building the jar is the deliverable.** Loading it needs a Mixin-capable launcher, which jals is
not and this example does not ship — see [Running it](#running-it).

## One source tree, 43 releases

The mixin asks the game what version it is. That call has three shapes across the range, and only
one of the two differences between them is one the source has to spell out:

```java
@Inject(method = "<init>", at = @At("RETURN"), remap = false)
private void hellomod$helloWorld(CallbackInfo callback) {
    #[cfg(feature = "since-1.21.6")] String version =
        SharedConstants.getCurrentVersion().name();
    #[cfg(not(feature = "since-1.21.6"))] String version =
        SharedConstants.getCurrentVersion().getName();
    System.out.println("Hello, world from Minecraft " + version);
}
```

1.21.6 turned `WorldVersion`'s getters into record-style accessors, so `getName()` became `name()`.
That is a rename in the game's **source**, not in its namespace: no mapping set relates the two, and
a mod that spelled one of them would simply not compile against the other half of the range. The
other difference is invisible here — 1.18 replaced the interface that call returns, and the source
gets away with it by never naming the type. See [What one selection produces](#what-one-selection-produces).

Both branches are live source. Whichever release is selected, the other is still parsed, formatted
and navigable in an editor; the compile frontend strips the attributes before `javac` sees the file
and blanks the disabled branch in place, length-preservingly, so a line number in a stack trace
still names the line that was written. `jals lint` says which branch that was, as advice rather
than as a finding, so it reports it and still exits 0:

```
[cfg] Advice: disabled by `cfg` under the current feature selection
    ╭─[ src/main/java/com/example/hellomod/mixin/HelloMixin.java:56:9 ]
```

The predicate is a **threshold feature**, not a version. `jals.toml` declares two kinds of name:

- a **version feature** (`1.20.1`) — one release. It routes its own name into the SDK, gates one
  `[[mappings.mojmap]]` alternative, and enables the highest threshold that release satisfies.
- a **threshold feature** (`since-1.18`) — what everything else reads. Each enables its
  predecessor, so naming the highest turns on the whole chain and `feature = "since-1.18"` is true
  for every release from 1.18 up without any of the thirty-odd releases above it listing more than
  one name.

There are five thresholds rather than forty-three, because a threshold exists only where something
branches on it:

| threshold      | what branches on it                                          |
| -------------- | ------------------------------------------------------------ |
| `since-1.14.4` | `build.rhai`: was a release named at all?                     |
| `since-1.17`   | `--release 16`, `compatibilityLevel: JAVA_16`                 |
| `since-1.18`   | `--release 17`, `compatibilityLevel: JAVA_17`                 |
| `since-1.20.5` | `--release 21`, `compatibilityLevel: JAVA_21`                 |
| `since-1.21.6` | `WorldVersion.getName()` → `name()`                           |

Three of them are the steps the game's own JVM took, which is the era table
[below](#jdk-requirement); one is the API rename above; and the fifth is the bottom of the chain,
which exists so that "no release named" stays distinguishable from "the oldest release named".

**A release has to be named.** `jals build` with no `--features` used to build against 26.2, because
`[dependencies] minecraft` leaves `default-features` at its `true` and the SDK falls back to its own
newest release. That is no longer a build this project can do: the SDK would fall back while every
threshold here stayed off, and the source would take its oldest branch against the newest game. So
`build.rhai` says so instead of letting `javac` say it:

```
$ cargo run -p jals-cli -- build --manifest-path examples/minecraft_mod/jals.toml
error[build-script]: select a Minecraft version feature, e.g. `--features 26.2`. There is
deliberately no default: a release chooses the game jar, the mapping set and every `#[cfg]` branch
at once, and this project cannot see which release the SDK fell back to.
error: the build script failed
```

There is no `default` naming a version either, and there cannot be: `[features]` resolution is
additive, so a default version would survive `--features 1.20.1` and be a *second* release — which
is exactly what the SDK rejects.

Only the *build* needs the selection. `jals fmt` never had a use for one, and `jals lint` without one
still analyses the file and exits 0: the script's error is a build-time diagnostic, so an editor
opened on this project with nothing selected reports it and goes on indexing.

## What one selection produces

Five selections, one source tree, and every column below read back off the packaged jar with
`javap -v`:

| selection  | `@Mixin(value = …)`      | version call              | accessor                    | class-file major | `compatibilityLevel` |
| ---------- | ------------------------ | ------------------------- | --------------------------- | ---------------- | -------------------- |
| `1.14.4`   | `Luk;`                   | `o.a()`                   | `GameVersion.getName()`     | 52         | `JAVA_8`             |
| `1.17.1`   | `Laas;`                  | `ab.b()`                  | `GameVersion.getName()`     | 60         | `JAVA_16`            |
| `1.20.1`   | `Lahe;`                  | `aa.b()`                  | `ad.c()`                    | 61         | `JAVA_17`            |
| `1.21.11`  | `Lary;`                  | `w.b()`                   | `aa.c()`                    | 65         | `JAVA_21`            |
| `26.2`     | `…/DedicatedServer;`     | `SharedConstants.…()`     | `WorldVersion.name()`       | 65         | `JAVA_21`            |

Three separate mechanisms are visible in that table, and it is worth naming which is which.

The first three columns of the 26.2 row are the point of the mappings design rather than a hole in
it: those four releases ship deobfuscated and declare no mappings download, so no alternative names
them and `[build] remap` packages without rewriting — which is exactly the right jar, because the
names already in it are the ones that runtime loads. "This selection ships no mappings" says *do not
rewrite the names*, not *produce nothing*.

`GameVersion` staying `com.mojang.bridge.game.GameVersion` on the two oldest rows is the same
property from the other side: it is a Mojang *library* type, not a game class, so it appears in no
mapping set and the pass leaves it alone. Up to 1.17.1 `SharedConstants.getCurrentVersion()` returned
it; 1.18 replaced it with `net.minecraft.WorldVersion`, which is obfuscated like everything else and
comes out as `Lad;` on 1.20.1. Neither change is anything the source has to know about — only the
1.21.6 rename is.

And the last two columns are the build script and the resource template, reading the same threshold
chain in their own vocabularies.

The jar's member list is identical in every row:

```sh
$ unzip -l target/jals/remap/hellomod-0.1.0.jar
  META-INF/MANIFEST.MF
  com/example/hellomod/mixin/HelloMixin.class
  mixins.hellomod.json
```

CI builds **all 43**, one cell per release, and merges the jars into a single `hellomod-jars`
artifact attached to the run — so the five rows above are a sample of what a run produces rather
than a claim about the releases someone remembered to check. Two of them, `1.20.1` and `1.21.11`,
one on each side of the `since-1.21.6` branch, are additionally run through `jals fmt --check` and
`jals lint`, which is the half a jar does not prove: the linter evaluates the same `cfg` the
frontend does, so each side needs its own selection to be looked at.

## Why 39 mappings under one name

`[build] remap`'s `with` names a single `[mappings]` key, and a mod that targets 39 obfuscated
releases needs 39 different mapping texts. Writing 39 keys would not help: `with` still takes one.

So the key holds *alternatives*:

```toml
[build]
remap = { with = "mojmap" }

[[mappings.mojmap]]
required-features = ["1.20.1"]
url = "https://piston-data.mojang.com/v1/objects/0b4dba049482496c507b2387a73a913230ebbd76/server.txt"
sha1 = "0b4dba049482496c507b2387a73a913230ebbd76"
max-bytes = 6291456

[[mappings.mojmap]]
required-features = ["1.19.4"]
…
```

They are gated, not merged: **at most one may be active**, so the name still denotes exactly one
mapping set once the features are known. jals enforces that statically rather than at build time —
any two alternatives whose `required-features` are comparable by inclusion are rejected, because
some selection would activate both. Here each names exactly one version feature, so the 39 sets are
pairwise incomparable and the table is accepted.

They gate on the **version** features and never on the thresholds, and the difference is not
cosmetic. That static check compares the lists as they are written, not as the feature graph closes
them, so a pair gated on `since-1.18` and `since-1.20.5` would pass it — and then fail at build time
on every release from 1.20.5 up, where a selection satisfies both at once and `jals` refuses to
guess which mapping set rewrote the jar. One version feature per alternative is what makes the 39
sets incomparable under the chain as well as on the page.

Every URL is content-addressed by the same SHA-1 it is checked against, so one digest pins both the
address and the bytes. `max-bytes` is the published size rounded up to the next MiB; the digest is
what guarantees the content, so pinning an exact byte count would only ever break on a re-serve of
identical text.

To refresh the table: for each catalog entry in [`../minecraft/build.rhai`](../minecraft/build.rhai)
whose `obfuscated?` flag is true, fetch
`https://piston-meta.mojang.com/v1/packages/<metadata sha1>/<version>.json` and project
`downloads.server_mappings.{url, sha1, size}`.

## Why `remap = false`, and where the line is

```java
@Mixin(value = DedicatedServer.class, remap = false)
public class HelloMixin {
    @Inject(method = "<init>", at = @At("RETURN"), remap = false)
    private void hellomod$helloWorld(CallbackInfo callback) { … }
}
```

jals rewrites two kinds of reference and not a third, and it generates no refmap. Everything about
this mixin follows from that:

- An **annotation `Class` value** is rewritten. `@Mixin`'s `value` is one, which is why `javap -v` on
  the packaged class reads `org.spongepowered.asm.mixin.Mixin(value=[class Lahe;], remap=false)` on
  1.20.1. The rewrite covers `RuntimeInvisibleAnnotations` as well as the visible ones, which is
  load-bearing here: `@Mixin` is `CLASS`-retained and `@Inject` is `RUNTIME`-retained, so this one
  mixin uses both attributes.
- An **ordinary reference in code** is rewritten too — it is a constant-pool entry like any other.
  `SharedConstants.getCurrentVersion().getName()` comes out of the pass as `aa.b()` and `ad.c()` on
  1.20.1, which is why the mixin may call the game at all.
- An **annotation string** is not. `method = "<init>"` names a constructor, which is `<init>` in
  every namespace and appears in no mapping; `@At("RETURN")` names an injection point, not a member.
  Neither needs a refmap, and neither would get one.
- `remap = false` is correct in **both** branches: on an obfuscated release the class literal already
  carries the obfuscated name, and on 26.x nothing was rewritten because the game ships
  deobfuscated. Either way the reference is already right and Mixin must take it verbatim.
  `remap = true` would send it looking for a refmap that exists in neither case. It is written on
  both annotations rather than relying on `@Inject` inheriting it, which removes one
  version-of-Mixin question from the example's correctness.

**The stated limit.** A `@Shadow`, `@Accessor`, `@Redirect`, `@ModifyArg`, `@At(target = "…")`, or a
`method` naming an obfuscated method all address their target through an annotation *string*. jals
rewrites none of them, so such a reference would bind against the wrong name at load time rather
than fail to compile — a silent wrong answer. Writing the obfuscated name by hand works and pins the
mod to one release. That is the boundary of what this example claims, not a to-do.

`DedicatedServer` is the target for the same reason: it is the one entry-point class obfuscated in
all 39 mapped releases. `MinecraftServer` and `net.minecraft.server.Main` map to themselves, so a
mixin aimed at either would round-trip unchanged and demonstrate nothing — and `Main` does not
appear in the mappings at all before 1.16. See the table in
[the SDK's README](../minecraft/README.md#writing-a-mod-against-this).

## The build script

`build.rhai` derives what `javac` needs that varies with the release, and it holds **no table of
releases**. `jals.toml` already routes 43 version features into the SDK, and the SDK's own build
script is what rejects a second one; a catalog here would be a second copy of that rule and the
first of the two to drift. So the script reads the threshold chain instead — which is also why a
release added to `jals.toml` needs nothing here at all:

```rhai
let release = 8;
if build.feature("since-1.20.5") {
    release = 21;
} else if build.feature("since-1.18") {
    release = 17;
} else if build.feature("since-1.17") {
    release = 16;
}
build.add_javac_arg("--release");
build.add_javac_arg("" + release);
```

`--release` rather than `source`/`target`, because only `--release` also pins the platform API the
mod may reach for. The level follows the game's own JVM, and it **stops at 21** rather than
continuing to 25 for the 26.x releases: the framework this mod compiles against is SpongePowered
Mixin 0.8.7, whose `compatibilityLevel` vocabulary ends at `JAVA_21`, and a mixin class Mixin cannot
name a level for is one it refuses to apply. Capping costs the mod nothing — a Java 21 class file
loads on the Java 25 JVM 26.x runs on, and this mixin needs no language feature above Java 8 anyway.

Two more arguments, for reasons that are not about this mod:

- `-Xlint:-options` when the level is 8, because a current JDK compiles at 8 but says twice on every
  build that it is obsolete. The warning is about the option, not about this project.
- `-proc:none`, always. `mixin-0.8.7.jar` registers two annotation processors in `META-INF/services`
  and the SDK puts that jar on the compile classpath. Nothing here wants them — their job is the
  refmap this example deliberately ships none of — and whether a `javac` starts a processor it
  merely *found* on the class path is a property of the JDK: a JDK 25 does not, an older one may.
  Saying so here is what keeps the build from depending on which JDK a reader happens to have
  installed.

## Resources

`[build] resource-dirs` (defaulting to `["src/main/resources"]`) names the directories packaged into
the jar alongside the classes. Mixin has to read `mixins.hellomod.json` at load time, so without it
the jar is a jar of class files nothing will ever apply.

One field of that file is per-release, so it is **rendered rather than copied**:

```json
{
  "required": true,
  "minVersion": "0.8",
  "package": "com.example.{{ package.name }}.mixin",
{% if features["since-1.20.5"] %}
  "compatibilityLevel": "JAVA_21",
{% elif features["since-1.18"] %}
  "compatibilityLevel": "JAVA_17",
{% elif features["since-1.17"] %}
  "compatibilityLevel": "JAVA_16",
{% else %}
  "compatibilityLevel": "JAVA_8",
{% endif %}
  "mixins": ["HelloMixin"],
  "injectors": { "defaultRequire": 1 }
}
```

`compatibilityLevel` must be at least the class-file version `javac` produced, and `build.rhai`
produces one of four. The two derivations read the same threshold chain in their own vocabularies —
`--release 17` there, `JAVA_17` here — which is one table rather than two: the chain is in
`jals.toml`, and neither site has a list of releases in it.

`[build.resources] template` is what turns rendering on, and it names files rather than switching a
mode:

```toml
[build.resources]
template = ["mixins.hellomod.json"]
```

Everything else under a resource directory is still packaged byte for byte, which is the point — a
resource is whatever the author put there, and a mod icon through a template engine is a corrupt
PNG. A block tag alone on its line takes the line with it, so the rendered file is valid JSON with
no blank lines left behind; see [the build crate's
README](../../jals-build/README.md#resource-templates) for the two readable namespaces and the three
deliberate divergences from Jinja.

There is still no `"refmap"` key: declaring one that is absent logs a warning on every load, and
there is none to declare.

Resources are authored project files, so they are read from the project snapshot rather than walked
off disk, and they reach the **jar only** — `jals run` executes `classes-dir`, which is compiler
output and never receives them.

## Features

The 43 version features, the five thresholds and the four SDK axes are all declared here; the two
kinds and what reads them are [above](#one-source-tree-43-releases). Two things about the table are
worth adding on their own.

```toml
[features]
server = ["minecraft/server"]
"1.20.1" = ["since-1.18", "minecraft/1.20.1"]
```

Declaring the version names locally is what lets `[[mappings.mojmap]]` gate on them; the
`minecraft/…` entry is pure routing and is never queryable in this project.

**Version exclusivity is not restated here.** The forwarded selection reaches the SDK's own build
script, whose `build.error` rejects a second version before any download:

```
$ cargo run -p jals-cli -- build --manifest-path examples/minecraft_mod/jals.toml --features 1.20.1,1.19.4
error: resolving the project dependency graph: dependency build script `…/examples/minecraft`
failed: build script reported: error: select at most one Minecraft version feature, got `1.20.1`
and `1.19.4`
```

`--all-features` fails identically, for the same reason, and always will. A second copy of that
43-row rule in this manifest would be a second thing to keep in sync.

The sentence is the SDK's own, reported where it was hit: the attribution names which dependency
failed and the body is that dependency's diagnostic, so there is nothing to go and reproduce
elsewhere. This is the one diagnostic you could not read for yourself — the script ran against a
snapshot this project does not own.

## JDK requirement

`build.rhai` decides what the mod is compiled *to*; your JDK still decides what can be compiled *at
all*, and the two are separate constraints. `--release 17` cannot relax the binding one: to compile
against the SDK's classpath `javac` must be able to **read** the game's class files, and a JDK 21
`javac` rejects a Java 25 class outright. So your JDK must be at least:

| releases         | JDK | `--release` | boots on |
| ---------------- | --- | ----------- | -------- |
| 1.14.4 – 1.16.5  | 9   | 8           | 8        |
| 1.17 – 1.17.1    | 16  | 16          | 16       |
| 1.18 – 1.20.4    | 17  | 17          | 17       |
| 1.20.5 – 1.21.11 | 21  | 21          | 21       |
| 26.x             | 25  | 21          | 25       |

The fourth column is `jals test`'s, not `jals build`'s, and it is a different constraint: compiling
only needs a `javac` that can read the game's class files, but *booting* the client needs the JVM
that release actually runs on. `$JAVAC` and `$JAVA` are resolved independently, so one command says
both — `JAVAC=$JDK25/bin/javac JAVA=$JDK8/bin/java jals test --features 1.14.4,client-test -j 1`.

The oldest row is the one place the two columns disagree about which number is binding: those game
jars are Java 8 class files, but `--release` did not exist before JDK 9, so 9 is the floor for
building them here.

A newer JDK is always fine — `javac` accepts any `--release` between the oldest it still supports (8
today) and its own version, and reads a *classpath* class newer than that release without complaint
— so one JDK 25 builds all 43. CI installs exactly that one.

## Running it

Out of scope, and stated rather than implied. The jar carries the names vanilla loads, but something
has to install Mixin's transformer into the JVM before the game classes are loaded — a mod loader,
or a launcher built around `MixinBootstrap`. jals is a build tool: it neither generates nor
validates loader metadata, and this example ships none (no `fabric.mod.json`, no
`META-INF/mods.toml`).

If you adapt this for a specific loader, note that the reobfuscation target here is **vanilla's own
obfuscated namespace**. A loader that deobfuscates the game to some other namespace at runtime
(Fabric's intermediary, older Forge's SRG) wants a different mapping set on the `[build] remap`
line, not a different mixin. Such a loader also ships its own Mixin, usually newer than the 0.8.7 the
SDK puts on the classpath — and 0.8.7's `CompatibilityLevel` ending at `JAVA_21` is the whole of what
the `--release` cap above answers to, so a build against a Mixin that names a higher level would
raise it.

## Booting the game from a test

`src/test/java` holds three `#[test]` methods that start a **real Minecraft client in the test JVM**
and assert against it — no Mixin, no java agent, no launcher, and on any of the 43 releases. What
starts it is not here: the harness is a project of its own, `../minecraft_client_test`, and this
project reaches it in one line.

```toml
[dev-dependencies]
mc-client-test = { path = "../minecraft_client_test" }
```

That is the whole of the arrangement, and it is the point of the split. `[dev-dependencies]` is
resolved by `jals test` and by the analysis hosts and by nothing that produces output, so the jar
`jals build` writes holds the mixin and nothing else — which a `[dependencies]` entry could not
promise, because a `path` dependency's `.java` is compiled into whoever consumes it. Nothing in
`GameClient` names a type in `com.example.hellomod`, so a second mod adds the same one line and gets
the same harness.

```sh
cd examples/minecraft_mod
cargo run -p jals-cli -- test --features 1.21.11,client-test -j 1
```

Any of the 43 releases goes in place of `1.21.11` — the harness carries the `#[cfg]` branches for
all of them, so nothing under `src/test/java` names a release. What does vary is the JVM the client
boots on; see [JDK requirement](#jdk-requirement).

```
   Compiling hellomod
    Starting 3 tests across 1 class
        PASS [  30.170s] com.example.hellomod.ClientTest#bootsToTheTitleScreen
        PASS [  30.123s] com.example.hellomod.ClientTest#opensAScreenAndFindsAWidgetByItsLabel
        PASS [  35.053s] com.example.hellomod.ClientTest#placesABlockThroughTheIntegratedServer
------------
     Summary [  95.350s] 3 tests run: 3 passed
```

A test reads like a browser driver, except that both handles are typed game objects:

```java
try (GameClient game = GameClient.launch()) {
    game.openWorld("jals-test");
    BlockPos pos = new BlockPos(0, 0, 0);
    game.runOnServer(server ->
        server.overworld().setBlockAndUpdate(pos, Blocks.DIAMOND_BLOCK.defaultBlockState()));
    assert game.evalOnServer(server -> server.overworld().getBlockState(pos).is(Blocks.DIAMOND_BLOCK));
}
```

### What this project still owns

Three things, and each is here because it cannot live on the other side of the edge:

- **`client-test`.** `["client", "mc-client-test/enabled"]` — it implies `client` so this project
  compiles against the client jar, and it says the harness is wanted. It names no release: each of
  the 43 version features carries its own `mc-client-test/<release>` beside its
  `minecraft/<release>`, so the release reaches the harness by the route it already took to the SDK.
  Everything under `src/test/java` is `#[cfg]`-gated on `client-test`, so a selection that does not
  name it compiles and lints as if the tests were not there, which is what keeps the other two
  `minecraft_mod` CI cells unchanged.
- **The second route on every version feature.** It is the one real cost of the split, and it is
  paid here rather than in the harness because a feature reaches a dependency only through the
  manifest that declares the edge. It costs a selection that never asks for the harness nothing: the
  harness registers no task and compiles no type without `enabled`, and the release it is handed is
  the one this project already routed to the same SDK node.
- **`-Xmx2G`.** `build.add_jvm_arg` reaches a test JVM only from the *root* project's script, so a
  dependency cannot contribute it. `build.rhai` writes the line; without it the boot dies inside the
  resource reload with an `OutOfMemoryError`.

Everything else — the harness class and its fourteen thresholds, the 2287 pinned runtime libraries,
the `Executor` hinge, the daemon-thread and watchdog dance `jals test` forces, and why there is no
native directory and no asset store — is documented in [`../minecraft_client_test/README.md`](../minecraft_client_test/README.md).

**Linux only.** GLFW wants the main thread on macOS (`-XstartOnFirstThread`) and the main thread
belongs to the test. CI runs the cell under `xvfb` with Mesa's llvmpipe.

## Legal note

Minecraft jars and mappings are Mojang's copyrighted material. This example records only download
URLs and digests; artifacts stay local to your machine and must not be redistributed.
