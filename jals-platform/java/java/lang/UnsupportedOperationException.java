package java.lang;

/**
 * The operation is not supported by this object's implementation — a collection that does not
 * implement an optional operation, most of the JDK's uses.
 */
public class UnsupportedOperationException extends RuntimeException {

    public UnsupportedOperationException() {
        super();
    }

    public UnsupportedOperationException(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.UnsupportedOperationException";
    }
}
