package java.lang;

/**
 * The common supertype of the reflective failures, and the one a single {@code catch} binds
 * through.
 *
 * <p>Nothing on this target raises one — there is no reflection here — and the hierarchy is
 * declared anyway, because a file that calls {@code Class.forName} must name these in a
 * {@code catch} to compile at all.
 */
public class ReflectiveOperationException extends Exception {

    /**
     * {@code "java.lang.ReflectiveOperationException"}, the name {@link Throwable#toString}
     * reports.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'R', 'e', 'f', 'l', 'e', 'c', 't', 'i',
        'v', 'e', 'O', 'p', 'e', 'r', 'a', 't', 'i', 'o', 'n', 'E', 'x', 'c', 'e', 'p', 't', 'i',
        'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public ReflectiveOperationException() {
        super();
    }

    /** With {@code message}. */
    public ReflectiveOperationException(String message) {
        super(message);
    }

    /** With {@code message}, raised in response to {@code cause}. */
    public ReflectiveOperationException(String message, Throwable cause) {
        super(message, cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
