package java.lang;

/** Text that does not spell a number of the requested type. */
public class NumberFormatException extends IllegalArgumentException {

    /**
     * {@code "java.lang.NumberFormatException"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'N', 'u', 'm', 'b', 'e', 'r', 'F', 'o',
        'r', 'm', 'a', 't', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public NumberFormatException() {
        super();
    }

    public NumberFormatException(String message) {
        super(message);
    }

    public NumberFormatException(String message, Throwable cause) {
        super(message, cause);
    }

    public NumberFormatException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
