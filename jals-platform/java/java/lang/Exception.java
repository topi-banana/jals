package java.lang;

/** A condition a reasonable program might want to catch. */
public class Exception extends Throwable {

    /**
     * {@code "java.lang.Exception"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o',
        'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public Exception() {
        super();
    }

    public Exception(String message) {
        super(message);
    }

    public Exception(String message, Throwable cause) {
        super(message, cause);
    }

    public Exception(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
