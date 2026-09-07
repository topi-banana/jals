package java.lang;

/** An arithmetic operation with no answer, such as an integer division by zero. */
public class ArithmeticException extends RuntimeException {

    /** {@code "java.lang.ArithmeticException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'A', 'r', 'i', 't', 'h', 'm', 'e', 't',
        'i', 'c', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public ArithmeticException() {
        super();
    }

    /** With {@code message}. */
    public ArithmeticException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
