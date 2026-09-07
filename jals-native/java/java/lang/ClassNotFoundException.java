package java.lang;

/** A class named at run time that no module declares. */
public class ClassNotFoundException extends ReflectiveOperationException {

    /** {@code "java.lang.ClassNotFoundException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'C', 'l', 'a', 's', 's', 'N', 'o', 't',
        'F', 'o', 'u', 'n', 'd', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public ClassNotFoundException() {
        super();
    }

    /** With {@code message}. */
    public ClassNotFoundException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
