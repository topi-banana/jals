package java.lang;

/** A failed {@code assert}. */
public class AssertionError extends Error {

    /**
     * {@code "java.lang.AssertionError"}, as a {@code char[]}.
     *
     * <p>The backend refuses a string literal, which is why every constant in this package is
     * spelled this way and wrapped once in a {@code static} field.
     */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'A', 's', 's', 'e', 'r', 't', 'i', 'o',
        'n', 'E', 'r', 'r', 'o', 'r'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public AssertionError() {
        super();
    }

    /**
     * An error whose message is {@code detailMessage} rendered as a string, and whose cause it is
     * when it is a {@link Throwable} — the JDK's one constructor for both, so {@code assert x : e}
     * and {@code new AssertionError("...")} select the same member here as against a real JDK.
     */
    public AssertionError(Object detailMessage) {
        super(
            String.valueOf(detailMessage),
            detailMessage instanceof Throwable ? (Throwable) detailMessage : null);
    }

    public AssertionError(String message, Throwable cause) {
        super(message, cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
