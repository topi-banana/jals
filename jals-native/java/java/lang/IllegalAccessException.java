package java.lang;

/** A reflective access a declaration's visibility does not allow. */
public class IllegalAccessException extends ReflectiveOperationException {

    /** {@code "java.lang.IllegalAccessException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'I', 'l', 'l', 'e', 'g', 'a', 'l', 'A',
        'c', 'c', 'e', 's', 's', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public IllegalAccessException() {
        super();
    }

    /** With {@code message}. */
    public IllegalAccessException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
