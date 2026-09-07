package java.lang;

/**
 * A failure a program is not expected to catch.
 *
 * <p>The split from {@link Exception} is the one Java's checked-exception rule reads: an
 * {@code Error} is unchecked, so a method that may raise one declares nothing.
 */
public class Error extends Throwable {

    /** {@code "java.lang.Error"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'E', 'r', 'r', 'o', 'r'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public Error() {
        super();
    }

    /** With {@code message}. */
    public Error(String message) {
        super(message);
    }

    /** With {@code message}, raised in response to {@code cause}. */
    public Error(String message, Throwable cause) {
        super(message, cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
