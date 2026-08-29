# Driving the real Minecraft client, and photographing what it draws

A `[[test-target]]` that boots the actual game client headless, opens the screen this project
draws, checks a fact about it, photographs it, and hands the picture to jals to compare against a
reference image.

```sh
cargo run -p jals-cli -- test --target client-e2e --features 1.21.11
```

```
   Compiling client-e2e
    Starting 2 tests across 2 classes
        PASS [  15.115s] com.example.e2e.HelloScreen#renders
        PASS [  15.149s] com.example.e2e.OptionsScreen#renders
     Summary [  40.252s] 2 tests run: 2 passed
```

The pictures are reproducible: two separate boots under a pinned software renderer write the same
PNG byte for byte, which is why the comparison runs at a threshold of zero and with no masks. That
took one setting and one measurement, and [Determinism](#determinism) is the whole of it.

## The four things it demonstrates

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

Three consequences worth knowing:

- **The driver must be in a named package.** In the default package it lands beside the game's own
  obfuscated classes and the JVM refuses it: `SecurityException: signer information does not match`.
- **The game exits the JVM.** `Main.main` never returns, so the report is written *before* the
  driver asks the client to stop, not after.
- **A screen appearing is not the game being ready.** The resource reload that runs behind the first
  screen ends by calling `setScreen(new TitleScreen(…))` itself, so a driver that starts as soon as
  `client.screen != null` opens its own screen and has it silently replaced a moment later. The
  driver waits for the overlay to go away *and* the title screen to be the one showing.

### 2. What is under test is the project's own code

`src/main/java` holds `HelloScreen` — a `Screen` drawn with the game's font, widgets and background.
`src/e2e/java` holds the driver, and it is the *target's* source root rather than the build's:

```toml
[build]
source-dirs = ["src/main/java"]

[[test-target]]
source-dirs = ["src/e2e/java"]
```

So `jals build` produces the screen and nothing else, and only a target run additionally compiles
the thing that photographs it. The reference images are pictures of the product.

The `HelloScreen#renders` case checks a fact before it takes the picture — that the button exists as
an object with the label the project declares:

```java
String label = hello.button().getMessage().getString();
if (!HelloScreen.BUTTON_LABEL.getString().equals(label)) { … }
```

A photograph proves a button was drawn; this proves it is the button `HelloScreen` says it builds,
which is what a refactor breaks first. Both halves arrive through the same report, and either can
fail the test.

**Why a screen and not a mixin.** There is no mod loader in this run, so nothing would apply one.
What a jals-built project *can* contribute to a client that has it on the classpath is code the run
calls — and a `Screen` is that at its smallest, going through the same rendering path a loaded mod's
GUI would.

### 3. The runtime is declared, not installed

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

### 4. The remapped jar is what runs

`minecraft_mod` reobfuscates its output because a mixin is loaded into a *vanilla* launcher. Here
there is no vanilla launcher — the target is the launcher — so it runs the same deobfuscated jar it
compiled against, exactly as a modded development environment does.

Making that work needed a fix in the remap itself: a signed jar's signature describes bytes that no
longer exist once every class has been renamed, and a JVM refuses such a jar outright
(`SecurityException: SHA-384 digest error`). `JarRemap` now drops the signature block and the manifest's
per-entry digests, which are 3.7 MB of stale claims in a Minecraft client jar.

## Determinism

A screenshot suite is worth exactly as much as the reproducibility of its screenshots, and there was
one thing standing in the way of it.

**Every menu since 1.20.5 is drawn over a panorama that rotates with the wall clock.** Photographing
one screen twice in a single process, fifteen seconds apart, gave frames differing in **67% of their
pixels**. Waiting does not help: it is not a fade that finishes, it is an animation that never stops.
Masking does not either, because the moving part is the whole background.

The fix is one line, and it is an accessibility option rather than a debug switch. `PanoramaRenderer`
advances the angle by

```
spin += realtimeDeltaTicks * panoramaSpeed * 0.1f
```

so at `panoramaScrollSpeed:0.0` the term is zero, `spin` stays at the `0.0f` it is constructed with,
and two *separate processes* write a byte-identical PNG. `fixtures/run/options.txt` seeds it.

Three more things follow from measuring rather than guessing:

- **Wait for what settles.** A screen fades in; photographing one four seconds after it opens gives
  a frame 20% different from the next run's, all of it mid-fade. The driver waits fifteen seconds
  before every shutter. That is why there are no masks over the buttons.
- **Choose a scene without what does not settle.** The title screen is deliberately *not* among the
  shots. Its splash text is drawn at random from a list on every launch — and on three days of the
  year from a fixed set of a different length — so the region it occupies cannot be bounded. A mask
  sized on ordinary splashes would pass all year and fail at Christmas. It is also the only screen
  carrying a network-dependent widget, the Realms notification.
- **Pin the renderer.** `LIBGL_ALWAYS_SOFTWARE=1` with Mesa's llvmpipe is deterministic run to run
  and across thread counts, but a *different* rasterizer is not: rendering one scene under softpipe
  instead changes 11.9% of its pixels. That is also why `[test-target.screenshots] threshold` is
  `0.0` — at pixelmatch's default of `0.1`, a whole rasterizer swap reports a clean pass.

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

Until such an archive exists there is nothing to fetch, and the manifest says so rather than pointing
at a URL that resolves to nothing: the entry is gated on a `published-golden` feature this checkout
does not enable, so it is simply not active and every shot is reported as unreferenced. Add the
feature to the selection once you have published one.

To judge a run without publishing anything, unpack an archive and point `--golden` at it:

```sh
jals test --target client-e2e --features 1.21.11 --update-golden
unzip -o target/jals/test/golden-update/client-e2e-1.21.11.zip -d /tmp/reference
jals test --target client-e2e --features 1.21.11 --golden /tmp/reference
```

That pair *is* the reproducibility check — bake once, judge a fresh boot against it — and it is what
the CI cell runs. A second pass that comes back green means two separate processes rendered
identical frames; because a reference the run did not produce is reported too, it also means neither
shot silently disappeared.

On this machine the two bakes agree so exactly that the *archives* have the same SHA-256, which is
the strongest form the claim takes: not "the pictures compare equal", but "the whole set is the same
bytes".

A reported path is a claim, not a fact — the program under test writes the report — so jals holds
each one to `[test-target.screenshots] dir`: a `..`, an absolute path, or a file somewhere else
under the run directory is refused rather than read, which is what keeps `--update-golden` from
packaging something the run never photographed.

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
