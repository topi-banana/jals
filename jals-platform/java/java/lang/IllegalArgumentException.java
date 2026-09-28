package java.lang;

/**
 * An argument was illegal: the class of the caller's mistake, as {@code NullPointerException} is
 * the class of the runtime's.
 */
public class IllegalArgumentException extends RuntimeException {

    public IllegalArgumentException() {
        super();
    }

    public IllegalArgumentException(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.IllegalArgumentException";
    }
}
