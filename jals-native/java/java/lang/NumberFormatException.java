package java.lang;

/** Text that does not spell a number of the type that was asked for. */
public class NumberFormatException extends IllegalArgumentException {

    /** {@code "java.lang.NumberFormatException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'N', 'u', 'm', 'b', 'e', 'r', 'F', 'o',
        'r', 'm', 'a', 't', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public NumberFormatException() {
        super();
    }

    /** With {@code message}. */
    public NumberFormatException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
