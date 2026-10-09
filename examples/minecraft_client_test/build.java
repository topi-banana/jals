import jals.build.Build;
import jals.build.Project;
import jals.build.Tasks;

// What a Minecraft client needs at *run* time, which is a different question from what compiling
// against one needs.
//
// The SDK dependency supplies the remapped game jar and its navigation sources; what it does not
// supply is the ~60 libraries a client loads when it boots — LWJGL and its native classifier jars,
// icu4j, oshi, jopt-simple, the netty/guava/log4j stack — because the SDK's job is the game jar,
// and a launcher's library set is per release and per platform.
//
// So they are pinned in `runtime.tsv`, for one platform and *every* release in the SDK's catalog,
// by a generator. The table is a data file rather than Java source on purpose: a script cannot walk
// the release metadata's `libraries` array — `Tasks.fetchJson` hands back a task handle and the
// fetch happens after this script has returned, so `Tasks.jsonAt` reads one named field and there
// is no index and no loop. `examples/scripts/gen-client-runtime.py` writes `runtime.tsv`, and this
// script parses it; a regeneration is therefore a diff of the one file whose content moved.
//
// A consumer reaches these through its own `[dev-dependencies]` edge: a dependency's
// `Tasks.addClasspath` lands on the consumer's classpath, which under `jals test` is also the
// classpath the test JVM runs on. What does *not* cross that edge is an argument —
// `Build.addJvmArg` and `Build.addJavacArg` alike reach a JVM or a compile from the root project's
// script only — so a consumer supplies `-Xmx2G` itself, and compiles this project's sources at its
// own `--release`. See `README.md`.
@SuppressWarnings("naming-convention")
class build {

    public static void main() {
        // A release has to be named, and naming one is the whole of how this harness is selected.
        // There is no second feature asking whether it is wanted: a consumer declares this project
        // in `[dev-dependencies]` precisely because its tests boot a client, and every version
        // feature it owns routes itself here the same way it routes itself to the SDK.
        //
        // `since-1.14.4` is the bottom of the threshold chain and every one of the 44 releases
        // reaches it, so it is the one question the chain can answer that no single release can:
        // was a release selected at all? Without one the SDK still falls back to its own newest
        // release — it has a `default` and this project does not — while every threshold here stays
        // off, and `GameClient.java` would take its oldest branch against the newest game. Saying
        // so costs one line; finding out from `javac` costs a client download and a wall of "cannot
        // find symbol".
        if (!Build.feature("since-1.14.4")) {
            Build.error(
                "select a Minecraft version feature, e.g. `--features 1.20.1`. This harness is"
                    + " written against 44 releases and compiles the one it is told about; there"
                    + " is deliberately no default, because a release chooses the game jar, the"
                    + " runtime libraries and every `#[cfg]` branch at once.");
            return;
        }

        // The class-file level for *this project's own* `jals build` — the cell that checks the
        // harness still compiles against the release it claims. It governs nothing on a consumer's
        // side: a dependency's `Build.addJavacArg` does not cross the edge any more than its
        // `Build.addJvmArg` does, so `GameClient.java` is compiled by whoever consumes it, at that
        // project's own `--release`.
        //
        // The numbers are the game's own `javaVersion.majorVersion` per era, because a client's
        // classes are loaded by the JVM that release runs on: a 1.14.4 client wants a Java 8 JVM,
        // and a Java 21 class file on its classpath would not load. That is also why
        // `GameClient.java` is written in Java 8 source — no `ProcessHandle`, no `Files.writeString`,
        // no `Stream.toList`, no pattern `instanceof`. It caps at 21 rather than following 26.x to
        // 25: 21 loads on every JVM from 21 up, and nothing in the harness needs a later language
        // level.
        int release = 8;
        if (Build.feature("since-1.20.5")) {
            release = 21;
        } else if (Build.feature("since-1.18")) {
            release = 17;
        } else if (Build.feature("since-1.17")) {
            release = 16;
        }
        Build.addJavacArg("--release");
        Build.addJavacArg("" + release);
        if (release <= 8) {
            // `--release 8` is supported and warned about in the same breath. The warning is about
            // the toolchain, not about this source, and CI reads warnings.
            Build.addJavacArg("-Xlint:-options");
        }

        // The selected release's libraries. "At most one release" is enforced by the SDK's own
        // build script — the release features here route `minecraft/<version>` into it — so this
        // loop does not restate the rule, exactly as `examples/minecraft_mod/jals.toml` declines to
        // restate it.
        addRuntimeLibraries(Project.readText("runtime.tsv"));
    }

    // The generated table, one release block at a time: a `[<release>]` header, then
    // `<path>\t<sha1>\t<bytes>` rows. Blocks for unselected releases are skipped whole, and the
    // scan stops at the header after the selected one — there is no index to seek to, so reaching
    // a release means walking the lines before it either way.
    private static void addRuntimeLibraries(String table) {
        boolean wanted = false;
        int at = 0;
        while (at < table.length()) {
            int end = at;
            while (end < table.length() && table.charAt(end) != '\n') {
                end = end + 1;
            }
            int stop = end;
            if (stop > at && table.charAt(stop - 1) == '\r') {
                stop = stop - 1;
            }
            if (stop > at && table.charAt(at) == '[') {
                // A header ends the previous block; nothing after the selected one matters.
                if (wanted) {
                    return;
                }
                wanted = Build.feature(table.substring(at + 1, stop - 1));
            } else if (wanted && stop > at && table.charAt(at) != '#') {
                String[] fields = fields(table.substring(at, stop));
                Tasks.addClasspath(
                    Tasks.fetchJar(
                        Tasks.httpsUrl("https://libraries.minecraft.net/" + fields[0]),
                        Tasks.sha1(fields[1]),
                        Tasks.bytes(Long.parseLong(fields[2]))));
            }
            at = end + 1;
        }
    }

    // The three tab-separated fields of one generated row: path, SHA-1, byte count.
    private static String[] fields(String line) {
        String[] fields = new String[3];
        int at = 0;
        int index = 0;
        while (index < 3) {
            int end = at;
            while (end < line.length() && line.charAt(end) != '\t') {
                end = end + 1;
            }
            fields[index] = line.substring(at, end);
            at = end + 1;
            index = index + 1;
        }
        return fields;
    }
}
