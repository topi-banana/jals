# A Minecraft mixin mod, reobfuscated for every release from 1.14.4 to 26.2

One mixin, one resource, one jar. It prints `Hello, world` as the dedicated server object finishes
construction, and it builds against any of the 43 releases
[`examples/minecraft`](../minecraft) knows:

```sh
cargo run -p jals-cli -- build --manifest-path examples/minecraft_mod/jals.toml --features 1.20.1
# → examples/minecraft_mod/target/jals/remap/hellomod-0.1.0.jar
```

Three declarations in `jals.toml` carry the whole thing:

- **`[dependencies] minecraft`** — a `path` dependency on the SDK example. Its build script fetches
  the release, remaps it with the official mappings, and puts the game jar, `mixin-0.8.7.jar` and
  `mixinextras-common-0.5.4.jar` on *this* project's compile classpath, with the matching sources
  published as navigation trees an editor can open.
- **`[[mappings.mojmap]]`** — 39 feature-gated alternatives of one name, one per release that ships
  obfuscated.
- **`[build] remap`** — reobfuscate the compiled classes with whichever alternative the selection
  activates, and package them, resources included, into a jar.

**Building the jar is the deliverable.** Loading it needs a Mixin-capable launcher, which jals is
not and this example does not ship — see [Running it](#running-it).

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

Every URL is content-addressed by the same SHA-1 it is checked against, so one digest pins both the
address and the bytes. `max-bytes` is the published size rounded up to the next MiB; the digest is
what guarantees the content, so pinning an exact byte count would only ever break on a re-serve of
identical text.

To refresh the table: for each catalog entry in [`../minecraft/build.rhai`](../minecraft/build.rhai)
whose `obfuscated?` flag is true, fetch
`https://piston-meta.mojang.com/v1/packages/<metadata sha1>/<version>.json` and project
`downloads.server_mappings.{url, sha1, size}`.

## The two jars

| selection                  | mapping    | what comes out                                            |
| -------------------------- | ---------- | --------------------------------------------------------- |
| `jals build` (26.2)        | none active | classes packaged under their own names                    |
| `--features 1.21.11`       | active      | `@Mixin(ary.class)`                                        |
| `--features 1.20.1`        | active      | `@Mixin(ahe.class)`                                        |
| `--features 1.14.4`        | active      | `@Mixin(uk.class)`                                         |

The 26.x row is the point of the design rather than a hole in it. Those four releases ship
deobfuscated and declare no mappings download, so no alternative names them and the step packages
without rewriting — which is exactly the right jar, because the names already in it are the ones
that runtime loads. "This selection ships no mappings" says *do not rewrite the names*, not *produce
nothing*.

```sh
$ unzip -l target/jals/remap/hellomod-0.1.0.jar
  META-INF/MANIFEST.MF
  com/example/hellomod/mixin/HelloMixin.class
  mixins.hellomod.json
```

The member list is identical in both branches: only the *contents* of `HelloMixin.class` differ, and
`mixins.hellomod.json` rides through untouched because a remap rewrites class files and leaves every
other archive member alone.

## Why `remap = false`, and where the line is

```java
@Mixin(value = DedicatedServer.class, remap = false)
public class HelloMixin {
    @Inject(method = "<init>", at = @At("RETURN"), remap = false)
    private void hellomod$helloWorld(CallbackInfo callback) {
        System.out.println("Hello, world");
    }
}
```

jals rewrites annotation **`Class`** values and never annotation **strings**, and it generates no
refmap. Everything about this mixin follows from that:

- `@Mixin`'s `value` is a `Class` value, so `[build] remap` rewrites it. On 1.20.1 `javap -v` on the
  packaged class reads `org.spongepowered.asm.mixin.Mixin(value=[class Lahe;], remap=false)`. The
  rewrite covers `RuntimeInvisibleAnnotations` as well as the visible ones, which is load-bearing
  here: `@Mixin` is `CLASS`-retained and `@Inject` is `RUNTIME`-retained, so this one mixin uses
  both attributes.
- `method = "<init>"` names a constructor, which is `<init>` in every namespace and appears in no
  mapping. `@At("RETURN")` names an injection point, not a member. Neither needs a refmap.
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

## Resources

`[build] resource-dirs` (defaulting to `["src/main/resources"]`) names the directories packaged into
the jar alongside the classes. Mixin has to read `mixins.hellomod.json` at load time, so without it
the jar is a jar of class files nothing will ever apply.

```json
{
  "required": true,
  "minVersion": "0.8",
  "package": "com.example.hellomod.mixin",
  "mixins": ["HelloMixin"],
  "injectors": { "defaultRequire": 1 }
}
```

No `"refmap"` — declaring one that is absent logs a warning on every load, and there is none to
declare. No `"compatibilityLevel"` either: it must be at least the class-file version `javac`
produced, and this project leaves `[build] release` unset (below), so setting the two is one paired
decision left to whoever adapts this.

Resources are authored project files, so they are read from the project snapshot rather than walked
off disk, and they reach the **jar only** — `jals run` executes `classes-dir`, which is compiler
output and never receives them.

## Features

The 43 version features and the four SDK axes are declared here as *local* names that route into the
dependency:

```toml
[features]
server = ["minecraft/server"]
"1.20.1" = ["minecraft/1.20.1"]
```

Declaring them locally is what lets `[[mappings.mojmap]]` gate on them; the `minecraft/…` entry is
pure routing and is never queryable in this project.

There is no `default` list. `[dependencies] minecraft` leaves `default-features` at its `true`, so
the SDK resolves its own `default = ["server", "mixin", "mixinextras"]` and falls back to its own
newest release. `jals build` with no flags therefore builds against 26.2, with Mixin and MixinExtras
on the classpath.

**Version exclusivity is not restated here.** The forwarded selection reaches the SDK's own build
script, whose `build.error` rejects a second version before any download:

```
$ cargo run -p jals-cli -- build --manifest-path examples/minecraft_mod/jals.toml --features 1.20.1,1.19.4
error: resolving the project dependency graph: dependency build script
`…/examples/minecraft` failed: build script reported 1 error diagnostic(s)
```

`--all-features` fails identically, for the same reason, and always will. A second copy of that
43-row rule in this manifest would be a second thing to keep in sync.

Note what that message does *not* say: a build script's own diagnostic text is printed when the
script belongs to the root project, but a **dependency's** is currently reported only by count, so
the actual sentence — `select at most one Minecraft version feature, got '1.20.1' and '1.19.4'` — is
not shown. Build the SDK directly to read it:

```sh
cargo run -p jals-cli -- build --manifest-path examples/minecraft/jals.toml --features 1.20.1,1.19.4
```

## JDK requirement

`[build] release` is deliberately unset. `--release N` cannot relax the binding constraint anyway:
to compile against the SDK's classpath `javac` must be able to **read** the game's class files, and
a JDK 21 `javac` rejects a Java 25 class outright. Your JDK must be at least:

| releases         | JDK |
| ---------------- | --- |
| 1.14.4 – 1.16.5  | 8   |
| 1.17 – 1.17.1    | 16  |
| 1.18 – 1.20.4    | 17  |
| 1.20.5 – 1.21.11 | 21  |
| 26.x             | 25  |

The mixin itself needs nothing above Java 8, so leaving the output at the JDK's default is the
honest position. Set `[build] release` yourself if you care, together with the mixin config's
`compatibilityLevel`.

## Running it

Out of scope, and stated rather than implied. The jar carries the names vanilla loads, but something
has to install Mixin's transformer into the JVM before the game classes are loaded — a mod loader,
or a launcher built around `MixinBootstrap`. jals is a build tool: it neither generates nor
validates loader metadata, and this example ships none (no `fabric.mod.json`, no
`META-INF/mods.toml`).

If you adapt this for a specific loader, note that the reobfuscation target here is **vanilla's own
obfuscated namespace**. A loader that deobfuscates the game to some other namespace at runtime
(Fabric's intermediary, older Forge's SRG) wants a different mapping set on the `[build] remap`
line, not a different mixin.

## Legal note

Minecraft jars and mappings are Mojang's copyrighted material. This example records only download
URLs and digests; artifacts stay local to your machine and must not be redistributed.
