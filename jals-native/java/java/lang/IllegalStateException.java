package java.lang;

/** A call made when the receiver was not in a state that allows it. */
public class IllegalStateException extends RuntimeException {

    /** {@code "java.lang.IllegalStateException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'I', 'l', 'l', 'e', 'g', 'a', 'l', 'S',
        't', 'a', 't', 'e', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public IllegalStateException() {
        super();
    }

    /** With {@code message}. */
    public IllegalStateException(String message) {
        super(message);
    }

    /** With {@code message}, raised in response to {@code cause}. */
    public IllegalStateException(String message, Throwable cause) {
        super(message, cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
