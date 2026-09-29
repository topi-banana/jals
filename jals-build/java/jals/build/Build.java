package jals.build;

/**
 * The host directives a script declares.
 *
 * <p>Everything here is recorded, not performed: arguments, environment entries, tracked inputs,
 * and diagnostics are buffered with the generated files and committed together when the script
 * finishes. The two calls that are the exception are the reads — {@link #env} and
 * {@link #feature} — which answer from what the host supplied this run.
 *
 * <p>A refusal is not a Java exception: a path that is not portable, a limit that a collection or a
 * directive would exceed, or an environment name with a {@code =} in it stops the run with a
 * message naming the operation and the value, exactly where the call was made.
 */
public final class Build {

    /**
     * The value of environment variable {@code name}, or {@code null} when it is absent.
     *
     * <p>A script sees only what the host supplied deliberately: for CLI builds, {@code JALS_}-
     * prefixed host variables plus the fixed project values ({@code OUT_DIR},
     * {@code JALS_MANIFEST_DIR}, and the optional package name/version). The rest of the host
     * environment is withheld, because a script can forward anything it reads into a task fetch
     * URL and a dependency's script is code nobody reviewed.
     */
    public static String env(String name) {
        int size = envSize(Text.of(name));
        if (size < 0) {
            return null;
        }
        char[] value = new char[size];
        Text.entryInto(0, value);
        return new String(value);
    }

    /**
     * Whether build feature {@code name} is enabled for this project.
     *
     * <p>Enabled means selected here: the root's own {@code [features]} selection, or, for a
     * dependency, what its dependents asked for closed over its own {@code [features]}. A
     * {@code <dependency>/<feature>} name is never enabled — it is a forwarding directive. The
     * resolved feature set is always part of the fingerprint, whether or not the script reads it.
     */
    public static boolean feature(String name) {
        return feature0(Text.of(name));
    }

    /** Every enabled build feature for this project, in lexical order. */
    public static String[] features() {
        return Project.entries(featureCount());
    }

    /**
     * Track one project file for cache invalidation.
     *
     * <p>A script that calls this at least once narrows project-file tracking to the declared set;
     * a script that calls it never fingerprints every project file except the managed build tree.
     * Managed build output cannot be tracked, so a generated file can never invalidate — or
     * certify — its own build.
     */
    public static void rerunIfChanged(String path) {
        rerunIfChanged0(Text.of(path));
    }

    /** Track one supplied environment value for cache invalidation. */
    public static void rerunIfEnvChanged(String name) {
        rerunIfEnvChanged0(Text.of(name));
    }

    /**
     * Add a project file — or the key {@link Output#write} returned — to the later source set.
     *
     * <p>Added sources are compiled by the same ordinary compile as the project's own sources, and
     * are how a generator hands a type to the program it generates it for.
     */
    public static void addSource(String path) {
        addSource0(Text.of(path));
    }

    /**
     * Add a project file — or the key {@link Output#write} returned — to the later classpath.
     *
     * <p>A classpath entry is on the compile's classpath and in analysis, exactly like a {@code
     * jar} dependency's classes.
     */
    public static void addClasspath(String path) {
        addClasspath0(Text.of(path));
    }

    /**
     * Append one {@code javac} argument, in call order.
     *
     * <p>Root-script arguments follow the manifest's {@code javac-flags} and stay before source
     * paths. This is inert during the script phase and affects the JDK subprocess a later CLI
     * {@code build}/{@code run} starts — so a root build script remains trusted project code, not a
     * security boundary for that process.
     */
    public static void addJavacArg(String arg) {
        addJavacArg0(Text.of(arg));
    }

    /** Append one JVM argument, in call order. Root-script arguments precede {@code -cp}. */
    public static void addJvmArg(String arg) {
        addJvmArg0(Text.of(arg));
    }

    /** Add one entry to the compiler's environment, replacing an earlier value for the name. */
    public static void setCompileEnv(String name, String value) {
        setCompileEnv0(Text.of(name), Text.of(value));
    }

    /** Add one entry to the runtime's environment, replacing an earlier value for the name. */
    public static void setRunEnv(String name, String value) {
        setRunEnv0(Text.of(name), Text.of(value));
    }

    /** Report a non-fatal diagnostic. Warnings travel with a fatal one when one is reported. */
    public static void warning(String message) {
        warning0(Text.of(message));
    }

    /**
     * Report a fatal diagnostic.
     *
     * <p>Recorded rather than thrown: the script keeps running, and every diagnostic it emits
     * afterwards stays with the ones already reported. At the end, any error diagnostic means no
     * generated file is published — and the failure reports every diagnostic the run produced, in
     * order, so a warning that preceded the fatal one stays as its context.
     */
    public static void error(String message) {
        error0(Text.of(message));
    }

    /**
     * Record deterministic host-readable metadata under {@code key}.
     *
     * <p>Metadata rides along in the build-script output for host integrations and changes no tool
     * invocation. The key must not be empty.
     */
    public static void metadata(String key, String value) {
        metadata0(Text.of(key), Text.of(value));
    }

    private static native int envSize(char[] name);

    private static native boolean feature0(char[] name);

    private static native int featureCount();

    private static native void rerunIfChanged0(char[] path);

    private static native void rerunIfEnvChanged0(char[] name);

    private static native void addSource0(char[] path);

    private static native void addClasspath0(char[] path);

    private static native void addJavacArg0(char[] arg);

    private static native void addJvmArg0(char[] arg);

    private static native void setCompileEnv0(char[] name, char[] value);

    private static native void setRunEnv0(char[] name, char[] value);

    private static native void warning0(char[] message);

    private static native void error0(char[] message);

    private static native void metadata0(char[] key, char[] value);
}
