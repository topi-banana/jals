package java.lang;

/**
 * The root of the exception hierarchy.
 *
 * <p>Two things a JVM {@code Throwable} has are absent here, each because of what the target is
 * rather than as an oversight. There is no stack trace: a wasm module has no walkable frame list
 * and no {@code fillInStackTrace} to call. And there is no {@code getClass().getName()} — {@link
 * Object} is not a type this package declares, so there is nothing to ask — which is why the
 * rendering goes through {@link #typeName()}, a method every subclass overrides with its own name.
 *
 * <p>Every constant here is a {@code char[]} wrapped once in a {@code static} field. That is not a
 * style: the backend refuses a string literal, so it is the only way this target spells one.
 */
public class Throwable {

    /** {@code "java.lang.Throwable"}. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'T', 'h', 'r', 'o', 'w', 'a', 'b', 'l',
        'e'
    };

    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** {@code ": "}, between a type name and a message. */
    private static final char[] SEPARATOR_CHARS = {':', ' '};

    private static final String SEPARATOR = new String(SEPARATOR_CHARS);

    private static final Throwable[] NONE = new Throwable[0];

    private final String message;

    private final Throwable cause;

    private Throwable[] suppressed;

    private int suppressedCount;

    public Throwable() {
        this(null, null);
    }

    public Throwable(String message) {
        this(message, null);
    }

    public Throwable(String message, Throwable cause) {
        this.message = message;
        this.cause = cause;
        this.suppressed = NONE;
        this.suppressedCount = 0;
    }

    public Throwable(Throwable cause) {
        this(cause == null ? null : cause.toString(), cause);
    }

    /** The detail message, or {@code null}. */
    public String getMessage() {
        return this.message;
    }

    /** The detail message, or {@code null} — the same answer as {@link #getMessage}. */
    public String getLocalizedMessage() {
        return this.message;
    }

    /** What caused this, or {@code null}. */
    public Throwable getCause() {
        return this.cause;
    }

    /** Record {@code exception} as suppressed by this one, as try-with-resources does. */
    public final void addSuppressed(Throwable exception) {
        if (exception == null || exception == this) {
            return;
        }
        if (this.suppressedCount == this.suppressed.length) {
            int grown = this.suppressedCount == 0 ? 2 : this.suppressedCount * 2;
            Throwable[] larger = new Throwable[grown];
            for (int i = 0; i < this.suppressedCount; i++) {
                larger[i] = this.suppressed[i];
            }
            this.suppressed = larger;
        }
        this.suppressed[this.suppressedCount] = exception;
        this.suppressedCount++;
    }

    /** Everything {@link #addSuppressed} recorded, in order. */
    public final Throwable[] getSuppressed() {
        Throwable[] out = new Throwable[this.suppressedCount];
        for (int i = 0; i < this.suppressedCount; i++) {
            out[i] = this.suppressed[i];
        }
        return out;
    }

    @Override
    public String toString() {
        String name = typeName();
        if (this.message == null) {
            return name;
        }
        return name.concat(SEPARATOR).concat(this.message);
    }

    /**
     * This type's fully-qualified name.
     *
     * <p>The stand-in for {@code getClass().getName()}, overridden by every subclass. It is the
     * one method in this package with two dozen overriders, which is what made the backend's
     * dispatch ordering matter.
     */
    protected String typeName() {
        return TYPE_NAME;
    }
}
