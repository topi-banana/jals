package java.lang;

/**
 * An unchecked exception: what a `throw` in ordinary code carries, and the superclass of every
 * failure the language reports on its own.
 */
public class RuntimeException extends Exception {

    public RuntimeException() {
        super();
    }

    public RuntimeException(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.RuntimeException";
    }
}
