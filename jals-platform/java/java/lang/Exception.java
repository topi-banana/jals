package java.lang;

/**
 * A checked exception: what a caller is expected to name in a `catch` or a `throws`.
 *
 * <p>Nothing in this platform throws one yet. It is declared because user code catches it — the
 * broadest handler that is not a {@link RuntimeException} — and a `catch (Exception e)` needs a
 * class to test the payload against.
 */
public class Exception extends Throwable {

    public Exception() {
        super();
    }

    public Exception(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.Exception";
    }
}
