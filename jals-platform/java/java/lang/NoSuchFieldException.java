package java.lang;

/** A field named at run time that does not exist. */
public class NoSuchFieldException extends ReflectiveOperationException {

    /**
     * {@code "java.lang.NoSuchFieldException"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'N', 'o', 'S', 'u', 'c', 'h', 'F', 'i',
        'e', 'l', 'd', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public NoSuchFieldException() {
        super();
    }

    public NoSuchFieldException(String message) {
        super(message);
    }

    public NoSuchFieldException(String message, Throwable cause) {
        super(message, cause);
    }

    public NoSuchFieldException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
