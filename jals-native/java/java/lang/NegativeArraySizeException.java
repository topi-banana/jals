package java.lang;

/** An array asked for with a negative length. */
public class NegativeArraySizeException extends RuntimeException {

    /**
     * {@code "java.lang.NegativeArraySizeException"}, the name {@link Throwable#toString}
     * reports.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'N', 'e', 'g', 'a', 't', 'i', 'v', 'e',
        'A', 'r', 'r', 'a', 'y', 'S', 'i', 'z', 'e', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public NegativeArraySizeException() {
        super();
    }

    /** With {@code message}. */
    public NegativeArraySizeException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
