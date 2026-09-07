package java.lang;

/**
 * The root of everything a {@code throw} may name.
 *
 * <p>On this target a {@code throw} is a wasm exception whose tag carries the thrown object, and a
 * {@code catch} narrows it with a {@code ref.cast} — so the hierarchy below is what a
 * {@code catch} clause selects on, exactly as the class hierarchy is on a JVM.
 *
 * <h2>No stack trace, and a type name that is a method</h2>
 *
 * <p>There is no {@code fillInStackTrace} here and no {@code getStackTrace}: a wasm frame carries
 * no Java identity, and a method that answered with an empty array would be a claim that the trace
 * was empty rather than that none was taken.
 *
 * <p>{@code toString} is the JDK's — the class's name, then the message — but the name cannot come
 * from {@code getClass()}: {@code java.lang.Object} is not a type this package declares (it is the
 * backend's {@code anyref}), so there is no {@code getClass} to call. {@link #typeName} is that
 * name as an overridable method instead, and every class below overrides it. A subclass that
 * forgets to reports its superclass's name, which is a wrong string rather than a wrong control
 * flow — the reason it is worth having at all is that {@code println(e)} is the one place an
 * exception is read.
 */
public class Throwable {

    /** {@code "java.lang.Throwable"}, the only way this target can spell a constant string. */
    private static final char[] TYPE_CHARS = {
        'j', 'a', 'v', 'a', '.', 'l', 'a', 'n', 'g', '.', 'T', 'h', 'r', 'o', 'w', 'a', 'b', 'l',
        'e'
    };

    /** {@link #TYPE_CHARS}, built once. */
    private static final String TYPE_NAME = new String(TYPE_CHARS);

    /** What {@link #toString} puts between the type name and the message. */
    private static final char[] SEPARATOR_CHARS = {':', ' '};

    /** {@link #SEPARATOR_CHARS}, built once. */
    private static final String SEPARATOR = new String(SEPARATOR_CHARS);

    /** The array {@link #getSuppressed} answers with until something is suppressed. */
    private static final Throwable[] NONE = new Throwable[0];

    /** What the thrower said, or null. */
    private final String message;

    /** What this one was raised in response to, or null. */
    private final Throwable cause;

    /** What a {@code try}-with-resources dropped on the way out of a failing body. */
    private Throwable[] suppressed;

    /** With no message and no cause. */
    public Throwable() {
        this.message = null;
        this.cause = null;
        this.suppressed = NONE;
    }

    /** With {@code message}. */
    public Throwable(String message) {
        this.message = message;
        this.cause = null;
        this.suppressed = NONE;
    }

    /** With {@code message}, raised in response to {@code cause}. */
    public Throwable(String message, Throwable cause) {
        this.message = message;
        this.cause = cause;
        this.suppressed = NONE;
    }

    /** Raised in response to {@code cause}, taking its rendering as the message. */
    public Throwable(Throwable cause) {
        this.message = messageOf(cause);
        this.cause = cause;
        this.suppressed = NONE;
    }

    /** What the thrower said, or null. */
    public String getMessage() {
        return this.message;
    }

    /**
     * {@link #getMessage}.
     *
     * <p>The JDK's hook for a localised rendering, and this package has no locale to render one
     * against — so it answers the same thing rather than not existing, which is what keeps a call
     * written against the JDK compiling here.
     */
    public String getLocalizedMessage() {
        return getMessage();
    }

    /** What this one was raised in response to, or null. */
    public Throwable getCause() {
        return this.cause;
    }

    /** Record that {@code exception} was dropped while this one was propagating. */
    public void addSuppressed(Throwable exception) {
        if (exception == null || exception == this) {
            return;
        }
        Throwable[] grown = new Throwable[this.suppressed.length + 1];
        int at = 0;
        while (at < this.suppressed.length) {
            grown[at] = this.suppressed[at];
            at = at + 1;
        }
        grown[at] = exception;
        this.suppressed = grown;
    }

    /** Everything {@link #addSuppressed} recorded, in order. */
    public Throwable[] getSuppressed() {
        Throwable[] copy = new Throwable[this.suppressed.length];
        int at = 0;
        while (at < this.suppressed.length) {
            copy[at] = this.suppressed[at];
            at = at + 1;
        }
        return copy;
    }

    /** The type name, then {@code ": "} and the message when there is one. */
    @Override
    public String toString() {
        String name = typeName();
        if (this.message == null) {
            return name;
        }
        return name.concat(SEPARATOR).concat(this.message);
    }

    /** {@code cause}'s rendering, or null when there is no cause. */
    private static String messageOf(Throwable cause) {
        if (cause == null) {
            return null;
        }
        return cause.toString();
    }

    /**
     * The fully-qualified name of this class — what {@code getClass().getName()} would answer on a
     * JVM.
     *
     * <p>Overridden by every class in the hierarchy. It is not {@code public} because it is not
     * part of {@code java.lang.Throwable}'s published surface: a project reads it through
     * {@link #toString}, and a project that wanted to add a class of its own to this hierarchy is
     * in this package's own {@code java.lang} or is not.
     */
    protected String typeName() {
        return TYPE_NAME;
    }
}
