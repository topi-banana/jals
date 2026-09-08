package java.lang;

/** A failed {@code assert}. */
public class AssertionError extends Error {

    /**
     * {@code "java.lang.AssertionError"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'A', 's', 's', 'e', 'r', 't', 'i', 'o',
        'n', 'E', 'r', 'r', 'o', 'r'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public AssertionError() {
        super();
    }

    public AssertionError(String message) {
        super(message);
    }

    public AssertionError(String message, Throwable cause) {
        super(message, cause);
    }

    public AssertionError(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
