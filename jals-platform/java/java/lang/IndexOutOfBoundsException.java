package java.lang;

/**
 * An index was outside the range a sequence allows.
 *
 * <p>The superclass of the two classes below: a handler for it catches both an array's failure
 * and a string's, which is the relation the classes exist to express.
 */
public class IndexOutOfBoundsException extends RuntimeException {

    public IndexOutOfBoundsException() {
        super();
    }

    public IndexOutOfBoundsException(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.IndexOutOfBoundsException";
    }
}
