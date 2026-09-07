package java.io;

/**
 * Where a module's text goes.
 *
 * <p>The two instances that exist are {@code System.out} and {@code System.err}, and each is a
 * stream identifier and a buffer. The identifier is what the host function reads to decide which
 * of its own sinks to write to — {@code jals} sends one to stdout and the other to stderr, the
 * browser playground sends both to the Run pane. Everything else here is Java.
 *
 * <h2>Buffered until a line ends</h2>
 *
 * <p>A {@code char} is half of a surrogate pair, so a host that decoded each code unit as it
 * arrived could not join one. The buffer is what makes the pair reachable; it is not a speed-up.
 * {@link #println} flushes and {@link #print} does not, which is the JDK's behaviour for a
 * line-buffered stream and the reason a program that only ever {@code print}s must
 * {@link #flush} itself.
 */
public final class PrintStream implements Closeable {

    /** What the buffer starts at, and what it never shrinks below. */
    private static final int INITIAL_CAPACITY = 128;

    /** Which of the host's sinks this stream writes to. */
    private final int stream;

    /** Code units written since the last flush. */
    private char[] buffered;

    /** How many of {@link #buffered} are live. */
    private int count;

    /**
     * The stream writing to the host sink named by {@code stream}.
     *
     * <p>The JDK's constructor takes an {@code OutputStream} and this target has none, so what
     * identifies a sink here is a number the host understands: {@code System.ERR_STREAM} is the
     * host's diagnostic sink and every other value is its output sink. Public because
     * {@code java.lang.System} builds the two shipped instances and is not in this package —
     * there is no {@code module-info} here to open one package to another.
     */
    public PrintStream(int stream) {
        this.stream = stream;
        this.buffered = new char[INITIAL_CAPACITY];
        this.count = 0;
    }

    /** Append {@code text}, or {@code "null"}. */
    public void print(String text) {
        write(String.valueOf(text));
    }

    /** Append every code unit of {@code text}. */
    public void print(char[] text) {
        int at = 0;
        while (at < text.length) {
            append(text[at]);
            at = at + 1;
        }
    }

    /** Append one code unit. */
    public void print(char value) {
        append(value);
    }

    /** Append {@code "true"} or {@code "false"}. */
    public void print(boolean value) {
        write(Boolean.toString(value));
    }

    /** Append {@code value} in decimal. */
    public void print(int value) {
        write(Integer.toString(value));
    }

    /** Append {@code value} in decimal. */
    public void print(long value) {
        write(Long.toString(value));
    }

    /** Append {@code value}, rendered. */
    public void print(float value) {
        write(Float.toString(value));
    }

    /** Append {@code value}, rendered. */
    public void print(double value) {
        write(Double.toString(value));
    }

    /** Append the rendering of a reference, through {@link String#valueOf(Object)}. */
    public void print(Object value) {
        write(String.valueOf(value));
    }

    /** End the line, and flush. */
    public void println() {
        append('\n');
        flush();
    }

    /** Append {@code text}, end the line, and flush. */
    public void println(String text) {
        print(text);
        println();
    }

    /** Append every code unit of {@code text}, end the line, and flush. */
    public void println(char[] text) {
        print(text);
        println();
    }

    /** Append one code unit, end the line, and flush. */
    public void println(char value) {
        print(value);
        println();
    }

    /** Append {@code "true"} or {@code "false"}, end the line, and flush. */
    public void println(boolean value) {
        print(value);
        println();
    }

    /** Append {@code value} in decimal, end the line, and flush. */
    public void println(int value) {
        print(value);
        println();
    }

    /** Append {@code value} in decimal, end the line, and flush. */
    public void println(long value) {
        print(value);
        println();
    }

    /** Append {@code value}, end the line, and flush. */
    public void println(float value) {
        print(value);
        println();
    }

    /** Append {@code value}, end the line, and flush. */
    public void println(double value) {
        print(value);
        println();
    }

    /** Append the rendering of a reference, end the line, and flush. */
    public void println(Object value) {
        print(value);
        println();
    }

    /** Hand everything buffered to the host, and empty the buffer. */
    public void flush() {
        if (this.count > 0) {
            writeUnits(this.stream, this.buffered, 0, this.count);
            this.count = 0;
        }
        flushStream(this.stream);
    }

    /** {@link #flush}. There is nothing to release: the sink belongs to the host. */
    @Override
    public void close() {
        flush();
    }

    /** Append every code unit of {@code text}. */
    private void write(String text) {
        int at = 0;
        int end = text.length();
        while (at < end) {
            append(text.charAt(at));
            at = at + 1;
        }
    }

    /** Append one code unit, growing the buffer when it is full. */
    private void append(char unit) {
        if (this.count == this.buffered.length) {
            char[] grown = new char[this.buffered.length * 2];
            int at = 0;
            while (at < this.count) {
                grown[at] = this.buffered[at];
                at = at + 1;
            }
            this.buffered = grown;
        }
        this.buffered[this.count] = unit;
        this.count = this.count + 1;
    }

    /**
     * Hand {@code count} code units of {@code text} from {@code offset} to the host sink named by
     * {@code stream}.
     *
     * <p>The one host function this class has, and the only thing here that is not Java. Its
     * arguments are a number and an array because those are the two shapes that cross the
     * boundary: the host reads the array element by element and decodes the pair itself.
     */
    private static native void writeUnits(int stream, char[] text, int offset, int count);

    /** Tell the host that a run of writes has ended. */
    private static native void flushStream(int stream);
}
