package java.lang;

/** An {@link Exception} a method need not declare: the unchecked half of the hierarchy. */
public class RuntimeException extends Exception {

    /** {@code "java.lang.RuntimeException"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'R', 'u', 'n', 't', 'i', 'm', 'e', 'E',
        'x', 'c', 'e', 'p', 't', 'i', 'o', 'n'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public RuntimeException() {
        super();
    }

    /** With {@code message}. */
    public RuntimeException(String message) {
        super(message);
    }

    /** With {@code message}, raised in response to {@code cause}. */
    public RuntimeException(String message, Throwable cause) {
        super(message, cause);
    }

    /** Raised in response to {@code cause}. */
    public RuntimeException(Throwable cause) {
        super(cause);
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
