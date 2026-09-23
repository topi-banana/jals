package java.lang;

/** An operation the receiver does not support. */
public class UnsupportedOperationException extends RuntimeException {

    /**
     * {@code "java.lang.UnsupportedOperationException"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'U', 'n', 's', 'u', 'p', 'p', 'o', 'r',
        't', 'e', 'd', 'O', 'p', 'e', 'r', 'a', 't', 'i', 'o', 'n', 'E', 'x', 'c', 'e', 'p', 't',
        'i', 'o', 'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public UnsupportedOperationException() {
        super();
    }

    public UnsupportedOperationException(String message) {
        super(message);
    }

    public UnsupportedOperationException(String message, Throwable cause) {
        super(message, cause);
    }

    public UnsupportedOperationException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
