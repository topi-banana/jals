package java.io;

/** An {@link IOException} raised where a checked one could not be declared. */
public class UncheckedIOException extends RuntimeException {

    /** {@code "java.io.UncheckedIOException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'i', 'o', '.', 'U', 'n', 'c', 'h', 'e', 'c', 'k', 'e', 'd', 'I',
        'O', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With {@code message}. */
    public UncheckedIOException(String message) {
        super(message);
    }

    /** Wrapping {@code cause}, taking its rendering as the message. */
    public UncheckedIOException(IOException cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
