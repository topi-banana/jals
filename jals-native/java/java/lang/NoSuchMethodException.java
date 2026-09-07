package java.lang;

/** A method a reflective lookup did not find. */
public class NoSuchMethodException extends ReflectiveOperationException {

    /** {@code "java.lang.NoSuchMethodException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'N', 'o', 'S', 'u', 'c', 'h', 'M', 'e',
        't', 'h', 'o', 'd', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public NoSuchMethodException() {
        super();
    }

    /** With {@code message}. */
    public NoSuchMethodException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
