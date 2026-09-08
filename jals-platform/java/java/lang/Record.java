package java.lang;

/**
 * The implicit supertype of every {@code record} declaration.
 *
 * <p>Signature tier, for the reason {@link Enum} is: a record's accessors, its {@code equals}, its
 * {@code hashCode} and its {@code toString} are synthesised per declaration from the components,
 * and no single body here would produce them. What it carries is the member set every record
 * inherits.
 */
public abstract class Record {

    /** Whether {@code other} is a record of the same type with equal components. */
    public abstract boolean equals(Object other);

    /** A hash derived from every component. */
    public abstract int hashCode();

    /** The type name and every component, as the language specifies. */
    public abstract String toString();
}
