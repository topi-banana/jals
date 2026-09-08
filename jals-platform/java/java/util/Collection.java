package java.util;

/** The root of the collection hierarchy. */
public interface Collection<E> extends Iterable<E> {

    /** How many elements this holds. */
    int size();

    /** Whether this holds no elements. */
    boolean isEmpty();

    /** Add {@code e}; whether the collection changed. */
    boolean add(E e);

    /** Remove one occurrence of {@code o}; whether the collection changed. */
    boolean remove(Object o);

    /** Whether this holds an element equal to {@code o}. */
    boolean contains(Object o);

    /** A cursor over this collection's elements. */
    Iterator<E> iterator();
}
