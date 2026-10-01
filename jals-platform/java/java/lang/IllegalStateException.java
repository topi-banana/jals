package java.lang;

/**
 * A method was called when the object was in no state to answer: the class of the caller's
 * sequencing mistake.
 */
public class IllegalStateException extends RuntimeException {

    public IllegalStateException() {
        super();
    }

    public IllegalStateException(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.IllegalStateException";
    }
}
