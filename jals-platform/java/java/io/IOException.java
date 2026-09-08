package java.io;

/** A failed or interrupted I/O operation — the root of the checked I/O failures. */
public class IOException extends Exception {

    /** {@code "java.io.IOException"}, as a {@code char[]}. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'i', 'o', '.', 'I', 'O', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o',
        'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public IOException() {
        super();
    }

    public IOException(String message) {
        super(message);
    }

    public IOException(String message, Throwable cause) {
        super(message, cause);
    }

    public IOException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
