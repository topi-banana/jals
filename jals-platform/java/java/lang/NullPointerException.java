package java.lang;

/**
 * A reference that had to be non-null was null.
 *
 * <p>Declared so user code can throw and catch it by name. The failures that would raise one on
 * their own — `throw null`, a field read through a null receiver — are still traps: the lowering
 * has no constructor to call at those sites yet, and a trap is refused rather than mis-reported.
 */
public class NullPointerException extends RuntimeException {

    public NullPointerException() {
        super();
    }

    public NullPointerException(String message) {
        super(message);
    }

    protected String className() {
        return "java.lang.NullPointerException";
    }
}
