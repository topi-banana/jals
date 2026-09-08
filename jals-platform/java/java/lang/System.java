package java.lang;

import java.io.PrintStream;

/**
 * The two standard streams, the clock, and array copying.
 *
 * <p>There is no {@code System.exit}. A wasm module does not <em>run</em>: it is called, and it
 * returns to whoever called it. A method that never returned would be a trap, which is not what
 * {@code exit(0)} means.
 *
 * <p>{@link #arraycopy} is five typed overloads rather than one taking {@code Object}. An
 * {@code Object} here is the engine's own {@code anyref} and there is no reflective step that would
 * tell one array type from another, so the choice is made where the types are still known: at the
 * call site, by overload selection.
 */
public final class System {

    /** The stream identifier the host binds to standard output. */
    static final int OUT_STREAM = 0;

    /** The stream identifier the host binds to standard error. */
    static final int ERR_STREAM = 1;

    /** A line break — the only constant string this class needs. */
    private static final char[] LINE_SEPARATOR_CHARS = {'\n'};

    private static final String LINE_SEPARATOR =
            new String(LINE_SEPARATOR_CHARS, 0, LINE_SEPARATOR_CHARS.length);

    /** The standard output stream. */
    public static final PrintStream out = new PrintStream(OUT_STREAM);

    /** The standard error stream. */
    public static final PrintStream err = new PrintStream(ERR_STREAM);

    private System() {
    }

    /** Milliseconds since the Unix epoch, as the host reads its clock. */
    public static native long currentTimeMillis();

    /**
     * A monotonic reading in nanoseconds.
     *
     * <p>Meaningful only as a difference from another reading: the origin is whenever the host
     * started counting, and a host with no clock at all answers zero every time — which is a real
     * host, not a broken one. A language server instantiates nothing and has no clock to offer.
     */
    public static native long nanoTime();

    /** The line separator, which is always {@code "\n"} on this target. */
    public static String lineSeparator() {
        return LINE_SEPARATOR;
    }

    /** Copy {@code length} elements out of {@code source} at {@code from}, into {@code target}. */
    public static void arraycopy(char[] source, int from, char[] target, int to, int length) {
        checkCopy(source.length, from, target.length, to, length);
        if (source == target && from < to) {
            for (int i = length - 1; i >= 0; i--) {
                target[to + i] = source[from + i];
            }
            return;
        }
        for (int i = 0; i < length; i++) {
            target[to + i] = source[from + i];
        }
    }

    /** Copy {@code length} elements between {@code int} arrays. */
    public static void arraycopy(int[] source, int from, int[] target, int to, int length) {
        checkCopy(source.length, from, target.length, to, length);
        if (source == target && from < to) {
            for (int i = length - 1; i >= 0; i--) {
                target[to + i] = source[from + i];
            }
            return;
        }
        for (int i = 0; i < length; i++) {
            target[to + i] = source[from + i];
        }
    }

    /** Copy {@code length} elements between {@code long} arrays. */
    public static void arraycopy(long[] source, int from, long[] target, int to, int length) {
        checkCopy(source.length, from, target.length, to, length);
        if (source == target && from < to) {
            for (int i = length - 1; i >= 0; i--) {
                target[to + i] = source[from + i];
            }
            return;
        }
        for (int i = 0; i < length; i++) {
            target[to + i] = source[from + i];
        }
    }

    /** Copy {@code length} elements between {@code byte} arrays. */
    public static void arraycopy(byte[] source, int from, byte[] target, int to, int length) {
        checkCopy(source.length, from, target.length, to, length);
        if (source == target && from < to) {
            for (int i = length - 1; i >= 0; i--) {
                target[to + i] = source[from + i];
            }
            return;
        }
        for (int i = 0; i < length; i++) {
            target[to + i] = source[from + i];
        }
    }

    /** Copy {@code length} elements between {@code double} arrays. */
    public static void arraycopy(double[] source, int from, double[] target, int to, int length) {
        checkCopy(source.length, from, target.length, to, length);
        if (source == target && from < to) {
            for (int i = length - 1; i >= 0; i--) {
                target[to + i] = source[from + i];
            }
            return;
        }
        for (int i = 0; i < length; i++) {
            target[to + i] = source[from + i];
        }
    }

    /**
     * The bounds every {@code arraycopy} overload checks, written once.
     *
     * <p>Lengths rather than arrays, because the five overloads have five unrelated parameter types
     * and no supertype this package can name to join them.
     */
    private static void checkCopy(
            int sourceLength, int from, int targetLength, int to, int length) {
        if (length < 0 || from < 0 || to < 0
                || from + length > sourceLength
                || to + length > targetLength) {
            throw new ArrayIndexOutOfBoundsException();
        }
    }
}
