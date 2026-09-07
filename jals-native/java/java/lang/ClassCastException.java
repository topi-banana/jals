package java.lang;

/** A cast to a type the value is not. */
public class ClassCastException extends RuntimeException {

    /** {@code "java.lang.ClassCastException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'C', 'l', 'a', 's', 's', 'C', 'a', 's',
        't', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public ClassCastException() {
        super();
    }

    /** With {@code message}. */
    public ClassCastException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
