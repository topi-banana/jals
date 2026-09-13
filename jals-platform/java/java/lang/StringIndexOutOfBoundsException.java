package java.lang;

/** A string index outside {@code [0, length)}. */
public class StringIndexOutOfBoundsException extends IndexOutOfBoundsException {

    /**
     * {@code "java.lang.StringIndexOutOfBoundsException"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'S', 't', 'r', 'i', 'n', 'g', 'I', 'n',
        'd', 'e', 'x', 'O', 'u', 't', 'O', 'f', 'B', 'o', 'u', 'n', 'd', 's', 'E', 'x', 'c', 'e',
        'p', 't', 'i', 'o', 'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public StringIndexOutOfBoundsException() {
        super();
    }

    public StringIndexOutOfBoundsException(String message) {
        super(message);
    }

    public StringIndexOutOfBoundsException(String message, Throwable cause) {
        super(message, cause);
    }

    public StringIndexOutOfBoundsException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
