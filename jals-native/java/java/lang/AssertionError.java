package java.lang;

/**
 * What a failed {@code assert} raises.
 *
 * <p>Reachable only when the compile turned assertions on — this target has no {@code -ea} to read
 * at start-up, so {@code jals test} compiles them in and {@code jals build} compiles them out.
 */
public class AssertionError extends Error {

    /** {@code "java.lang.AssertionError"}, the name {@link Throwable#toString} reports. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'A', 's', 's', 'e', 'r', 't', 'i', 'o',
        'n', 'E', 'r', 'r', 'o', 'r'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** With no message. */
    public AssertionError() {
        super();
    }

    /**
     * With {@code detail} rendered as the message.
     *
     * <p>The rendering goes through {@link String#valueOf(Object)}, which answers for the
     * references this package can render and refuses the rest. A refusal here would replace the
     * assertion failure with an unrelated one, so anything it cannot render becomes no message at
     * all.
     */
    public AssertionError(Object detail) {
        super(messageOf(detail));
    }

    /** {@link String#valueOf(Object)}, with its refusal turned into "no message". */
    private static String messageOf(Object detail) {
        if (detail instanceof String) {
            return (String) detail;
        }
        return null;
    }

    @Override
    protected String typeName() {
        return TYPE_NAME;
    }
}
