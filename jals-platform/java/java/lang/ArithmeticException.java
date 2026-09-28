package java.lang;

/**
 * An arithmetic operation had no result of its type: integer division by zero, or the one
 * remainder whose sign no dividend can carry.
 *
 * <p>Declared so user code can throw and catch it by name; the operations themselves still trap
 * where the JVM would raise one, because a trap is what a wasm host gives a lowering that has not
 * been taught to construct the exception at the failure site.
 */
public class ArithmeticException extends RuntimeException {

    public ArithmeticException() {
        super();
    }

    public ArithmeticException(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.ArithmeticException";
    }
}
