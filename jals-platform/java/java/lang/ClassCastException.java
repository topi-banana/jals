package java.lang;

/**
 * A cast or an `instanceof`-narrowed binding met a value of the wrong class.
 *
 * <p>Declared so user code can throw and catch it by name. A failed cast in this backend is a
 * trap — `ref.cast` is the same instruction whether the program would have caught the failure or
 * not — so the exception is not raised on its own yet.
 */
public class ClassCastException extends RuntimeException {

    public ClassCastException() {
        super();
    }

    public ClassCastException(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.ClassCastException";
    }
}
