package java.lang;

/** An index outside the range a sequence defines. */
public class IndexOutOfBoundsException extends RuntimeException {

    /**
     * {@code "java.lang.IndexOutOfBoundsException"}, the name {@link Throwable#toString}
     * reports.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'I', 'n', 'd', 'e', 'x', 'O', 'u', 't',
        'O', 'f', 'B', 'o', 'u', 'n', 'd', 's', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public IndexOutOfBoundsException() {
        super();
    }

    /** With {@code message}. */
    public IndexOutOfBoundsException(String message) {
        super(message);
    }

    /** With the offending index as the message. */
    public IndexOutOfBoundsException(int index) {
        super(Integer.toString(index));
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
