package java.util;

/**
 * The cursor an {@link java.lang.Iterable} hands out: `hasNext` says whether another element is
 * there, and `next` returns it.
 *
 * <p>Two methods and none of the JDK's others: no `remove`, no default `forEach`. A method this
 * file does not declare is a compile error at the line that needs it, which is the only answer a
 * missing method can give — a `remove` that silently did nothing, or a `forEach` that needed
 * consumer dispatch, would be worse than one that does not exist.
 */
public interface Iterator<E> {

    boolean hasNext();

    E next();
}
