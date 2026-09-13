package java.lang;

/** A clone of a type that does not support one. */
public class CloneNotSupportedException extends Exception {

    /**
     * {@code "java.lang.CloneNotSupportedException"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'C', 'l', 'o', 'n', 'e', 'N', 'o', 't',
        'S', 'u', 'p', 'p', 'o', 'r', 't', 'e', 'd', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public CloneNotSupportedException() {
        super();
    }

    public CloneNotSupportedException(String message) {
        super(message);
    }

    public CloneNotSupportedException(String message, Throwable cause) {
        super(message, cause);
    }

    public CloneNotSupportedException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
