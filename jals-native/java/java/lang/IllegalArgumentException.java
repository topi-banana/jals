package java.lang;

/** An argument a method was not written to accept. */
public class IllegalArgumentException extends RuntimeException {

    /**
     * {@code "java.lang.IllegalArgumentException"}, the name {@link Throwable#toString}
     * reports.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'I', 'l', 'l', 'e', 'g', 'a', 'l', 'A',
        'r', 'g', 'u', 'm', 'e', 'n', 't', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public IllegalArgumentException() {
        super();
    }

    /** With {@code message}. */
    public IllegalArgumentException(String message) {
        super(message);
    }

    /** With {@code message}, raised in response to {@code cause}. */
    public IllegalArgumentException(String message, Throwable cause) {
        super(message, cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
