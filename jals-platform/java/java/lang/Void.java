package java.lang;

/** The uninstantiable placeholder for {@code void}. */
public final class Void {

    /** {@code "void"}, as a {@code char[]}. */
    private static final char[] PRIMITIVE_NAME = {'v', 'o', 'i', 'd'};

    /** The {@code Class} standing for the primitive type {@code void}. */
    public static final Class<Void> TYPE = new Class<Void>(new String(PRIMITIVE_NAME));

    private Void() {}
}
