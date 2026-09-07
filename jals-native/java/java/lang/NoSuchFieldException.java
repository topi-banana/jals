package java.lang;

/** A field a reflective lookup did not find. */
public class NoSuchFieldException extends ReflectiveOperationException {

    /** {@code "java.lang.NoSuchFieldException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'N', 'o', 'S', 'u', 'c', 'h', 'F', 'i',
        'e', 'l', 'd', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public NoSuchFieldException() {
        super();
    }

    /** With {@code message}. */
    public NoSuchFieldException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
