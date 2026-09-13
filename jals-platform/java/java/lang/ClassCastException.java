package java.lang;

/** A cast to a type the value is not. */
public class ClassCastException extends RuntimeException {

    /**
     * {@code "java.lang.ClassCastException"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'C', 'l', 'a', 's', 's', 'C', 'a', 's',
        't', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public ClassCastException() {
        super();
    }

    public ClassCastException(String message) {
        super(message);
    }

    public ClassCastException(String message, Throwable cause) {
        super(message, cause);
    }

    public ClassCastException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
