package java.lang;

/** An index outside a {@link String} or a {@link StringBuilder}. */
public class StringIndexOutOfBoundsException extends IndexOutOfBoundsException {

    /**
     * {@code "java.lang.StringIndexOutOfBoundsException"}, the name {@link Throwable#toString}
     * reports.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'S', 't', 'r', 'i', 'n', 'g', 'I', 'n',
        'd', 'e', 'x', 'O', 'u', 't', 'O', 'f', 'B', 'o', 'u', 'n', 'd', 's', 'E', 'x', 'c', 'e',
        'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public StringIndexOutOfBoundsException() {
        super();
    }

    /** With {@code message}. */
    public StringIndexOutOfBoundsException(String message) {
        super(message);
    }

    /** With the offending index as the message. */
    public StringIndexOutOfBoundsException(int index) {
        super(Integer.toString(index));
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
