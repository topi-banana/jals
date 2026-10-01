package jals.build;

/**
 * The declarative task graph: what the project has to fetch, verify, remap and publish.
 *
 * <p>A script records work here; it never performs any. Each method adds one node to the plan the
 * host executes after the script finishes and answers that node's <em>handle</em> — an {@code int}
 * a later call uses as an input. A plan is written in dependency order: a handle may only name a
 * node an earlier call answered, and an input of the wrong kind — a JSON value where a jar belongs
 * — is refused where it is declared, not where it runs. Nothing here touches the network, the
 * disk or the clock: the host executes the recorded plan later, once, with everything validated.
 *
 * <p>A handle is the node's index in the plan, so {@code 0} is the first node a script records.
 * The number means nothing outside the run that answered it: a handle from elsewhere would name
 * whatever node held that index here, and where that is wrong the plan's own validation — missing
 * inputs, forward references, wrong kinds — refuses the declaration that wrote it.
 *
 * <p>The class is static only: it is a namespace for the task vocabulary, not an object.
 */
public final class Tasks {

    /** A value node holding {@code value}: an HTTPS URL a fetch may read. */
    public static int httpsUrl(String value) {
        return httpsUrl0(Text.of(value));
    }

    /** A value node holding {@code path}, a project-relative jar. */
    public static int projectJar(String path) {
        return projectJar0(Text.of(path));
    }

    /** A value node holding the SHA-1 digest {@code value}, lowercase hex. */
    public static int sha1(String value) {
        return digest0(Text.of(value), 0);
    }

    /** A value node holding the SHA-256 digest {@code value}, lowercase hex. */
    public static int sha256(String value) {
        return digest0(Text.of(value), 1);
    }

    /**
     * A value node holding the largest number of bytes a fetch may read.
     *
     * <p>The count must be positive and within the host's fetch limit; both are checked where the
     * node is recorded, so an impossible fetch is refused before anything runs.
     */
    public static int bytes(long value) {
        return bytes0(value);
    }

    /**
     * A node fetching {@code url} and verifying it against {@code digest}, expecting at most
     * {@code maxBytes} bytes of JSON.
     */
    public static int fetchJson(int url, int digest, int maxBytes) {
        return fetch0(url, digest, maxBytes, 0);
    }

    /**
     * A node fetching {@code url} and verifying it against {@code digest}, expecting at most
     * {@code maxBytes} bytes of jar.
     */
    public static int fetchJar(int url, int digest, int maxBytes) {
        return fetch0(url, digest, maxBytes, 1);
    }

    /**
     * A node fetching {@code url} and verifying it against {@code digest}, expecting at most
     * {@code maxBytes} bytes of UTF-8 text.
     */
    public static int fetchText(int url, int digest, int maxBytes) {
        return fetch0(url, digest, maxBytes, 2);
    }

    /**
     * The JSON value at {@code path} below {@code json}.
     *
     * <p>A path is a list of object keys or array indices, none of them empty; the empty list names
     * the value itself.
     */
    public static int jsonAt(int json, String[] path) {
        int[] ends = new int[path.length];
        return jsonAt0(json, pack(path, ends), ends);
    }

    /**
     * The first JSON object below {@code json} at {@code path} whose {@code field} is {@code value}.
     *
     * <p>The field is matched as a string, so it can name a version, a release, or a build number
     * without the script parsing the document itself.
     */
    public static int jsonFindString(int json, String[] path, String field, String value) {
        int[] ends = new int[path.length];
        return jsonFindString0(json, pack(path, ends), ends, Text.of(field), Text.of(value));
    }

    /** The HTTPS URL stored as a string at {@code path}. */
    public static int jsonUrl(int json, String[] path) {
        int[] ends = new int[path.length];
        return jsonUrl0(json, pack(path, ends), ends);
    }

    /** The SHA-1 digest stored as a string at {@code path}. */
    public static int jsonSha1(int json, String[] path) {
        int[] ends = new int[path.length];
        return jsonDigest0(json, pack(path, ends), ends, 0);
    }

    /** The SHA-256 digest stored as a string at {@code path}. */
    public static int jsonSha256(int json, String[] path) {
        int[] ends = new int[path.length];
        return jsonDigest0(json, pack(path, ends), ends, 1);
    }

    /** The count stored at {@code path}, read as a byte size. */
    public static int jsonU64(int json, String[] path) {
        int[] ends = new int[path.length];
        return jsonU640(json, pack(path, ends), ends);
    }

    /** The Java sources below {@code prefix} in {@code jar}, as one source tree. */
    public static int extractJava(int jar, String prefix) {
        return extractJava0(jar, Text.of(prefix));
    }

    /** The jar stored as entry {@code member} of {@code jar}. */
    public static int nestedJar(int jar, String member) {
        return nestedJar0(jar, Text.of(member));
    }

    /**
     * Deobfuscate {@code jar} with {@code mappings}, read as ProGuard-style text.
     *
     * <p>The shorthand for what {@link #remapJarAs} spells with an explicit {@link #proguard()}: a
     * script fetching a game jar and its mappings is asking for exactly this.
     */
    public static int remapJar(int jar, int mappings) {
        return remapJar0(jar, mappings, 0, Text.of(""), Text.of(""));
    }

    /**
     * Deobfuscate {@code jar} with {@code mappings}, read through {@code format}.
     *
     * <p>The direction is always deobfuscating, as in the two-argument form — the reobfuscating
     * direction is a {@code [build] remap}'s concern, not a task node's.
     */
    public static int remapJarAs(int jar, int mappings, MappingFormat format) {
        return remapJar0(jar, mappings, format.kind, Text.of(format.from), Text.of(format.to));
    }

    /** {@code overlay} merged over {@code base}: entries both name come from {@code overlay}. */
    public static int mergeJars(int base, int overlay) {
        return mergeJars0(base, overlay);
    }

    /** Java sources decompiled from {@code jar}, under {@code prefix}. */
    public static int decompileJava(int jar, String prefix) {
        return decompileJava0(jar, Text.of(prefix));
    }

    /** The ProGuard-style grammar, which names no namespaces of its own. */
    public static MappingFormat proguard() {
        return new MappingFormat(0, "", "");
    }

    /**
     * The tiny v2 grammar, read through one pair of its namespaces.
     *
     * <p>{@code from} and {@code to} have to be two different non-empty names: naming one namespace
     * twice renames nothing, and the host refuses the pair here rather than letting a remap that
     * cannot translate anything look like a step that worked.
     */
    public static MappingFormat tinyV2(String from, String to) {
        tinyV2Check(Text.of(from), Text.of(to));
        return new MappingFormat(1, from, to);
    }

    /** Add {@code jar} to the classpath everything compiled after the script sees. */
    public static void addClasspath(int jar) {
        addClasspath0(jar);
    }

    /** Add every nested jar inside {@code jar} — not the jar itself — to that classpath. */
    public static void addNestedClasspath(int jar) {
        addNestedClasspath0(jar);
    }

    /**
     * Publish the source tree {@code tree} for the dependency named {@code owner}, replacing the
     * root of the project-relative directory {@code destination}.
     *
     * <p>{@code intent} says what a consumer does with the tree and has no default: {@code
     * "compile"} when a consumer compiles it, {@code "navigation"} when a consumer only reads it
     * and the classpath defines the types. The distinction only becomes visible when the project
     * is a dependency, and nothing in the task graph could infer which was meant — so a script
     * that does not say is a script whose author has not decided.
     */
    public static void publishTree(String owner, int tree, String destination, String intent) {
        publishTree0(Text.of(owner), tree, Text.of(destination), Text.of(intent));
    }

    /**
     * The code units of every value in {@code values}, concatenated, with each value's end offset
     * (exclusive) written into {@code ends}.
     *
     * <p>A native method cannot receive a {@code String[]}: an element is a string object whose
     * layout is the backend's own, and a host reading one would be reading a fact no declaration
     * states. The same text in a flat {@code char[]} plus one offset per value is a shape the host
     * can read element by element, and the two arrays are adjacent arguments of one call.
     */
    private static char[] pack(String[] values, int[] ends) {
        int total = 0;
        int i = 0;
        while (i < values.length) {
            total = total + values[i].length();
            ends[i] = total;
            i = i + 1;
        }
        char[] packed = new char[total];
        int at = 0;
        i = 0;
        while (i < values.length) {
            int end = ends[i];
            int j = 0;
            while (j < end - at) {
                packed[at + j] = values[i].charAt(j);
                j = j + 1;
            }
            at = end;
            i = i + 1;
        }
        return packed;
    }

    private static native int httpsUrl0(char[] value);

    private static native int projectJar0(char[] path);

    private static native int digest0(char[] value, int algorithm);

    private static native int bytes0(long value);

    private static native int fetch0(int url, int digest, int maxBytes, int kind);

    private static native int jsonAt0(int json, char[] path, int[] ends);

    private static native int jsonFindString0(
        int json, char[] path, int[] ends, char[] field, char[] value);

    private static native int jsonUrl0(int json, char[] path, int[] ends);

    private static native int jsonDigest0(int json, char[] path, int[] ends, int algorithm);

    private static native int jsonU640(int json, char[] path, int[] ends);

    private static native int extractJava0(int jar, char[] prefix);

    private static native int nestedJar0(int jar, char[] member);

    private static native void tinyV2Check(char[] from, char[] to);

    private static native int remapJar0(int jar, int mappings, int kind, char[] from, char[] to);

    private static native int mergeJars0(int base, int overlay);

    private static native int decompileJava0(int jar, char[] prefix);

    private static native void addClasspath0(int jar);

    private static native void addNestedClasspath0(int jar);

    private static native void publishTree0(
        char[] owner, int tree, char[] destination, char[] intent);
}
