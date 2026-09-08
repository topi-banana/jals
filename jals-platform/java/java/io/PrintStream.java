package java.io;

/**
 * A text stream the host writes.
 *
 * <p>Two things are buffered here rather than in the host, and each is a decision.
 *
 * <p>The stream buffers <strong>characters</strong> until a flush, because a {@code char} is a
 * UTF-16 code unit and half of a surrogate pair is not text. A host that decoded each unit as it
 * arrived could not join a pair, and would write a replacement character in place of every
 * astral-plane character a program printed.
 *
 * <p>{@code println} flushes and {@code print} does not. That is the difference a program can
 * observe between the two beyond the line break: a partial line stays in this buffer until
 * something ends it, which is what keeps two interleaved streams from splitting each other's lines.
 */
public final class PrintStream implements Closeable {

    private static final int INITIAL_CAPACITY = 128;

    private static final char[] NULL_TEXT = {'n', 'u', 'l', 'l'};

    private final int stream;

    private char[] buffered;

    private int count;

    /** A stream writing to the host channel {@code stream} names. */
    public PrintStream(int stream) {
        this.stream = stream;
        this.buffered = new char[INITIAL_CAPACITY];
        this.count = 0;
    }

    /** Hand {@code count} characters of {@code text} at {@code offset} to the host. */
    private static native void writeUnits(int stream, char[] text, int offset, int count);

    /** Tell the host that everything handed to it so far is a complete piece of text. */
    private static native void flushStream(int stream);

    private void put(char[] chars, int length) {
        if (this.count + length > this.buffered.length) {
            int grown = this.buffered.length * 2;
            while (grown < this.count + length) {
                grown = grown * 2;
            }
            char[] larger = new char[grown];
            for (int i = 0; i < this.count; i++) {
                larger[i] = this.buffered[i];
            }
            this.buffered = larger;
        }
        for (int i = 0; i < length; i++) {
            this.buffered[this.count + i] = chars[i];
        }
        this.count += length;
    }

    private void put(String text) {
        if (text == null) {
            put(NULL_TEXT, NULL_TEXT.length);
            return;
        }
        char[] chars = text.toCharArray();
        put(chars, chars.length);
    }

    /** Write everything buffered, then tell the host the text is complete. */
    public void flush() {
        if (this.count > 0) {
            writeUnits(this.stream, this.buffered, 0, this.count);
            this.count = 0;
        }
        flushStream(this.stream);
    }

    @Override
    public void close() {
        flush();
    }

    /** Write {@code text}, or {@code "null"}. */
    public void print(String text) {
        put(text);
    }

    /** Write {@code value}'s rendering, or {@code "null"}. */
    public void print(Object value) {
        put(String.valueOf(value));
    }

    /** Write {@code chars}. */
    public void print(char[] chars) {
        put(chars, chars.length);
    }

    /** Write one character. */
    public void print(char value) {
        char[] one = {value};
        put(one, 1);
    }

    /** Write {@code "true"} or {@code "false"}. */
    public void print(boolean value) {
        put(Boolean.toString(value));
    }

    /** Write {@code value} in base ten. */
    public void print(int value) {
        put(Integer.toString(value));
    }

    /** Write {@code value} in base ten. */
    public void print(long value) {
        put(Long.toString(value));
    }

    /** Write {@code value} in Java's decimal layout, at {@code float} width. */
    public void print(float value) {
        put(Float.toString(value));
    }

    /** Write {@code value} in Java's decimal layout. */
    public void print(double value) {
        put(Double.toString(value));
    }

    /** End the line, and flush. */
    public void println() {
        print('\n');
        flush();
    }

    /** Write {@code text}, end the line, and flush. */
    public void println(String text) {
        put(text);
        println();
    }

    /** Write {@code value}'s rendering, end the line, and flush. */
    public void println(Object value) {
        put(String.valueOf(value));
        println();
    }

    /** Write {@code chars}, end the line, and flush. */
    public void println(char[] chars) {
        put(chars, chars.length);
        println();
    }

    /** Write one character, end the line, and flush. */
    public void println(char value) {
        print(value);
        println();
    }

    /** Write {@code "true"} or {@code "false"}, end the line, and flush. */
    public void println(boolean value) {
        put(Boolean.toString(value));
        println();
    }

    /** Write {@code value} in base ten, end the line, and flush. */
    public void println(int value) {
        put(Integer.toString(value));
        println();
    }

    /** Write {@code value} in base ten, end the line, and flush. */
    public void println(long value) {
        put(Long.toString(value));
        println();
    }

    /** Write {@code value} at {@code float} width, end the line, and flush. */
    public void println(float value) {
        put(Float.toString(value));
        println();
    }

    /** Write {@code value} in Java's decimal layout, end the line, and flush. */
    public void println(double value) {
        put(Double.toString(value));
        println();
    }
}
