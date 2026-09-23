package java.util;

/** An ordered collection, addressable by index. */
public interface List<E> extends Collection<E> {

    /** The element at {@code index}. */
    E get(int index);

    /** Replace the element at {@code index}; the element that was there. */
    E set(int index, E element);

    /** Insert {@code element} at {@code index}. */
    void add(int index, E element);

    /** Remove the element at {@code index}; the element that was there. */
    E remove(int index);

    /** The index of the first element equal to {@code o}, or {@code -1}. */
    int indexOf(Object o);
}
