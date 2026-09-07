package java.lang;

/** An operation a type declares but does not implement. */
public class UnsupportedOperationException extends RuntimeException {

    /**
     * {@code "java.lang.UnsupportedOperationException"}, the name {@link Throwable#toString}
     * reports.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'U', 'n', 's', 'u', 'p', 'p', 'o', 'r',
        't', 'e', 'd', 'O', 'p', 'e', 'r', 'a', 't', 'i', 'o', 'n', 'E', 'x', 'c', 'e', 'p', 't',
        'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public UnsupportedOperationException() {
        super();
    }

    /** With {@code message}. */
    public UnsupportedOperationException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
