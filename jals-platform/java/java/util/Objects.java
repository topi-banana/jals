package java.util;

/** Null-safe operations on references. */
public final class Objects {

    /** Whether {@code a} and {@code b} are equal, either or both being {@code null}. */
    public static boolean equals(Object a, Object b);

    /** {@code o}'s hash, or {@code 0} when it is {@code null}. */
    public static int hashCode(Object o);

    /** {@code o}'s rendering, or {@code "null"}. */
    public static String toString(Object o);
}
