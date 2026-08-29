# Driving the real Minecraft client, and photographing what it draws

A `[[test-target]]` that boots the actual game client headless, opens a screen, takes a screenshot,
and hands it to jals to compare against a reference image.

```sh
cargo run -p jals-cli -- test --target client-e2e --features 1.21.11
```

```
   Compiling client-e2e
    Starting 2 tests across 2 classes
        PASS [  15.106s] com.example.e2e.TitleScreen#renders
        PASS [  15.129s] com.example.e2e.OptionsScreen#renders
     Summary [  36.604s] 2 tests run: 2 passed
```

**Read [Where this stands](#where-this-stands) before adopting it.** The pipeline works end to end
and is gated in CI; the *screenshots it produces are not yet reproducible*, for a reason that is the
game's and not jals's, and the fix is described there.

## The three things it demonstrates

### 1. No Mixin, no agent, no launcher

The driver is a plain class on the classpath. It starts the game on the main thread and scripts it
from a second one:

```java
Thread script = new Thread(driver, "jals-e2e-driver");
script.setDaemon(true);
script.start();
net.minecraft.client.main.Main.main(gameArgs);
```

That works because `Minecraft` implements `java.util.concurrent.Executor`, so `execute(Runnable)`
**keeps its name through obfuscation** — it is an override of a JDK interface method, which no
mapping set may rename. A thread that is not the render thread can therefore schedule work onto it:

```java
client.execute(() -> Screenshot.grab(runDir, name, client.getMainRenderTarget(), 1, done));
```

Everything else follows from that one hinge. A mixin would need a transformer, a transformer would
need a mod loader or a java agent, and the example would be about loaders instead of about tests.

Two consequences worth knowing:

- **The driver must be in a named package.** In the default package it lands beside the game's own
  obfuscated classes and the JVM refuses it: `SecurityException: signer information does not match`.
- **The game exits the JVM.** `Main.main` never returns, so the report is written *before* the
  driver asks the client to stop, not after.

### 2. The runtime is declared, not installed

A launcher assembles a client from the release metadata. `build.rhai` declares the same thing, every
byte pinned by digest:

| what | how |
| --- | --- |
| the game jar | `[dependencies] minecraft` — the SDK fetches and **remaps it to official names**, which is what lets the driver be written in Java rather than in reflection |
| 52 libraries | `tasks.fetch_jar` + `tasks.add_classpath` |
| 9 native jars | `tasks.extract_files` per jar, `tasks.merge_trees`, `tasks.add_runtime_dir("natives", …)` |
| 45 asset objects | `tasks.fetch_bytes` + `tasks.place` + `merge_trees`, `add_runtime_dir("assets", …)` |

The two directories are addressed by name, because their paths are content digests that do not exist
when the manifest is written:

```toml
args     = ["--assetsDir", "{dir:assets}", …]
jvm-args = ["-Djava.library.path={dir:natives}"]
```

**The asset slice is the reason this fits in CI.** The full store is 4591 objects and 430 MiB; drop
the sounds and the 142 languages nobody reads and it is 45 objects and 16 MiB. A screenshot never
plays a sound.

`build.rhai` is generated — 105 pinned URLs are not something to maintain by hand:

```sh
examples/scripts/gen-client-runtime.py 1.21.11
```

### 3. The remapped jar is what runs

`minecraft_mod` reobfuscates its output because a mixin is loaded into a *vanilla* launcher. Here
there is no vanilla launcher — the target is the launcher — so it runs the same deobfuscated jar it
compiled against, exactly as a modded development environment does.

Making that work needed a fix in the remap itself: a signed jar's signature describes bytes that no
longer exist once every class has been renamed, and a JVM refuses such a jar outright
(`SecurityException: SHA-384 digest error`). `JarRemap` now drops the signature block and the manifest's
per-entry digests, which are 3.7 MB of stale claims in a Minecraft client jar.

## Where this stands

**Verified, end to end, on a real client:** the fetch, the runtime directories, the boot (about seven
seconds to a first screen), the driving, the shutter, the report, and `--update-golden` packaging an
archive and printing its digest. All of that is what the CI cell gates.

**Not yet true: the screenshots are reproducible.** They are not, and the cause is the game's:

> Photographing the same screen twice **in one process**, fifteen seconds apart, gives frames that
> differ in **67% of their pixels**.

Every menu in 1.20.5 and later is drawn over a panorama that rotates with the wall clock, and the
options screen inherits it. Waiting longer does not help — it is not a fade that finishes, it is an
animation that never stops. Masking is not the answer either: the moving part is the whole
background.

**The fix is a scene without a panorama, which means a world.** Inside a level there is no panorama,
and the remaining motion — clouds, the sun, entities — stops when the tick is frozen. That needs the
driver to create a superflat world with a fixed seed (superflat generation has no noise, so it is
deterministic), wait for it to load, freeze the tick, and only then open a screen. It is more driver
code against a larger API, and it is the next thing this example needs.

Until then the `[[golden.client-e2e]]` entry below points at a URL that does not exist, and running
without `--update-golden` will fail at the fetch. That is deliberate: an entry with a plausible URL
and a wrong digest would be worse.

## Reference images

Reference images are not committed. They are binary, about 250 KiB each, and regenerated whenever
Mesa moves — three arguments against a repository, and none against the machinery `jals.toml`
already has for pinning bytes by digest.

```sh
# Inside the CI container, so the renderer that bakes them is the renderer that checks them.
jals test --target client-e2e --features 1.21.11 --update-golden
```

That writes an archive under `target/jals/test/golden-update/` and prints the `[[golden.client-e2e]]`
block to paste once it is uploaded — digest and byte cap already computed.

**The renderer has to be pinned.** `LIBGL_ALWAYS_SOFTWARE=1` with Mesa's llvmpipe is deterministic
run to run and across thread counts, but a *different* rasterizer is not: rendering one scene under
softpipe instead changes 11.9% of its pixels. That is also why
`[test-target.screenshots] threshold` is `0.0` — at pixelmatch's default of `0.1`, a whole rasterizer
swap reports a clean pass.

## Adding a release

1. `examples/scripts/gen-client-runtime.py <version>` — rewrites `build.rhai`.
2. Add the version to `[features]`, routing `minecraft/<version>` and `minecraft/client`.
3. Update `--assetIndex` in `[[test-target]] args`: the id comes from the release metadata and
   changes between releases. The generator prints it.
4. Bake and publish a golden archive for that release.

Only one release is wired up, deliberately. `minecraft_mod` builds against 43 because a *jar* is
cheap to produce for each; a screenshot suite is not — every release needs its own reference images,
baked in the container that renders them.

## Legal note

Minecraft jars, mappings and assets are Mojang's copyrighted material. This example records only
download URLs and digests; artifacts stay local to your machine and must not be redistributed.
