package java.lang;

/** An argument a method will not accept. */
public class IllegalArgumentException extends RuntimeException {

    /**
     * {@code "java.lang.IllegalArgumentException"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'I', 'l', 'l', 'e', 'g', 'a', 'l', 'A',
        'r', 'g', 'u', 'm', 'e', 'n', 't', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public IllegalArgumentException() {
        super();
    }

    public IllegalArgumentException(String message) {
        super(message);
    }

    public IllegalArgumentException(String message, Throwable cause) {
        super(message, cause);
    }

    public IllegalArgumentException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
