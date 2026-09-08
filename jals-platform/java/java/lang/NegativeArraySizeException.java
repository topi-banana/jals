package java.lang;

/** An array created with a negative length. */
public class NegativeArraySizeException extends RuntimeException {

    /**
     * {@code "java.lang.NegativeArraySizeException"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'N', 'e', 'g', 'a', 't', 'i', 'v', 'e',
        'A', 'r', 'r', 'a', 'y', 'S', 'i', 'z', 'e', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public NegativeArraySizeException() {
        super();
    }

    public NegativeArraySizeException(String message) {
        super(message);
    }

    public NegativeArraySizeException(String message, Throwable cause) {
        super(message, cause);
    }

    public NegativeArraySizeException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
