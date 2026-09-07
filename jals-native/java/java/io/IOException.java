package java.io;

/**
 * A failure of an input or output operation.
 *
 * <p>Declared in {@code java.io} because that is where a project's {@code throws} clause names it.
 * Nothing in this package raises one — a wasm module's only output is a host function that cannot
 * fail — and it is here so the checked-exception rule classifies a project's own I/O the way a JVM
 * would.
 */
public class IOException extends Exception {

    /** {@code "java.io.IOException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'i', 'o', '.', 'I', 'O', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o',
        'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public IOException() {
        super();
    }

    /** With {@code message}. */
    public IOException(String message) {
        super(message);
    }

    /** With {@code message}, raised in response to {@code cause}. */
    public IOException(String message, Throwable cause) {
        super(message, cause);
    }

    /** Raised in response to {@code cause}. */
    public IOException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
