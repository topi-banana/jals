package java.io;

/** A file that is not there. */
public class FileNotFoundException extends IOException {

    /** {@code "java.io.FileNotFoundException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'i', 'o', '.', 'F', 'i', 'l', 'e', 'N', 'o', 't', 'F', 'o', 'u',
        'n', 'd', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public FileNotFoundException() {
        super();
    }

    /** With {@code message}. */
    public FileNotFoundException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
