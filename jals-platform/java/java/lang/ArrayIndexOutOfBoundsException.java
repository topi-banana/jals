package java.lang;

/**
 * An array index was outside the range the array allows.
 *
 * <p>Declared so user code can throw and catch it by name. An out-of-range access still traps:
 * the lowering's bounds check branches to `unreachable` rather than to a construction of this
 * class, which is the part of the failure model that has not been built yet.
 */
public class ArrayIndexOutOfBoundsException extends IndexOutOfBoundsException {

    public ArrayIndexOutOfBoundsException() {
        super();
    }

    public ArrayIndexOutOfBoundsException(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.ArrayIndexOutOfBoundsException";
    }
}
