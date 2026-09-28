package java.lang;

/**
 * A string index was outside the range the string allows.
 *
 * <p>Declared for the same reason the array's sibling is, and trapped for the same reason: the
 * string methods that check a range have no exception to construct at the check yet.
 */
public class StringIndexOutOfBoundsException extends IndexOutOfBoundsException {

    public StringIndexOutOfBoundsException() {
        super();
    }

    public StringIndexOutOfBoundsException(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.StringIndexOutOfBoundsException";
    }
}
