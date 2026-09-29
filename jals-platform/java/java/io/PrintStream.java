package java.io;

/**
 * Where a program's text goes.
 *
 * <p>The JDK's stream is a byte stream with a charset and a buffer in front of it; this one is a
 * console, and the shape is the host's. A wasm embedder can read a Java {@code char[]} element by
 * element, but it cannot read a {@code String} — a string's representation is the backend's own
 * layout, and a native package reading one would be reading a fact no declaration states. So the
 * one {@code native} method here takes a {@code char[]}, and every printing method below is
 * ordinary Java that builds one.
 *
 * <p>{@code print} writes through immediately rather than buffering. That is not the JDK's
 * arrangement — {@code System.out} buffers and flushes — but the sink it writes to is already a
 * console rather than a file descriptor, so the flush the JDK has to perform has happened by the
 * time {@link #flush()} could be called, and it says so. The one consequence worth stating is that
 * a surrogate pair split across two {@code print(char)} calls arrives as two code units, exactly
 * as it would through a byte stream.
 *
 * <p>What is not here yet: the {@code float} and {@code double} overloads, which need a decimal
 * renderer the platform has not chosen, and the two {@code Object} ones, which would dispatch
 * {@code toString} and inherit the stub {@code Object}'s missing default. Those calls are refused
 * by name until then rather than answered with something almost right.
 */
public class PrintStream extends Object {

    /** A stream whose text goes to the host's console. */
    public PrintStream() {}

    /** Write {@code s}, without a line break. */
    public void print(String s) {
        char[] chars = new char[s.length()];
        int i = 0;
        while (i < chars.length) {
            chars[i] = s.charAt(i);
            i = i + 1;
        }
        writeChars(chars, 0, chars.length);
    }

    /** Write {@code s}, then a line break. */
    public void println(String s) {
        this.print(s);
        this.println();
    }

    /** Write a line break. */
    public void println() {
        char[] line = new char[1];
        line[0] = '\n';
        writeChars(line, 0, 1);
    }

    /** Write {@code value} in decimal, without a line break. */
    public void print(int value) {
        this.print(Integer.valueOf(value).toString());
    }

    /** Write {@code value} in decimal, then a line break. */
    public void println(int value) {
        this.print(value);
        this.println();
    }

    /** Write {@code value} in decimal, without a line break. */
    public void print(long value) {
        this.print(Long.valueOf(value).toString());
    }

    /** Write {@code value} in decimal, then a line break. */
    public void println(long value) {
        this.print(value);
        this.println();
    }

    /** Write {@code true} or {@code false}, without a line break. */
    public void print(boolean value) {
        this.print(Boolean.valueOf(value).toString());
    }

    /** Write {@code true} or {@code false}, then a line break. */
    public void println(boolean value) {
        this.print(value);
        this.println();
    }

    /** Write one UTF-16 code unit, without a line break. */
    public void print(char value) {
        char[] one = new char[1];
        one[0] = value;
        writeChars(one, 0, 1);
    }

    /** Write one UTF-16 code unit, then a line break. */
    public void println(char value) {
        this.print(value);
        this.println();
    }

    /**
     * Nothing to do.
     *
     * <p>Every {@code print} above has already written through to the host's sink, so the flush
     * the JDK has to perform — moving buffered bytes onto the stream — has already happened by the
     * time this could be called. The method exists because a program that calls
     * {@code System.out.flush()} should mean what it says rather than fail to compile.
     */
    public void flush() {}

    /** Write {@code count} code units of {@code chars} starting at {@code offset} to the console. */
    private static native void writeChars(char[] chars, int offset, int count);
}
