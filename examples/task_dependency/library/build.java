import jals.build.Build;
import jals.build.Tasks;

/**
 * A library whose classes are produced by a build task rather than checked in.
 *
 * <p>Nothing here is aware of being a dependency: the same script is what this project would run
 * as a root. What differs is where the results land — for a consumer they are projected into *its*
 * verified cache, and this directory is never written to.
 */
@SuppressWarnings("naming-convention")
class build {

    public static void main() {
        // `projectJar` keeps the example network-independent. A pinned remote archive substitutes
        // directly, since every downstream handle is the same:
        //
        //     int jar = Tasks.fetchJar(
        //         Tasks.httpsUrl("https://downloads.example.invalid/example.jar"),
        //         Tasks.sha256("<64 lowercase hexadecimal characters>"),
        //         Tasks.bytes(16777216));
        int jar = Tasks.projectJar("vendor/example.jar");

        // Reaches the consumer's compile classpath and its analysis, exactly like a `jar`
        // dependency.
        Tasks.addClasspath(jar);

        if (Build.feature("sources")) {
            // Reaches the consumer as read-only navigation sources, addressed `net/example/…` — the
            // destination's source root (`src/main/java`) is what gets stripped. They are never
            // compile inputs: the classpath jar above already defines these types.
            int sources = Tasks.extractJava(jar, "net/example");
            Tasks.publishTree(
                "example-sources", sources, "src/main/java/net/example", "navigation");
        }
    }
}
