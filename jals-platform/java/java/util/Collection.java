package java.util;

/**
 * A group of elements: what a container can say about itself, and the one way to add to it.
 *
 * <p>This is the standard library stub's surface minus the three methods that compare elements —
 * `contains`, `remove`, and {@link List}'s `indexOf`. Each would have to run `equals` on an element
 * that may be a *consumer's* object, and a library body dispatching a method on a consumer type is
 * the one call this link cannot make yet; leaving them out makes the call a compile error rather
 * than an element comparison that silently answered `==` — which for a string read off a heap the
 * caller does not control would be wrong exactly when it mattered.
 */
public interface Collection<E> extends Iterable<E> {

    int size();

    boolean isEmpty();

    boolean add(E element);

    void clear();
}
