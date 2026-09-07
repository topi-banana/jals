package java.lang;

/** A reflective instantiation of a type that has no instances. */
public class InstantiationException extends ReflectiveOperationException {

    /** {@code "java.lang.InstantiationException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'I', 'n', 's', 't', 'a', 'n', 't', 'i',
        'a', 't', 'i', 'o', 'n', 'E', 'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public InstantiationException() {
        super();
    }

    /** With {@code message}. */
    public InstantiationException(String message) {
        super(message);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
