package java.lang;

/** {@code clone()} on a type that does not support it. */
public class CloneNotSupportedException extends Exception {

    /**
     * {@code "java.lang.CloneNotSupportedException"}, the name {@link Throwable#toString}
     * reports.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'C', 'l', 'o', 'n', 'e', 'N', 'o', 't',
        'S', 'u', 'p', 'p', 'o', 'r', 't', 'e', 'd', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public CloneNotSupportedException() {
        super();
    }

    /** With {@code message}. */
    public CloneNotSupportedException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
