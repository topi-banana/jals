package java.lang;

/**
 * A failure a program is expected to handle.
 *
 * <p>Checked, unless it is a {@link RuntimeException} — which is a rule about this class's
 * subclasses rather than about this class, and is why the two are separate declarations rather
 * than one with a flag.
 */
public class Exception extends Throwable {

    /** {@code "java.lang.Exception"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o',
        'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public Exception() {
        super();
    }

    /** With {@code message}. */
    public Exception(String message) {
        super(message);
    }

    /** With {@code message}, raised in response to {@code cause}. */
    public Exception(String message, Throwable cause) {
        super(message, cause);
    }

    /** Raised in response to {@code cause}. */
    public Exception(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
