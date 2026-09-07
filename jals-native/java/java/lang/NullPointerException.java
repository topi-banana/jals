package java.lang;

/** A null reference where an object was needed. */
public class NullPointerException extends RuntimeException {

    /** {@code "java.lang.NullPointerException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'N', 'u', 'l', 'l', 'P', 'o', 'i', 'n',
        't', 'e', 'r', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public NullPointerException() {
        super();
    }

    /** With {@code message}. */
    public NullPointerException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
