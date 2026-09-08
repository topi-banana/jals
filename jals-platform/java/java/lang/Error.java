package java.lang;

/** A serious problem a reasonable program should not try to catch. */
public class Error extends Throwable {

    /**
     * {@code "java.lang.Error"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'E', 'r', 'r', 'o', 'r'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public Error() {
        super();
    }

    public Error(String message) {
        super(message);
    }

    public Error(String message, Throwable cause) {
        super(message, cause);
    }

    public Error(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
