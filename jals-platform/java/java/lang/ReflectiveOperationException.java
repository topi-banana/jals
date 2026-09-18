package java.lang;

/**
 * The root of the reflection failures.
 *
 * <p>Reflection itself is absent here; the hierarchy is not, so a {@code catch} written against
 * it still compiles.
 */
public class ReflectiveOperationException extends Exception {

    /**
     * {@code "java.lang.ReflectiveOperationException"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'R', 'e', 'f', 'l', 'e', 'c', 't', 'i',
        'v', 'e', 'O', 'p', 'e', 'r', 'a', 't', 'i', 'o', 'n', 'E', 'x', 'c', 'e', 'p', 't', 'i',
        'o', 'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public ReflectiveOperationException() {
        super();
    }

    public ReflectiveOperationException(String message) {
        super(message);
    }

    public ReflectiveOperationException(String message, Throwable cause) {
        super(message, cause);
    }

    public ReflectiveOperationException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
