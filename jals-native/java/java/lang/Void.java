package java.lang;

/**
 * A type with no instances, held only so {@code void} has a {@link Class} like every other
 * primitive.
 *
 * <p>The constructor is private and nothing calls it, which is the JDK's shape and the whole
 * declaration: {@code Void} exists to be named, never to be built.
 */
public final class Void {

    /** {@code "void"}, the only way this target can spell a constant string. */
    private static final char[] TYPE_CHARS = {'v', 'o', 'i', 'd'};

    /** The identity of the pseudo-type this class stands for. */
    public static final Class TYPE = new Class(new String(TYPE_CHARS));

    /** Never called. */
    private Void() {
    }
}
