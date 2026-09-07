package java.lang;

import java.io.PrintStream;

/**
 * The two output streams and the clock, plus array copying.
 *
 * <h2>{@code arraycopy} is a set of overloads, not one {@code Object} method</h2>
 *
 * <p>The JDK's {@code arraycopy} takes two {@code Object}s and works out at run time what kind of
 * array each is. It can, because a JVM carries the element type on the object. This target does
 * not: an {@code Object} is wasm's {@code anyref}, an array of {@code char} and an array of
 * {@code int} are two unrelated types, and there is no reflective step that would tell them apart.
 * So the copy is declared once per element type. A call written against the JDK —
 * {@code System.arraycopy(source, 0, destination, 0, n)} — picks the overload for the arrays it
 * was given and compiles unchanged; only a call whose arrays are *typed* {@code Object} does not,
 * and that call has no meaning here either way.
 *
 * <h2>There is no {@code exit}</h2>
 *
 * <p>A wasm module does not run: it is called. An export returns and the call is over, so there is
 * no process to end and nothing for a status code to be reported to. A method that trapped instead
 * would make {@code System.exit(0)} a failure.
 */
public final class System {

    /** The stream identifier {@link PrintStream} passes to its host function for standard output. */
    static final int OUT_STREAM = 0;

    /** The stream identifier for standard error. */
    static final int ERR_STREAM = 1;

    /** A line break, the only way this target can spell a constant string. */
    private static final char[] LINE_SEPARATOR_CHARS = {'\n'};

    /** {@link #LINE_SEPARATOR_CHARS}, built once. */
    private static final String LINE_SEPARATOR = new String(LINE_SEPARATOR_CHARS);

    /** Where a program's own output goes. */
    public static final PrintStream out = new PrintStream(OUT_STREAM);

    /** Where a program's diagnostics go. */
    public static final PrintStream err = new PrintStream(ERR_STREAM);

    /** Never called. */
    private System() {
    }

    /**
     * Milliseconds since the epoch, as the host reads its own clock.
     *
     * <p>{@code native} because this crate is portable and has none: a browser tab's clock and a
     * terminal's are different host calls, and neither is reachable from a module.
     */
    public static native long currentTimeMillis();

    /** A monotonic reading in nanoseconds, meaningful only as a difference from another one. */
    public static native long nanoTime();

    /**
     * A line separator.
     *
     * <p>Always {@code "\n"}. A wasm module has no operating system to ask, and a host that writes
     * the text sees exactly the code units the module produced — so choosing anything else would
     * be choosing on the host's behalf from inside the module.
     */
    public static String lineSeparator() {
        return LINE_SEPARATOR;
    }

    /** Copy {@code length} elements from {@code source} at {@code from} to {@code target} at {@code to}. */
    public static void arraycopy(char[] source, int from, char[] target, int to, int length) {
        checkRange(source.length, from, target.length, to, length);
        if (source == target && from < to) {
            int at = length - 1;
            while (at >= 0) {
                target[to + at] = source[from + at];
                at = at - 1;
            }
            return;
        }
        int at = 0;
        while (at < length) {
            target[to + at] = source[from + at];
            at = at + 1;
        }
    }

    /** Copy {@code length} elements from {@code source} at {@code from} to {@code target} at {@code to}. */
    public static void arraycopy(int[] source, int from, int[] target, int to, int length) {
        checkRange(source.length, from, target.length, to, length);
        if (source == target && from < to) {
            int at = length - 1;
            while (at >= 0) {
                target[to + at] = source[from + at];
                at = at - 1;
            }
            return;
        }
        int at = 0;
        while (at < length) {
            target[to + at] = source[from + at];
            at = at + 1;
        }
    }

    /** Copy {@code length} elements from {@code source} at {@code from} to {@code target} at {@code to}. */
    public static void arraycopy(long[] source, int from, long[] target, int to, int length) {
        checkRange(source.length, from, target.length, to, length);
        if (source == target && from < to) {
            int at = length - 1;
            while (at >= 0) {
                target[to + at] = source[from + at];
                at = at - 1;
            }
            return;
        }
        int at = 0;
        while (at < length) {
            target[to + at] = source[from + at];
            at = at + 1;
        }
    }

    /** Copy {@code length} elements from {@code source} at {@code from} to {@code target} at {@code to}. */
    public static void arraycopy(byte[] source, int from, byte[] target, int to, int length) {
        checkRange(source.length, from, target.length, to, length);
        if (source == target && from < to) {
            int at = length - 1;
            while (at >= 0) {
                target[to + at] = source[from + at];
                at = at - 1;
            }
            return;
        }
        int at = 0;
        while (at < length) {
            target[to + at] = source[from + at];
            at = at + 1;
        }
    }

    /** Copy {@code length} elements from {@code source} at {@code from} to {@code target} at {@code to}. */
    public static void arraycopy(double[] source, int from, double[] target, int to, int length) {
        checkRange(source.length, from, target.length, to, length);
        if (source == target && from < to) {
            int at = length - 1;
            while (at >= 0) {
                target[to + at] = source[from + at];
                at = at - 1;
            }
            return;
        }
        int at = 0;
        while (at < length) {
            target[to + at] = source[from + at];
            at = at + 1;
        }
    }

    /** The one bounds check every {@code arraycopy} overload goes through. */
    private static void checkRange(int sourceLength, int from, int targetLength, int to, int length) {
        if (length < 0
                || from < 0
                || to < 0
                || from > sourceLength - length
                || to > targetLength - length) {
            throw new ArrayIndexOutOfBoundsException();
        }
    }
}
