import jals.build.Tasks;

/**
 * A one-step task graph: read a checked-in source archive out of the project, extract its
 * `net/example` package, and hand the tree to the one destination that owns it.
 *
 * <p>The destination directory is *replaced*, not merged: the publication is exclusive, so the
 * first successful run makes `src/main/java/net/example` exactly the archive's contents and later
 * runs keep it that way.
 */
@SuppressWarnings("naming-convention")
class build {

    public static void main() {
        int jar = Tasks.projectJar("vendor/example-sources.jar");
        int sources = Tasks.extractJava(jar, "net/example");
        Tasks.publishTree("example-sources", sources, "src/main/java/net/example", "navigation");
    }
}
