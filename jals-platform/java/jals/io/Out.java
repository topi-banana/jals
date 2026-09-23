package jals.io;

/**
 * Text out of a WebAssembly module, one UTF-16 code unit at a time.
 *
 * <p>There is no {@code String} on this target — a wasm host has no {@code java.base} — so the
 * currency here is {@code char[]}, which is a wasm array the embedder's collector owns and the
 * host can read element by element. Everything below the three {@code native} methods is ordinary
 * Java compiled into the same module: the Rust half of this package is three functions, and the
 * library is Java.
 *
 * <p>Writing is buffered on the host side and reaches the console on {@link #flush()}. That is not
 * an optimisation: a code unit is half of a surrogate pair, and a host that wrote each one as it
 * arrived could not join them.
 */
public final class Out {

    /** Append one UTF-16 code unit to the host's buffer. */
    public static native void writeChar(int codeUnit);

    /** Append {@code count} code units of {@code text}, starting at {@code offset}. */
    public static native void writeChars(char[] text, int offset, int count);

    /** Write everything buffered so far, and empty the buffer. */
    public static native void flush();

    /** Append every code unit of {@code text}. */
    public static void print(char[] text) {
        writeChars(text, 0, text.length);
    }

    /** Append every code unit of {@code text}, then a line break, then flush. */
    public static void println(char[] text) {
        writeChars(text, 0, text.length);
        writeChar('\n');
        flush();
    }

    /** Append a line break, then flush. */
    public static void println() {
        writeChar('\n');
        flush();
    }

    /**
     * Append {@code value} in decimal.
     *
     * <p>Written out rather than delegated, because the delegate would be {@code Integer.toString}
     * and that returns a {@code String}. The digits are built into a {@code char[]} big enough for
     * {@code -2147483648} and written from the tail — which is also why the digits are taken from
     * the *negative* side: {@code -Integer.MIN_VALUE} does not fit an {@code int}, so negating
     * first would be wrong for exactly one input.
     */
    public static void printInt(int value) {
        if (value == 0) {
            writeChar('0');
            return;
        }
        char[] digits = new char[11];
        int at = digits.length;
        boolean negative = value < 0;
        int rest = value;
        if (!negative) {
            rest = -value;
        }
        while (rest != 0) {
            at = at - 1;
            digits[at] = (char) ('0' - (rest % 10));
            rest = rest / 10;
        }
        if (negative) {
            at = at - 1;
            digits[at] = '-';
        }
        writeChars(digits, at, digits.length - at);
    }

    /** Append {@code value} in decimal, then a line break, then flush. */
    public static void printlnInt(int value) {
        printInt(value);
        println();
    }
}
