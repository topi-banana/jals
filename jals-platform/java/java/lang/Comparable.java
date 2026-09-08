package java.lang;

/**
 * A total order on this type.
 *
 * <p>Every wrapper below implements it, and saying so is not decoration: an omitted interface is
 * not a negative answer, and overload selection reads the relation as though it were. With the edge
 * missing, {@code f(Comparable)} against {@code f(Object)} for an {@code Integer} argument drops
 * the correct candidate as inapplicable and the call silently picks the other.
 */
public interface Comparable<T> {

    /** Negative, zero or positive as this orders before, with, or after {@code other}. */
    int compareTo(T other);
}
