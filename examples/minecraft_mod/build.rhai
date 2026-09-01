// What `javac` needs that varies with the Minecraft release, and nothing else.
//
// There is deliberately no catalog of versions here. `jals.toml` routes each of its 43 version
// features into the SDK, and the SDK's own build script is what rejects a second one; a table of
// releases in this file would be a second copy of that rule and the first of the two to drift.
// What this script reads is the *threshold* chain instead — four names, three of which are the
// steps the game's own JVM took — so a release added to `jals.toml` needs nothing here at all.

// A release has to be named. `since-1.14.4` is the bottom of the threshold chain and every version
// feature reaches it, so this is the one question the chain can answer that no single release can:
// was a release selected at all? Without one the SDK still falls back to its own newest release —
// it has a `default` and this project does not — while every threshold here stays off, and the
// source would take its oldest branch against the newest game. Saying so costs one line and one
// name; finding out from `javac` costs a download and a "cannot find symbol".
if !build.feature("since-1.14.4") {
    build.error(
        "select a Minecraft version feature, e.g. `--features 26.2`. There is deliberately no "
            + "default: a release chooses the game jar, the mapping set and every `#[cfg]` branch "
            + "at once, and this project cannot see which release the SDK fell back to."
    );
    return;
}

// The class-file level this mod is compiled at. `--release` rather than `source`/`target`, because
// only `--release` also pins the platform API the mod may reach for, and a mod that quietly used a
// method added in Java 17 would load on 1.18 and fail on 1.17.
//
// The level follows the game's own JVM: Java 8 through 1.16.5, 16 for 1.17, 17 for 1.18, 21 from
// 1.20.5 — the era table in `README.md`. It stops there rather than continuing to 25 for 26.x,
// because the framework this mod compiles against is SpongePowered Mixin 0.8.7, whose
// `compatibilityLevel` vocabulary ends at `JAVA_21`, and a mixin class Mixin cannot name a level
// for is one it refuses to apply. Capping costs the mod nothing: a Java 21 class file loads on the
// Java 25 JVM 26.x runs on, and this mixin needs no language feature above Java 8 anyway.
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
if release <= 8 {
    // A current JDK still compiles at 8 but says so twice on every build. The releases that ask
    // for it are 1.14.4 through 1.16.5, whose game jars are Java 8 class files; the warning is
    // about the option, not about this project.
    build.add_javac_arg("-Xlint:-options");
}

// `mixin-0.8.7.jar` registers two annotation processors in `META-INF/services`, and the SDK puts
// that jar on this project's compile classpath. Nothing in this build wants them: their job is the
// refmap, and this example ships none — it addresses its target through an annotation `Class`
// value that `[build] remap` rewrites, which is the whole point of it. Whether a `javac` starts a
// processor it merely *found* on the class path is a property of the JDK (since 23 it does not),
// so saying it here is what keeps the build from depending on which JDK a reader has installed.
build.add_javac_arg("-proc:none");

// --- the client end-to-end tests -------------------------------------------------------------
//
// `client-test` selects the whole test arrangement: it implies `client` (so this project compiles
// against the client jar) and routes `enabled` into the `mc-client-test` dev-dependency, whose own
// build script then puts that release's ~60 runtime libraries on the classpath. Which release is
// not decided here either — every version feature in `jals.toml` routes itself to the harness the
// same way it routes itself to the SDK. None of that is here, and none of it is mod-specific — see
// `examples/minecraft_client_test/`.
//
// There is no release guard left to write. `since-1.14.4` above already rejected a selection that
// named no release, and the harness supports every release that names one.
//
// What stays is the one thing a dependency cannot contribute: `build.add_jvm_arg` reaches the test
// JVM only from the *root* project's script, so every consumer of the harness writes this line.
if build.feature("client-test") {
    // The client is given its own heap rather than whatever a quarter of the runner's RAM comes to,
    // so the test behaves the same on a laptop and on a CI machine. Without it the boot dies inside
    // a resource reload with an `OutOfMemoryError`, which reads as a harness bug and is not one.
    build.add_jvm_arg("-Xmx2G");
}
