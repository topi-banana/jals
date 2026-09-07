package java.lang;

/**
 * A total order a type declares over its own values.
 *
 * <p>Generic here where {@code jals-hir}'s stub is raw: a stub exists so a reference to a JDK type
 * resolves, and {@code compareTo(Object)} is enough for that. This declaration is compiled, so it
 * is the one a call site is checked against — and {@code T} is what keeps
 * {@code Integer.valueOf(1).compareTo(Long.valueOf(2))} from being a call that type-checks.
 *
 * @param <T> the type this one compares against
 */
public interface Comparable<T> {

    /**
     * A negative number, zero, or a positive number as this value sorts before, with, or after
     * {@code other}.
     */
    int compareTo(T other);
}
