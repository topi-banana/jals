package java.lang;

/**
 * The implicit supertype of every {@code enum} declaration.
 *
 * <p>Signature tier, and it has to be: an {@code enum}'s constants, its {@code values()} and its
 * {@code ordinal()} are *synthesised* by the compiler for each declaration, and there is no single
 * body this class could carry that would produce them. What it carries instead is the member set
 * every constant inherits, so a call to {@code ordinal()} or {@code name()} resolves and infers.
 *
 * <p>The edge to it is added by the index rather than written by the source, exactly as
 * {@link Object}'s is.
 */
public abstract class Enum<E extends Enum<E>> implements Comparable<E> {

    /** This constant's name, exactly as the declaration spells it. */
    public final String name();

    /** This constant's position in its declaration, counting from zero. */
    public final int ordinal();

    /** {@link #name()}. */
    public String toString();

    /** Constants of one enum compare by {@link #ordinal()}. */
    public final int compareTo(E other);
}
