package java.io;

/** An {@link IOException} raised where a checked one cannot be declared. */
public class UncheckedIOException extends RuntimeException {

    /** {@code "java.io.UncheckedIOException"}, as a {@code char[]}. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'i', 'o', '.', 'U', 'n', 'c', 'h', 'e', 'c', 'k', 'e', 'd', 'I',
        'O', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public UncheckedIOException(IOException cause) {
        super(cause);
    }

    public UncheckedIOException(String message, IOException cause) {
        super(message, cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
