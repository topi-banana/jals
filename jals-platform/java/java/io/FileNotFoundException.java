package java.io;

/** A file named for reading or writing that could not be opened. */
public class FileNotFoundException extends IOException {

    /** {@code "java.io.FileNotFoundException"}, as a {@code char[]}. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'i', 'o', '.', 'F', 'i', 'l', 'e', 'N', 'o', 't', 'F', 'o', 'u',
        'n', 'd', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    public FileNotFoundException() {
        super();
    }

    public FileNotFoundException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
