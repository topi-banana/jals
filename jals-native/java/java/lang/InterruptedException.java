package java.lang;

/**
 * Declared because a method written against the JDK declares it.
 *
 * <p>Nothing on this target raises one: there is no second thread to interrupt this one. It is
 * here so a {@code throws InterruptedException} clause and the {@code catch} that answers it both
 * resolve.
 */
public class InterruptedException extends Exception {

    /** {@code "java.lang.InterruptedException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'I', 'n', 't', 'e', 'r', 'r', 'u', 'p',
        't', 'e', 'd', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public InterruptedException() {
        super();
    }

    /** With {@code message}. */
    public InterruptedException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
