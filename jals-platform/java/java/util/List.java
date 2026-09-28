package java.util;

/**
 * A sequence with an index: `get` and `set` name positions, and `add` at one moves the tail.
 *
 * <p>{@code indexOf} and {@code remove(Object)} are absent for the reason {@link Collection}'s
 * `contains` is — both compare elements, and element comparison across a link is not built yet.
 * `remove(int)` has no such problem and is simply not written yet: nothing needs it, and a method
 * that exists is a method that has to keep working.
 */
public interface List<E> extends Collection<E> {

    E get(int index);

    E set(int index, E element);

    void add(int index, E element);
}
